package api

import (
	"context"
	"fmt"
	"math"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// Веха В6: папки, поиск, сведения о чате, действия над чатом.

func (s *Service) loadFolders(ctx context.Context, id uint64) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.MessagesGetDialogFiltersRequest{})
	if err != nil {
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
	out := &pb.FolderList{}
	if fs, ok := v.(*tl.MessagesDialogFilters); ok {
		for _, f := range fs.Filters {
			if x := conv.Folder(f); x != nil {
				out.Folders = append(out.Folders, x)
			}
		}
	}
	s.log.Printf("папки: %d", len(out.Folders))
	return payload(id, out)
}

func searchFilter(f pb.SearchMessages_Filter) tl.MessagesFilterClass {
	switch f {
	case pb.SearchMessages_MEDIA:
		return &tl.InputMessagesFilterPhotoVideo{}
	case pb.SearchMessages_FILES:
		return &tl.InputMessagesFilterDocument{}
	case pb.SearchMessages_LINKS:
		return &tl.InputMessagesFilterUrl{}
	case pb.SearchMessages_VOICE:
		return &tl.InputMessagesFilterVoice{}
	}
	return &tl.InputMessagesFilterEmpty{}
}

// searchMessages — без peer по всем чатам (messages.searchGlobal), с peer —
// в чате (messages.search) с фильтрами Дз-18.
func (s *Service) searchMessages(ctx context.Context, id uint64, c *pb.SearchMessages) *pb.Event {
	limit := c.GetLimit()
	if limit <= 0 || limit > 100 {
		limit = 50
	}
	var req tlbin.Request
	if c.GetPeer().GetId() == 0 {
		var offsetPeer tl.InputPeerClass = &tl.InputPeerEmpty{}
		if c.GetOffsetPeer().GetId() != 0 {
			p, err := conv.InputPeer(c.GetOffsetPeer())
			if err != nil {
				return resultError(id, err)
			}
			offsetPeer = p
		}
		req = &tl.MessagesSearchGlobalRequest{Q: c.GetQuery(), Filter: searchFilter(c.GetFilter()),
			MinDate: int32(c.GetMinDate()), MaxDate: int32(c.GetMaxDate()), OffsetRate: c.GetOffsetRate(),
			OffsetPeer: offsetPeer, OffsetId: c.GetOffsetId(), Limit: limit}
	} else {
		peer, err := conv.InputPeer(c.GetPeer())
		if err != nil {
			return resultError(id, err)
		}
		r := &tl.MessagesSearchRequest{Peer: peer, Q: c.GetQuery(), Filter: searchFilter(c.GetFilter()),
			MinDate: int32(c.GetMinDate()), MaxDate: int32(c.GetMaxDate()), OffsetId: c.GetOffsetId(), Limit: limit}
		if c.GetFrom().GetId() != 0 {
			from, err := conv.InputPeer(c.GetFrom())
			if err != nil {
				return resultError(id, err)
			}
			r.FromId = from
		}
		req = r
	}
	v, err := s.m.Invoke(ctx, req)
	if err != nil {
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
	out, err := historyOf(v)
	if err != nil {
		return resultErrorKind(id, pb.Error_INTERNAL, err.Error())
	}
	if sl, ok := v.(*tl.MessagesMessagesSlice); ok && sl.NextRate != nil {
		out.NextRate = *sl.NextRate
	}
	s.upd.RememberPeers(out.Peers)
	return payload(id, out)
}

// getChatInfo — шапка и участники для правой панели (Св-1).
func (s *Service) getChatInfo(ctx context.Context, id uint64, c *pb.GetChatInfo) *pb.Event {
	ref := c.GetPeer()
	out := &pb.ChatInfo{PeerId: ref.GetId()}
	switch conv.Kind(ref.GetId()) {
	case pb.Peer_USER:
		in, err := conv.InputPeer(ref)
		if err != nil {
			return resultError(id, err)
		}
		user, ok := in.(*tl.InputPeerUser)
		if !ok {
			return resultErrorKind(id, pb.Error_INTERNAL, "не пользователь")
		}
		v, err := s.m.Invoke(ctx, &tl.UsersGetFullUserRequest{Id: &tl.InputUser{UserId: user.UserId, AccessHash: user.AccessHash}})
		if err != nil {
			return resultError(id, err)
		}
		if f, ok := v.(*tl.UsersUserFull); ok {
			if u, ok := f.FullUser.(*tl.UserFull); ok {
				if u.About != nil {
					out.About = *u.About
				}
				out.Blocked = u.Blocked
			}
			out.Peers = conv.Peers(f.Users, f.Chats)
		}
	case pb.Peer_CHAT:
		v, err := s.m.Invoke(ctx, &tl.MessagesGetFullChatRequest{ChatId: -ref.GetId()})
		if err != nil {
			return resultError(id, err)
		}
		if f, ok := v.(*tl.MessagesChatFull); ok {
			if full, ok := f.FullChat.(*tl.ChatFull); ok {
				out.About = full.About
				if ps, ok := full.Participants.(*tl.ChatParticipants); ok {
					for _, p := range ps.Participants {
						switch x := p.(type) {
						case *tl.ChatParticipant:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId})
						case *tl.ChatParticipantAdmin:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId, Role: 1})
						case *tl.ChatParticipantCreator:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId, Role: 2})
						}
					}
					out.Members = int32(len(out.Participants))
				}
			}
			out.Peers = conv.Peers(f.Users, f.Chats)
		}
	case pb.Peer_CHANNEL:
		ch, _ := conv.InputChannel(ref)
		v, err := s.m.Invoke(ctx, &tl.ChannelsGetFullChannelRequest{Channel: ch})
		if err != nil {
			return resultError(id, err)
		}
		broadcast := false
		if f, ok := v.(*tl.MessagesChatFull); ok {
			if full, ok := f.FullChat.(*tl.ChannelFull); ok {
				out.About = full.About
				if full.ParticipantsCount != nil {
					out.Members = *full.ParticipantsCount
				}
			}
			out.Peers = conv.Peers(f.Users, f.Chats)
			for _, p := range out.Peers {
				if p.GetId() == ref.GetId() {
					broadcast = p.GetBroadcast()
				}
			}
		}
		// Участники — у супергрупп (у каналов — только число подписчиков, Св-6).
		if !broadcast {
			var filter tl.ChannelParticipantsFilterClass = &tl.ChannelParticipantsRecent{}
			if q := c.GetMembersQuery(); q != "" {
				filter = &tl.ChannelParticipantsSearch{Q: q}
			}
			v, err := s.m.Invoke(ctx, &tl.ChannelsGetParticipantsRequest{Channel: ch, Filter: filter, Limit: 200})
			if err == nil {
				if ps, ok := v.(*tl.ChannelsChannelParticipants); ok {
					for _, p := range ps.Participants {
						switch x := p.(type) {
						case *tl.ChannelParticipant:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId})
						case *tl.ChannelParticipantSelf:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId})
						case *tl.ChannelParticipantAdmin:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId, Role: 1})
						case *tl.ChannelParticipantCreator:
							out.Participants = append(out.Participants, &pb.Member{UserId: x.UserId, Role: 2})
						}
					}
					out.Peers = append(out.Peers, conv.Peers(ps.Users, ps.Chats)...)
				}
			} else {
				s.log.Printf("участники: %T", err)
			}
		}
	}
	s.upd.RememberPeers(out.Peers)
	return payload(id, out)
}

