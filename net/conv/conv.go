// Package conv — отображение TL (слой API) в доменные сообщения client.proto
// и обратно: в client.proto нет ничего из TL (Архитектура/nuncio - Границы и
// C ABI.md), поэтому смена слоя не трогает интерфейс.
//
// Идентификаторы собеседников «помеченные», как в Bot API: пользователь +id,
// группа -id, канал -(1000000000000 + id).
package conv

import (
	"fmt"
	"strings"

	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// ChannelShift — сдвиг помеченных id каналов.
const ChannelShift int64 = 1_000_000_000_000

// PeerID — помеченный id по TL Peer (0 — неизвестно).
func PeerID(p tl.PeerClass) int64 {
	switch v := p.(type) {
	case *tl.PeerUser:
		return v.UserId
	case *tl.PeerChat:
		return -v.ChatId
	case *tl.PeerChannel:
		return -(ChannelShift + v.ChannelId)
	}
	return 0
}

// UserID, ChatID, ChannelID — помеченные id по сырым.
func UserID(id int64) int64    { return id }
func ChatID(id int64) int64    { return -id }
func ChannelID(id int64) int64 { return -(ChannelShift + id) }

// Kind — вид собеседника по помеченному id.
func Kind(id int64) pb.Peer_Kind {
	switch {
	case id > 0:
		return pb.Peer_USER
	case id < -ChannelShift:
		return pb.Peer_CHANNEL
	default:
		return pb.Peer_CHAT
	}
}

// InputPeer — ссылка для запросов.
func InputPeer(r *pb.PeerRef) (tl.InputPeerClass, error) {
	if r == nil || r.GetId() == 0 {
		return nil, fmt.Errorf("conv: пустая ссылка на собеседника")
	}
	id := r.GetId()
	switch Kind(id) {
	case pb.Peer_USER:
		return &tl.InputPeerUser{UserId: id, AccessHash: r.GetAccessHash()}, nil
	case pb.Peer_CHANNEL:
		return &tl.InputPeerChannel{ChannelId: -id - ChannelShift, AccessHash: r.GetAccessHash()}, nil
	default:
		return &tl.InputPeerChat{ChatId: -id}, nil
	}
}

// InputChannel — для channels.*.
func InputChannel(r *pb.PeerRef) (tl.InputChannelClass, bool) {
	if r == nil || Kind(r.GetId()) != pb.Peer_CHANNEL {
		return nil, false
	}
	return &tl.InputChannel{ChannelId: -r.GetId() - ChannelShift, AccessHash: r.GetAccessHash()}, true
}

func str(p *string) string {
	if p == nil {
		return ""
	}
	return *p
}

// User — пользователь в Peer.
func User(u tl.UserClass) *pb.Peer {
	v, ok := u.(*tl.User)
	if !ok {
		if e, ok := u.(*tl.UserEmpty); ok {
			return &pb.Peer{Id: e.Id, Kind: pb.Peer_USER, Min: true}
		}
		return nil
	}
	p := &pb.Peer{Id: v.Id, Kind: pb.Peer_USER, Username: str(v.Username), IsSelf: v.Self, IsBot: v.Bot, Min: v.Min}
	p.Name = strings.TrimSpace(str(v.FirstName) + " " + str(v.LastName))
	if v.Deleted && p.Name == "" {
		p.Name = "\x00deleted" // интерфейс подставит «Удалённый аккаунт»
	}
	if v.AccessHash != nil {
		p.AccessHash = *v.AccessHash
	}
	if ph, ok := v.Photo.(*tl.UserProfilePhoto); ok {
		p.PhotoId, p.PhotoDc = ph.PhotoId, ph.DcId
	}
	return p
}

// Chat — группа или канал в Peer.
func Chat(c tl.ChatClass) *pb.Peer {
	switch v := c.(type) {
	case *tl.Chat:
		p := &pb.Peer{Id: ChatID(v.Id), Kind: pb.Peer_CHAT, Name: v.Title}
		if ph, ok := v.Photo.(*tl.ChatPhoto); ok {
			p.PhotoId, p.PhotoDc = ph.PhotoId, ph.DcId
		}
		return p
	case *tl.ChatForbidden:
		return &pb.Peer{Id: ChatID(v.Id), Kind: pb.Peer_CHAT, Name: v.Title}
	case *tl.Channel:
		p := &pb.Peer{Id: ChannelID(v.Id), Kind: pb.Peer_CHANNEL, Name: v.Title, Username: str(v.Username),
			Broadcast: v.Broadcast, Min: v.Min}
		if v.AccessHash != nil {
			p.AccessHash = *v.AccessHash
		}
		if ph, ok := v.Photo.(*tl.ChatPhoto); ok {
			p.PhotoId, p.PhotoDc = ph.PhotoId, ph.DcId
		}
		return p
	case *tl.ChannelForbidden:
		return &pb.Peer{Id: ChannelID(v.Id), Kind: pb.Peer_CHANNEL, Name: v.Title, AccessHash: v.AccessHash,
			Broadcast: v.Broadcast}
	}
	return nil
}

// Peers — все пользователи и чаты ответа.
func Peers(users []tl.UserClass, chats []tl.ChatClass) []*pb.Peer {
	out := make([]*pb.Peer, 0, len(users)+len(chats))
	for _, u := range users {
		if p := User(u); p != nil {
			out = append(out, p)
		}
	}
	for _, c := range chats {
		if p := Chat(c); p != nil {
			out = append(out, p)
		}
	}
	return out
}

// mediaKind — вид вложения (в В4 показывается подписью; сами вложения — В7).
func mediaKind(m tl.MessageMediaClass) string {
	switch v := m.(type) {
	case nil, *tl.MessageMediaEmpty, *tl.MessageMediaWebPage:
		return "" // превью ссылки — не отдельное вложение
	case *tl.MessageMediaPhoto:
		return "photo"
	case *tl.MessageMediaDocument:
		if d, ok := v.Document.(*tl.Document); ok {
			for _, a := range d.Attributes {
				switch at := a.(type) {
				case *tl.DocumentAttributeSticker:
					return "sticker"
				case *tl.DocumentAttributeAnimated:
					return "gif"
				case *tl.DocumentAttributeVideo:
					if at.RoundMessage {
						return "video_note"
					}
					return "video"
				case *tl.DocumentAttributeAudio:
					if at.Voice {
						return "voice"
					}
					return "audio"
				}
			}
		}
		return "document"
	case *tl.MessageMediaGeo, *tl.MessageMediaGeoLive, *tl.MessageMediaVenue:
		return "location"
	case *tl.MessageMediaContact:
		return "contact"
	case *tl.MessageMediaPoll:
		return "poll"
	case *tl.MessageMediaDice:
		return "dice"
	}
	return "other"
}

// serviceKind — вид служебного сообщения по имени конструктора действия.
func serviceKind(a tl.MessageActionClass) string {
	if a == nil {
		return "other"
	}
	return strings.TrimPrefix(a.TLName(), "messageAction")
}

func replyTo(r tl.MessageReplyHeaderClass) int32 {
	if h, ok := r.(*tl.MessageReplyHeader); ok && h.ReplyToMsgId != nil {
		return *h.ReplyToMsgId
	}
	return 0
}

// Message — сообщение в доменное (nil — пустое сообщение).
func Message(m tl.MessageClass) *pb.Message {
	switch v := m.(type) {
	case *tl.Message:
		out := &pb.Message{PeerId: PeerID(v.PeerId), Id: v.Id, Date: int64(v.Date), Out: v.Out, Text: v.Message,
			ReplyTo: replyTo(v.ReplyTo), Media: mediaKind(v.Media)}
		out.FromId = PeerID(v.FromId)
		if out.FromId == 0 && Kind(out.PeerId) == pb.Peer_USER && !v.Out {
			out.FromId = out.PeerId // в личке отправитель — собеседник
		}
		if v.EditDate != nil {
			out.EditDate = int64(*v.EditDate)
		}
		return out
	case *tl.MessageService:
		out := &pb.Message{PeerId: PeerID(v.PeerId), Id: v.Id, Date: int64(v.Date), Out: v.Out,
			FromId: PeerID(v.FromId), ReplyTo: replyTo(v.ReplyTo), Service: serviceKind(v.Action)}
		return out
	}
	return nil
}

// Messages — список (пустые пропускаются).
func Messages(ms []tl.MessageClass) []*pb.Message {
	out := make([]*pb.Message, 0, len(ms))
	for _, m := range ms {
		if x := Message(m); x != nil {
			out = append(out, x)
		}
	}
	return out
}

// Dialog — диалог в доменный.
func Dialog(d tl.DialogClass) *pb.Dialog {
	v, ok := d.(*tl.Dialog)
	if !ok {
		return nil // папки (dialogFolder) — В6
	}
	out := &pb.Dialog{PeerId: PeerID(v.Peer), TopMessage: v.TopMessage, UnreadCount: v.UnreadCount, Pinned: v.Pinned,
		ReadInboxMaxId: v.ReadInboxMaxId, ReadOutboxMaxId: v.ReadOutboxMaxId}
	if v.FolderId != nil {
		out.Folder = *v.FolderId
	}
	return out
}
