package api

import (
	"context"
	"fmt"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// Рекламные сообщения (условия API Telegram): в каналах и чатах с ботами.
// Показ, просмотр и щелчок — как требует сервер; жалоба — двумя шагами
// (сначала варианты причины, потом выбранный).

func (s *Service) getSponsored(ctx context.Context, id uint64, c *pb.GetSponsored) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesGetSponsoredMessagesRequest{Peer: peer})
	if err != nil {
		return resultError(id, err)
	}
	out := &pb.SponsoredList{}
	if list, ok := v.(*tl.MessagesSponsoredMessages); ok {
		for _, m := range list.Messages {
			x, ok := m.(*tl.SponsoredMessage)
			if !ok {
				continue
			}
			out.Messages = append(out.Messages, &pb.Sponsored{RandomId: x.RandomId, Title: x.Title, Text: x.Message,
				Url: x.Url, Button: x.ButtonText, SponsorInfo: str(x.SponsorInfo), AdditionalInfo: str(x.AdditionalInfo),
				CanReport: x.CanReport, Recommended: x.Recommended})
		}
	}
	s.log.Printf("реклама: %d", len(out.Messages))
	return payloadAny(id, out)
}

func (s *Service) viewSponsored(ctx context.Context, id uint64, c *pb.ViewSponsored) *pb.Event {
	if _, err := s.m.Invoke(ctx, &tl.MessagesViewSponsoredMessageRequest{RandomId: c.GetRandomId()}); err != nil {
		return resultError(id, err)
	}
	return payload(id, nil)
}

func (s *Service) clickSponsored(ctx context.Context, id uint64, c *pb.ClickSponsored) *pb.Event {
	if _, err := s.m.Invoke(ctx, &tl.MessagesClickSponsoredMessageRequest{RandomId: c.GetRandomId()}); err != nil {
		return resultError(id, err)
	}
	return payload(id, nil)
}

func (s *Service) reportSponsored(ctx context.Context, id uint64, c *pb.ReportSponsored) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.MessagesReportSponsoredMessageRequest{RandomId: c.GetRandomId(),
		Option: c.GetOption()})
	if err != nil {
		return resultError(id, err)
	}
	out := &pb.SponsoredReport{}
	switch x := v.(type) {
	case *tl.ChannelsSponsoredMessageReportResultChooseOption:
		out.Kind = pb.SponsoredReport_CHOOSE
		out.Title = x.Title
		for _, o := range x.Options {
			if op, ok := o.(*tl.SponsoredMessageReportOption); ok {
				out.Options = append(out.Options, &pb.ReportOption{Text: op.Text, Option: op.Option})
			}
		}
	case *tl.ChannelsSponsoredMessageReportResultAdsHidden:
		out.Kind = pb.SponsoredReport_HIDDEN
	case *tl.ChannelsSponsoredMessageReportResultReported:
		out.Kind = pb.SponsoredReport_REPORTED
	default:
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	return payloadAny(id, out)
}

func str(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

// readContents — содержимое просмотрено (самоуничтожающиеся вложения).
func (s *Service) readContents(ctx context.Context, id uint64, c *pb.ReadContents) *pb.Event {
	var err error
	if ch, ok := conv.InputChannel(c.GetPeer()); ok {
		_, err = s.m.Invoke(ctx, &tl.ChannelsReadMessageContentsRequest{Channel: ch, Id: c.GetIds()})
	} else {
		_, err = s.m.Invoke(ctx, &tl.MessagesReadMessageContentsRequest{Id: c.GetIds()})
	}
	if err != nil {
		return resultError(id, err)
	}
	return payload(id, nil)
}