// chatAction — действия над чатом (Сп-4, Св-1).
func (s *Service) chatAction(ctx context.Context, id uint64, c *pb.ChatAction) *pb.Event {
	ref := c.GetPeer()
	peer, err := conv.InputPeer(ref)
	if err != nil {
		return resultError(id, err)
	}
	var req tlbin.Request
	channel, isChannel := conv.InputChannel(ref)
	switch c.GetKind() {
	case pb.ChatAction_MUTE, pb.ChatAction_UNMUTE:
		until := int32(0)
		if c.GetKind() == pb.ChatAction_MUTE {
			until = math.MaxInt32
		}
		req = &tl.AccountUpdateNotifySettingsRequest{Peer: &tl.InputNotifyPeer{Peer: peer},
			Settings: &tl.InputPeerNotifySettings{MuteUntil: &until}}
	case pb.ChatAction_ARCHIVE, pb.ChatAction_UNARCHIVE:
		folder := int32(0)
		if c.GetKind() == pb.ChatAction_ARCHIVE {
			folder = 1
		}
		req = &tl.FoldersEditPeerFoldersRequest{FolderPeers: []tl.InputFolderPeerClass{
			&tl.InputFolderPeer{Peer: peer, FolderId: folder}}}
	case pb.ChatAction_PIN, pb.ChatAction_UNPIN:
		req = &tl.MessagesToggleDialogPinRequest{Pinned: c.GetKind() == pb.ChatAction_PIN,
			Peer: &tl.InputDialogPeer{Peer: peer}}
	case pb.ChatAction_MARK_UNREAD:
		req = &tl.MessagesMarkDialogUnreadRequest{Unread: true, Peer: &tl.InputDialogPeer{Peer: peer}}
	case pb.ChatAction_CLEAR_HISTORY:
		if isChannel {
			req = &tl.ChannelsDeleteHistoryRequest{Channel: channel}
		} else {
			req = &tl.MessagesDeleteHistoryRequest{JustClear: true, Peer: peer}
		}
	case pb.ChatAction_DELETE_CHAT:
		if isChannel {
			req = &tl.ChannelsLeaveChannelRequest{Channel: channel}
		} else {
			req = &tl.MessagesDeleteHistoryRequest{Revoke: c.GetRevoke(), Peer: peer}
		}
	case pb.ChatAction_LEAVE:
		switch {
		case isChannel:
			req = &tl.ChannelsLeaveChannelRequest{Channel: channel}
		case conv.Kind(ref.GetId()) == pb.Peer_CHAT:
			req = &tl.MessagesDeleteChatUserRequest{ChatId: -ref.GetId(), UserId: &tl.InputUserSelf{}}
		default:
			return resultErrorKind(id, pb.Error_INTERNAL, "выйти можно только из группы или канала")
		}
	case pb.ChatAction_BLOCK:
		req = &tl.ContactsBlockRequest{Id: peer}
	case pb.ChatAction_UNBLOCK:
		req = &tl.ContactsUnblockRequest{Id: peer}
	default:
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("действие %v не поддерживается", c.GetKind()))
	}
	// Удаление истории идёт порциями: сервер отвечает affectedHistory с offset.
	for i := 0; i < 50; i++ {
		v, err := s.m.Invoke(ctx, req)
		if err != nil {
			s.auth.CheckAuthError(err)
			return resultError(id, err)
		}
		switch r := v.(type) {
		case *tl.MessagesAffectedHistory:
			s.upd.Process(ctx, &tl.UpdateShort{Update: &tl.UpdateDeleteMessages{Pts: r.Pts, PtsCount: r.PtsCount}}, nil)
			if r.Offset > 0 {
				continue
			}
		default:
			s.processUpdates(ctx, v)
		}
		break
	}
	s.log.Printf("действие над чатом: %v", c.GetKind())
	return payload(id, nil)
}

