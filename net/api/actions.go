package api

import (
	"context"
	"fmt"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// Веха В5: правка, удаление, пересылка, закрепление, закреплённые, контакты,
// поиск людей. Ответы с Updates и affectedMessages идут через поток
// обновлений: их pts нельзя пропускать, иначе будет ложный разрыв.

func (s *Service) processUpdates(ctx context.Context, v any) {
	if u, ok := v.(tl.UpdatesClass); ok {
		s.upd.Process(ctx, u, nil)
	}
}

func (s *Service) editMessage(ctx context.Context, id uint64, c *pb.EditMessage) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	text := c.GetText()
	// Пустой (не nil) список сущностей — правка снимает форматирование.
	ents := conv.InputEntities(c.GetEntities())
	if ents == nil {
		ents = []tl.MessageEntityClass{}
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesEditMessageRequest{Peer: peer, Id: c.GetId(), Message: &text, Entities: ents})
	if err != nil {
		return resultError(id, err)
	}
	s.processUpdates(ctx, v)
	return payload(id, nil)
}

func (s *Service) deleteMessages(ctx context.Context, id uint64, c *pb.DeleteMessagesCmd) *pb.Event {
	if ch, ok := conv.InputChannel(c.GetPeer()); ok {
		v, err := s.m.Invoke(ctx, &tl.ChannelsDeleteMessagesRequest{Channel: ch, Id: c.GetIds()})
		if err != nil {
			return resultError(id, err)
		}
		if aff, ok := v.(*tl.MessagesAffectedMessages); ok {
			s.upd.Process(ctx, &tl.UpdateShort{Update: &tl.UpdateDeleteChannelMessages{
				ChannelId: -c.GetPeer().GetId() - conv.ChannelShift, Messages: c.GetIds(), Pts: aff.Pts,
				PtsCount: aff.PtsCount}}, nil)
		}
		return payload(id, nil)
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesDeleteMessagesRequest{Revoke: c.GetRevoke(), Id: c.GetIds()})
	if err != nil {
		return resultError(id, err)
	}
	if aff, ok := v.(*tl.MessagesAffectedMessages); ok {
		s.upd.Process(ctx, &tl.UpdateShort{Update: &tl.UpdateDeleteMessages{Messages: c.GetIds(), Pts: aff.Pts,
			PtsCount: aff.PtsCount}}, nil)
	}
	return payload(id, nil)
}

// forward — пересылка нескольким адресатам по очереди (Дз-16); ошибка у одного
// адресата не отменяет остальных, возвращается первая.
func (s *Service) forward(ctx context.Context, id uint64, c *pb.ForwardMessages) *pb.Event {
	from, err := conv.InputPeer(c.GetFromPeer())
	if err != nil {
		return resultError(id, err)
	}
	var first error
	for _, t := range c.GetTargets() {
		to, err := conv.InputPeer(t.GetPeer())
		if err == nil && len(t.GetRandomIds()) != len(c.GetIds()) {
			err = fmt.Errorf("пересылка: random_id не на каждое сообщение")
		}
		if err == nil {
			var v any
			v, err = s.m.Invoke(ctx, &tl.MessagesForwardMessagesRequest{FromPeer: from, Id: c.GetIds(),
				RandomId: t.GetRandomIds(), ToPeer: to})
			if err == nil {
				s.processUpdates(ctx, v)
			}
		}
		if err != nil && first == nil {
			first = err
		}
	}
	if first != nil {
		return resultError(id, first)
	}
	return payload(id, nil)
}

func (s *Service) pinMessage(ctx context.Context, id uint64, c *pb.PinMessage) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesUpdatePinnedMessageRequest{Silent: c.GetSilent(), Unpin: c.GetUnpin(),
		Peer: peer, Id: c.GetId()})
	if err != nil {
		return resultError(id, err)
	}
	s.processUpdates(ctx, v)
	return payload(id, nil)
}

func historyOf(v any) (*pb.History, error) {
	out := &pb.History{}
	switch h := v.(type) {
	case *tl.MessagesMessages:
		out.Messages, out.Peers, out.Total = conv.Messages(h.Messages), conv.Peers(h.Users, h.Chats), int32(len(h.Messages))
	case *tl.MessagesMessagesSlice:
		out.Messages, out.Peers, out.Total = conv.Messages(h.Messages), conv.Peers(h.Users, h.Chats), h.Count
	case *tl.MessagesChannelMessages:
		out.Messages, out.Peers, out.Total = conv.Messages(h.Messages), conv.Peers(h.Users, h.Chats), h.Count
	case *tl.MessagesMessagesNotModified:
		out.Total = h.Count
	default:
		return nil, fmt.Errorf("неожиданный ответ %T", v)
	}
	return out, nil
}

func (s *Service) loadPinned(ctx context.Context, id uint64, c *pb.LoadPinned) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	limit := c.GetLimit()
	if limit <= 0 || limit > 100 {
		limit = 20
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesSearchRequest{Peer: peer, Filter: &tl.InputMessagesFilterPinned{},
		Limit: limit})
	if err != nil {
		return resultError(id, err)
	}
	h, err := historyOf(v)
	if err != nil {
		return resultErrorKind(id, pb.Error_INTERNAL, err.Error())
	}
	for _, m := range h.Messages {
		m.Pinned = true
	}
	s.upd.RememberPeers(h.Peers)
	return payload(id, h)
}

func (s *Service) loadMessages(ctx context.Context, id uint64, c *pb.LoadMessages) *pb.Event {
	ids := make([]tl.InputMessageClass, 0, len(c.GetIds()))
	for _, x := range c.GetIds() {
		ids = append(ids, &tl.InputMessageID{Id: x})
	}
	var v any
	var err error
	if ch, ok := conv.InputChannel(c.GetPeer()); ok {
		v, err = s.m.Invoke(ctx, &tl.ChannelsGetMessagesRequest{Channel: ch, Id: ids})
	} else {
		v, err = s.m.Invoke(ctx, &tl.MessagesGetMessagesRequest{Id: ids})
	}
	if err != nil {
		return resultError(id, err)
	}
	h, err := historyOf(v)
	if err != nil {
		return resultErrorKind(id, pb.Error_INTERNAL, err.Error())
	}
	s.upd.RememberPeers(h.Peers)
	return payload(id, h)
}

func (s *Service) getContacts(ctx context.Context, id uint64) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.ContactsGetContactsRequest{Hash: 0})
	if err != nil {
		return resultError(id, err)
	}
	out := &pb.PeerList{}
	if c, ok := v.(*tl.ContactsContacts); ok {
		out.Peers = conv.Peers(c.Users, nil)
	}
	return payload(id, out)
}

func (s *Service) searchPeers(ctx context.Context, id uint64, c *pb.SearchPeers) *pb.Event {
	limit := c.GetLimit()
	if limit <= 0 || limit > 50 {
		limit = 20
	}
	v, err := s.m.Invoke(ctx, &tl.ContactsSearchRequest{Q: c.GetQuery(), Limit: limit})
	if err != nil {
		return resultError(id, err)
	}
	out := &pb.PeerList{}
	if f, ok := v.(*tl.ContactsFound); ok {
		out.Peers = conv.Peers(f.Users, f.Chats)
		s.upd.RememberPeers(out.Peers)
	}
	return payload(id, out)
}
