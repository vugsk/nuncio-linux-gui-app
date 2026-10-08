package api

import (
	"context"
	"fmt"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// Стикеры: панель в поле ввода (установленные наборы, недавние) и отправка.

func (s *Service) getStickerSets(ctx context.Context, id uint64) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.MessagesGetAllStickersRequest{})
	if err != nil {
		return resultError(id, err)
	}
	out := &pb.StickerSets{}
	if all, ok := v.(*tl.MessagesAllStickers); ok {
		for _, x := range all.Sets {
			if set, ok := x.(*tl.StickerSet); ok && !set.Archived && !set.Masks && !set.Emojis {
				out.Sets = append(out.Sets, &pb.StickerSetInfo{Id: set.Id, AccessHash: set.AccessHash, Title: set.Title,
					Count: set.Count})
			}
		}
	}
	s.log.Printf("наборы стикеров: %d", len(out.Sets))
	return payloadAny(id, out)
}

// getStickerSet — набор целиком; id 0 — недавние.
func (s *Service) getStickerSet(ctx context.Context, id uint64, c *pb.GetStickerSet) *pb.Event {
	var docs []tl.DocumentClass
	if c.GetId() == 0 {
		v, err := s.m.Invoke(ctx, &tl.MessagesGetRecentStickersRequest{})
		if err != nil {
			return resultError(id, err)
		}
		if r, ok := v.(*tl.MessagesRecentStickers); ok {
			docs = r.Stickers
		}
	} else {
		v, err := s.m.Invoke(ctx, &tl.MessagesGetStickerSetRequest{
			Stickerset: &tl.InputStickerSetID{Id: c.GetId(), AccessHash: c.GetAccessHash()}})
		if err != nil {
			return resultError(id, err)
		}
		if r, ok := v.(*tl.MessagesStickerSet); ok {
			docs = r.Documents
		}
	}
	out := &pb.StickerList{SetId: c.GetId()}
	for _, d := range docs {
		if m := conv.MediaInfo(&tl.MessageMediaDocument{Document: d}); m != nil {
			out.Stickers = append(out.Stickers, m)
		}
	}
	return payloadAny(id, out)
}

func (s *Service) sendSticker(ctx context.Context, id uint64, c *pb.SendSticker) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	f := c.GetFile()
	var replyTo tl.InputReplyToClass
	if r := c.GetReplyTo(); r != 0 {
		replyTo = &tl.InputReplyToMessage{ReplyToMsgId: r}
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesSendMediaRequest{Peer: peer, ReplyTo: replyTo, RandomId: c.GetRandomId(),
		Media: &tl.InputMediaDocument{Id: &tl.InputDocument{Id: f.GetId(), AccessHash: f.GetAccessHash(),
			FileReference: f.GetFileReference()}}})
	if err != nil {
		return resultError(id, err)
	}
	u, ok := v.(tl.UpdatesClass)
	if !ok {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	s.upd.Process(ctx, u, nil)
	return payload(id, nil)
}