// updateFolder — папка целиком (messages.updateDialogFilter); access_hash чатов
// папки присылает Rust (они в его базе).
func (s *Service) updateFolder(ctx context.Context, id uint64, c *pb.UpdateFolder) *pb.Event {
	f := c.GetFolder()
	refs := map[int64]*pb.PeerRef{}
	for _, r := range c.GetPeers() {
		refs[r.GetId()] = r
	}
	peers := func(ids []int64) []tl.InputPeerClass {
		out := []tl.InputPeerClass{}
		for _, pid := range ids {
			ref := refs[pid]
			if ref == nil {
				ref = &pb.PeerRef{Id: pid}
			}
			if p, err := conv.InputPeer(ref); err == nil {
				out = append(out, p)
			}
		}
		return out
	}
	title := &tl.TextWithEntities{Text: f.GetTitle(), Entities: []tl.MessageEntityClass{}}
	var filter tl.DialogFilterClass
	if f.GetChatlist() {
		cl := &tl.DialogFilterChatlist{Id: f.GetId(), Title: title, PinnedPeers: peers(f.GetPinned()),
			IncludePeers: peers(f.GetInclude())}
		if e := f.GetEmoticon(); e != "" {
			cl.Emoticon = &e
		}
		filter = cl
	} else {
		df := &tl.DialogFilter{Id: f.GetId(), Title: title, Contacts: f.GetContacts(), NonContacts: f.GetNonContacts(),
			Groups: f.GetGroups(), Broadcasts: f.GetBroadcasts(), Bots: f.GetBots(), ExcludeMuted: f.GetExcludeMuted(),
			ExcludeRead: f.GetExcludeRead(), ExcludeArchived: f.GetExcludeArchived(), PinnedPeers: peers(f.GetPinned()),
			IncludePeers: peers(f.GetInclude()), ExcludePeers: peers(f.GetExclude())}
		if e := f.GetEmoticon(); e != "" {
			df.Emoticon = &e
		}
		filter = df
	}
	if _, err := s.m.Invoke(ctx, &tl.MessagesUpdateDialogFilterRequest{Id: f.GetId(), Filter: filter}); err != nil {
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
	return payload(id, nil)
}
