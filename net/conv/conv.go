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
	"time"

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
	p := &pb.Peer{Id: v.Id, Kind: pb.Peer_USER, Username: str(v.Username), IsSelf: v.Self, IsBot: v.Bot, Min: v.Min,
		Contact: v.Contact}
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
// EmptyLog — куда сообщить о сообщении без текста и без вложения (только id и
// имена типов, без содержимого): такие приходят и показываются пустым пузырём.
var EmptyLog func(format string, args ...any)

func Message(m tl.MessageClass) *pb.Message {
	switch v := m.(type) {
	case *tl.Message:
		if v.Message == "" && v.RichMessage == nil && EmptyLog != nil {
			if _, empty := v.Media.(*tl.MessageMediaEmpty); v.Media == nil || empty {
				EmptyLog("сообщение без текста и вложения: id %d, вложение %T, кнопки %T, сущностей %d, ответ %T",
					v.Id, v.Media, v.ReplyMarkup, len(v.Entities), v.ReplyTo)
			}
		}
		out := &pb.Message{PeerId: PeerID(v.PeerId), Id: v.Id, Date: int64(v.Date), Out: v.Out, Text: v.Message,
			ReplyTo: replyTo(v.ReplyTo), Media: mediaKind(v.Media)}
		out.FromId = PeerID(v.FromId)
		if out.FromId == 0 && Kind(out.PeerId) == pb.Peer_USER && !v.Out {
			out.FromId = out.PeerId // в личке отправитель — собеседник
		}
		// edit_hide: правка не показывается («изм.» нет), как у опросов и реакций.
		if v.EditDate != nil && !v.EditHide {
			out.EditDate = int64(*v.EditDate)
		}
		if f, ok := v.FwdFrom.(*tl.MessageFwdHeader); ok {
			out.FwdFromId = PeerID(f.FromId)
			out.FwdFromName = str(f.FromName)
			if out.FwdFromId == 0 && out.FwdFromName == "" {
				out.FwdFromName = str(f.PostAuthor)
			}
		}
		out.Pinned = v.Pinned
		out.Entities = Entities(v.Entities)
		out.MediaInfo = MediaInfo(v.Media)
		if v.RichMessage != nil && v.Message == "" {
			// Богатое сообщение (блоки статьи): текст с форматированием и первое фото.
			text, ents, photo := RichMessage(v.RichMessage)
			out.Text, out.Entities = text, ents
			if out.MediaInfo == nil && photo != nil {
				out.MediaInfo, out.Media = photo, "photo"
			}
		}
		if v.GroupedId != nil {
			out.GroupedId = *v.GroupedId
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
	if v.Pts != nil {
		out.Pts = *v.Pts
	}
	out.Muted = Muted(v.NotifySettings)
	out.UnreadMark = v.UnreadMark
	return out
}

// Muted — уведомления выключены (mute_until в будущем).
func Muted(n tl.PeerNotifySettingsClass) bool {
	s, ok := n.(*tl.PeerNotifySettings)
	return ok && s.MuteUntil != nil && int64(*s.MuteUntil) > time.Now().Unix()
}

// Folder — папка (dialogFilter) в доменную; nil — «Все чаты» (dialogFilterDefault).
func Folder(f tl.DialogFilterClass) *pb.Folder {
	ids := func(ps []tl.InputPeerClass) []int64 {
		var out []int64
		for _, p := range ps {
			if id := InputPeerID(p); id != 0 {
				out = append(out, id)
			}
		}
		return out
	}
	title := func(t tl.TextWithEntitiesClass) string {
		if v, ok := t.(*tl.TextWithEntities); ok {
			return v.Text
		}
		return ""
	}
	switch v := f.(type) {
	case *tl.DialogFilter:
		return &pb.Folder{Id: v.Id, Title: title(v.Title), Emoticon: str(v.Emoticon), Contacts: v.Contacts,
			NonContacts: v.NonContacts, Groups: v.Groups, Broadcasts: v.Broadcasts, Bots: v.Bots,
			ExcludeMuted: v.ExcludeMuted, ExcludeRead: v.ExcludeRead, ExcludeArchived: v.ExcludeArchived,
			Pinned: ids(v.PinnedPeers), Include: ids(v.IncludePeers), Exclude: ids(v.ExcludePeers)}
	case *tl.DialogFilterChatlist:
		return &pb.Folder{Id: v.Id, Title: title(v.Title), Emoticon: str(v.Emoticon), Chatlist: true,
			Pinned: ids(v.PinnedPeers), Include: ids(v.IncludePeers)}
	}
	return nil
}

// InputPeerID — помеченный id по ссылке InputPeer (0 — не собеседник).
func InputPeerID(p tl.InputPeerClass) int64 {
	switch v := p.(type) {
	case *tl.InputPeerUser:
		return v.UserId
	case *tl.InputPeerChat:
		return ChatID(v.ChatId)
	case *tl.InputPeerChannel:
		return ChannelID(v.ChannelId)
	}
	return 0
}

// Entities — форматирование сообщения в доменное (Дз-15). Смещения в UTF-16,
// как у Telegram; перевод в байты делает интерфейс.
func Entities(es []tl.MessageEntityClass) []*pb.MessageEntity {
	var out []*pb.MessageEntity
	add := func(k pb.MessageEntity_Kind, off, n int32, url string) {
		out = append(out, &pb.MessageEntity{Kind: k, Offset: off, Length: n, Url: url})
	}
	for _, e := range es {
		switch v := e.(type) {
		case *tl.MessageEntityBold:
			add(pb.MessageEntity_BOLD, v.Offset, v.Length, "")
		case *tl.MessageEntityItalic:
			add(pb.MessageEntity_ITALIC, v.Offset, v.Length, "")
		case *tl.MessageEntityUnderline:
			add(pb.MessageEntity_UNDERLINE, v.Offset, v.Length, "")
		case *tl.MessageEntityStrike:
			add(pb.MessageEntity_STRIKE, v.Offset, v.Length, "")
		case *tl.MessageEntityCode:
			add(pb.MessageEntity_CODE, v.Offset, v.Length, "")
		case *tl.MessageEntityPre:
			add(pb.MessageEntity_PRE, v.Offset, v.Length, "")
		case *tl.MessageEntityTextUrl:
			add(pb.MessageEntity_TEXT_URL, v.Offset, v.Length, v.Url)
		case *tl.MessageEntityUrl:
			add(pb.MessageEntity_URL, v.Offset, v.Length, "")
		case *tl.MessageEntityEmail:
			add(pb.MessageEntity_EMAIL, v.Offset, v.Length, "")
		case *tl.MessageEntityMention:
			add(pb.MessageEntity_MENTION, v.Offset, v.Length, "")
		case *tl.MessageEntityHashtag:
			add(pb.MessageEntity_HASHTAG, v.Offset, v.Length, "")
		case *tl.MessageEntitySpoiler:
			add(pb.MessageEntity_SPOILER, v.Offset, v.Length, "")
		case *tl.MessageEntityBlockquote:
			add(pb.MessageEntity_BLOCKQUOTE, v.Offset, v.Length, "")
		}
	}
	return out
}

// InputEntities — форматирование для sendMessage и editMessage. Ссылки,
// упоминания и хештеги в тексте сервер находит сам, их не отправляем.
func InputEntities(es []*pb.MessageEntity) []tl.MessageEntityClass {
	var out []tl.MessageEntityClass
	for _, e := range es {
		o, n := e.GetOffset(), e.GetLength()
		if n <= 0 || o < 0 {
			continue
		}
		switch e.GetKind() {
		case pb.MessageEntity_BOLD:
			out = append(out, &tl.MessageEntityBold{Offset: o, Length: n})
		case pb.MessageEntity_ITALIC:
			out = append(out, &tl.MessageEntityItalic{Offset: o, Length: n})
		case pb.MessageEntity_UNDERLINE:
			out = append(out, &tl.MessageEntityUnderline{Offset: o, Length: n})
		case pb.MessageEntity_STRIKE:
			out = append(out, &tl.MessageEntityStrike{Offset: o, Length: n})
		case pb.MessageEntity_CODE:
			out = append(out, &tl.MessageEntityCode{Offset: o, Length: n})
		case pb.MessageEntity_PRE:
			out = append(out, &tl.MessageEntityPre{Offset: o, Length: n})
		case pb.MessageEntity_TEXT_URL:
			out = append(out, &tl.MessageEntityTextUrl{Offset: o, Length: n, Url: e.GetUrl()})
		}
	}
	return out
}
