package api

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// Веха В4: диалоги, история, отправка текста, «прочитано», маленькие аватары.

func payload(id uint64, p any) *pb.Event {
	r := &pb.CommandResult{CmdId: id}
	switch v := p.(type) {
	case *pb.Dialogs:
		r.Payload = &pb.CommandResult_Dialogs{Dialogs: v}
	case *pb.History:
		r.Payload = &pb.CommandResult_History{History: v}
	case *pb.Sent:
		r.Payload = &pb.CommandResult_Sent{Sent: v}
	case *pb.FilePath:
		r.Payload = &pb.CommandResult_File{File: v}
	}
	return &pb.Event{Body: &pb.Event_Result{Result: r}}
}

func (s *Service) loadDialogs(ctx context.Context, id uint64, c *pb.LoadDialogs) *pb.Event {
	limit := c.GetLimit()
	if limit <= 0 || limit > 100 {
		limit = 100
	}
	var offsetPeer tl.InputPeerClass = &tl.InputPeerEmpty{}
	if c.GetOffsetPeer().GetId() != 0 {
		p, err := conv.InputPeer(c.GetOffsetPeer())
		if err != nil {
			return resultError(id, err)
		}
		offsetPeer = p
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesGetDialogsRequest{OffsetDate: int32(c.GetOffsetDate()),
		OffsetId: c.GetOffsetId(), OffsetPeer: offsetPeer, Limit: limit})
	if err != nil {
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
	out := &pb.Dialogs{}
	var dialogs []tl.DialogClass
	switch d := v.(type) {
	case *tl.MessagesDialogs:
		dialogs, out.Complete = d.Dialogs, true
		out.Messages, out.Peers = conv.Messages(d.Messages), conv.Peers(d.Users, d.Chats)
		out.Total = int32(len(d.Dialogs))
	case *tl.MessagesDialogsSlice:
		dialogs = d.Dialogs
		out.Messages, out.Peers = conv.Messages(d.Messages), conv.Peers(d.Users, d.Chats)
		out.Total = d.Count
		out.Complete = len(d.Dialogs) < int(limit)
	case *tl.MessagesDialogsNotModified:
		out.Complete, out.Total = true, d.Count
	default:
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	for _, d := range dialogs {
		if x := conv.Dialog(d); x != nil {
			out.Dialogs = append(out.Dialogs, x)
		}
	}
	s.log.Printf("диалоги: %d (всего %d)", len(out.Dialogs), out.Total)
	return payload(id, out)
}

func (s *Service) loadHistory(ctx context.Context, id uint64, c *pb.LoadHistory) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	limit := c.GetLimit()
	if limit <= 0 || limit > 100 {
		limit = 50
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesGetHistoryRequest{Peer: peer, OffsetId: c.GetOffsetId(),
		AddOffset: c.GetAddOffset(), Limit: limit})
	if err != nil {
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
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
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	return payload(id, out)
}

// sendMessage — текст с random_id (О-9: повтор с тем же random_id не создаёт дубль).
func (s *Service) sendMessage(ctx context.Context, id uint64, c *pb.SendMessage) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	req := &tl.MessagesSendMessageRequest{Peer: peer, Message: c.GetText(), RandomId: c.GetRandomId()}
	if c.GetReplyTo() != 0 {
		r := c.GetReplyTo()
		req.ReplyTo = &tl.InputReplyToMessage{ReplyToMsgId: r}
	}
	v, err := s.m.Invoke(ctx, req)
	if err != nil {
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
	u, ok := v.(tl.UpdatesClass)
	if !ok {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	sent := &pb.Message{PeerId: c.GetPeer().GetId(), Text: c.GetText(), RandomId: c.GetRandomId(),
		FromId: s.auth.UserID(), Out: true, ReplyTo: c.GetReplyTo()}
	s.upd.Process(ctx, u, sent)
	if sent.Id == 0 {
		sent.Id = s.upd.SentID(c.GetRandomId())
	}
	s.log.Printf("отправлено сообщение id %d", sent.Id)
	return payload(id, &pb.Sent{Message: sent})
}

func (s *Service) markRead(ctx context.Context, id uint64, c *pb.MarkRead) *pb.Event {
	var err error
	if ch, ok := conv.InputChannel(c.GetPeer()); ok {
		_, err = s.m.Invoke(ctx, &tl.ChannelsReadHistoryRequest{Channel: ch, MaxId: c.GetMaxId()})
	} else {
		peer, perr := conv.InputPeer(c.GetPeer())
		if perr != nil {
			return resultError(id, perr)
		}
		var v any
		v, err = s.m.Invoke(ctx, &tl.MessagesReadHistoryRequest{Peer: peer, MaxId: c.GetMaxId()})
		if aff, ok := v.(*tl.MessagesAffectedMessages); ok {
			// affectedMessages несёт pts: пропустить его — значит получить ложный разрыв.
			s.upd.Process(ctx, &tl.UpdateShort{Update: &tl.UpdateReadHistoryInbox{Peer: peerOf(c.GetPeer()),
				MaxId: c.GetMaxId(), Pts: aff.Pts, PtsCount: aff.PtsCount}}, nil)
		}
	}
	if err != nil {
		return resultError(id, err)
	}
	return payload(id, nil)
}

func peerOf(r *pb.PeerRef) tl.PeerClass {
	switch conv.Kind(r.GetId()) {
	case pb.Peer_USER:
		return &tl.PeerUser{UserId: r.GetId()}
	case pb.Peer_CHANNEL:
		return &tl.PeerChannel{ChannelId: -r.GetId() - conv.ChannelShift}
	}
	return &tl.PeerChat{ChatId: -r.GetId()}
}

// downloadAvatar — маленькое фото собеседника в кэш (Go пишет, Rust читает).
func (s *Service) downloadAvatar(ctx context.Context, id uint64, c *pb.DownloadAvatar) *pb.Event {
	if c.GetPhotoId() == 0 {
		return resultErrorKind(id, pb.Error_INTERNAL, "нет фото")
	}
	dir := filepath.Join(s.req.GetCacheDir(), "avatars")
	path := filepath.Join(dir, strconv.FormatInt(c.GetPeer().GetId(), 10)+"_"+strconv.FormatInt(c.GetPhotoId(), 10)+".jpg")
	if st, err := os.Stat(path); err == nil && st.Size() > 0 {
		return payload(id, &pb.FilePath{Path: path})
	}
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	v, err := s.m.InvokeOn(ctx, int(c.GetDc()), &tl.UploadGetFileRequest{
		Location: &tl.InputPeerPhotoFileLocation{Peer: peer, PhotoId: c.GetPhotoId()}, Offset: 0, Limit: 1 << 20})
	if err != nil {
		return resultError(id, err)
	}
	f, ok := v.(*tl.UploadFile)
	if !ok || len(f.Bytes) == 0 {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return resultError(id, err)
	}
	tmp := path + ".part"
	if err := os.WriteFile(tmp, f.Bytes, 0o600); err != nil {
		return resultError(id, err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return resultError(id, err)
	}
	return payload(id, &pb.FilePath{Path: path})
}
