package updates

import (
	"context"
	"time"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// У каждого канала и супергруппы свой pts; его разрыв закрывается
// updates.getChannelDifference, состояние уходит Rust в той же пачке
// (ChannelState) и возвращается в StartRequest.

// SetChannels — pts каналов из базы Rust (при старте).
func (m *Manager) SetChannels(states []*pb.ChannelState) {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, s := range states {
		if s.GetPts() > 0 {
			m.chans[s.GetPeerId()] = s.GetPts()
		}
	}
}

// RememberPeers запоминает access_hash каналов (нужен для getChannelDifference).
func (m *Manager) RememberPeers(peers []*pb.Peer) {
	m.mu.Lock()
	defer m.mu.Unlock()
	for _, p := range peers {
		if p.GetKind() == pb.Peer_CHANNEL && !p.GetMin() && p.GetAccessHash() != 0 {
			m.hashes[p.GetId()] = p.GetAccessHash()
		}
	}
}

// NoteDialogs — pts каналов из списка диалогов (если ещё не известны) и их hash.
func (m *Manager) NoteDialogs(dialogs []*pb.Dialog, peers []*pb.Peer) []*pb.ChannelState {
	m.RememberPeers(peers)
	m.mu.Lock()
	defer m.mu.Unlock()
	var out []*pb.ChannelState
	for _, d := range dialogs {
		if conv.Kind(d.GetPeerId()) != pb.Peer_CHANNEL || d.GetPts() == 0 {
			continue
		}
		if _, known := m.chans[d.GetPeerId()]; !known {
			m.chans[d.GetPeerId()] = d.GetPts()
			out = append(out, &pb.ChannelState{PeerId: d.GetPeerId(), Pts: d.GetPts()})
		}
	}
	return out
}

// ChannelPts — текущий pts канала (0 — неизвестен).
func (m *Manager) ChannelPts(id int64) int32 {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.chans[id]
}

func (b *batch) dirty(ch int64) {
	if b.chanDirty == nil {
		b.chanDirty = map[int64]bool{}
	}
	b.chanDirty[ch] = true
	b.changed = true
}

// channelPts — проверка pts канала по правилу общего pts. Неизвестный канал
// принимает первое значение. check=false — из разницы (pts уже учтён).
func (m *Manager) channelPts(b *batch, ch int64, pts, count int32, check bool) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	local, known := m.chans[ch]
	switch {
	case !check:
		return true
	case !known || local == 0:
		m.chans[ch] = pts
		b.dirty(ch)
		return true
	case local+count == pts:
		m.chans[ch] = pts
		b.dirty(ch)
		return true
	case local+count > pts:
		return false
	default:
		for _, g := range b.chanGaps {
			if g == ch {
				return false
			}
		}
		b.chanGaps = append(b.chanGaps, ch)
		return false
	}
}

func (m *Manager) setChannelPts(b *batch, ch int64, pts int32) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if pts > m.chans[ch] {
		m.chans[ch] = pts
		b.dirty(ch)
	}
}

// channelDifference догоняет канал до конца (final).
func (m *Manager) channelDifference(ctx context.Context, ch int64) {
	m.mu.Lock()
	hash, ok := m.hashes[ch]
	pts := m.chans[ch]
	m.mu.Unlock()
	if !ok || pts == 0 {
		m.cfg.Log("updates: канал без access_hash или pts, разница пропущена")
		return
	}
	in, _ := conv.InputChannel(&pb.PeerRef{Id: ch, AccessHash: hash})
	for i := 0; i < 100 && ctx.Err() == nil; i++ {
		dctx, cancel := context.WithTimeout(ctx, 60*time.Second)
		v, err := m.cfg.Inv.Invoke(dctx, &tl.UpdatesGetChannelDifferenceRequest{Channel: in,
			Filter: &tl.ChannelMessagesFilterEmpty{}, Pts: m.ChannelPts(ch), Limit: 100})
		cancel()
		if err != nil {
			m.cfg.Log("updates: getChannelDifference: %T", err)
			return
		}
		b := &batch{}
		final := true
		switch d := v.(type) {
		case *tl.UpdatesChannelDifferenceEmpty:
			m.setChannelPts(b, ch, d.Pts)
			final = d.Final
		case *tl.UpdatesChannelDifference:
			m.diffContent(b, d.NewMessages, d.OtherUpdates, d.Users, d.Chats)
			m.mu.Lock()
			m.chans[ch] = d.Pts
			m.mu.Unlock()
			b.dirty(ch)
			final = d.Final
		case *tl.UpdatesChannelDifferenceTooLong:
			// История канала на клиенте устарела: Rust перечитывает её с сервера.
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ResetChannel{ResetChannel: &pb.ResetChannel{PeerId: ch}}})
			m.diffContent(b, d.Messages, nil, d.Users, d.Chats)
			if dl, ok := d.Dialog.(*tl.Dialog); ok && dl.Pts != nil {
				m.mu.Lock()
				m.chans[ch] = *dl.Pts
				m.mu.Unlock()
				b.dirty(ch)
			}
			final = d.Final
		}
		m.flush(b)
		if final {
			return
		}
	}
}
