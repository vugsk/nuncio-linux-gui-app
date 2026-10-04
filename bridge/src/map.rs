//! Отображение доменных типов в protobuf `client.proto` и обратно.

use domain::model::{
    AuthState, AuthStep, ChatActionKind, ChatInfo, CodeInfo, Command, CommandResult, ConnState, Country, CountryCode,
    Dialog, DomainUpdate, Entity, EntityKind, ErrorInfo, FileRef, Folder, Media, MediaKind, Poll, PollOption,
    SearchFilter, WebPage, ErrorKind, Event, Message, Payload, Peer, PeerKind, PeerRef, ProxyKind, ProxySettings,
    StartupErrorKind, UpdateState, UpdatesBatch, User,
};

use crate::proto::{self, command::Body as C, event::Body as E};

pub fn proxy_to(p: &ProxySettings) -> proto::ProxySettings {
    let kind = match p.kind {
        ProxyKind::None => proto::proxy_settings::Kind::None,
        ProxyKind::Socks5 => proto::proxy_settings::Kind::Socks5,
        ProxyKind::MtProxy => proto::proxy_settings::Kind::Mtproxy,
    };
    proto::ProxySettings {
        kind: kind as i32,
        addr: p.addr.clone(),
        user: p.user.clone(),
        password: p.password.clone(),
        secret: p.secret.clone(),
    }
}

fn peer_ref_to(p: PeerRef) -> proto::PeerRef {
    proto::PeerRef { id: p.id, access_hash: p.access_hash }
}

pub fn state_to(s: UpdateState) -> proto::UpdateState {
    proto::UpdateState { pts: s.pts, qts: s.qts, seq: s.seq, date: s.date }
}

fn state_from(s: proto::UpdateState) -> UpdateState {
    UpdateState { pts: s.pts, qts: s.qts, seq: s.seq, date: s.date }
}

fn peer_from(p: proto::Peer) -> Peer {
    Peer {
        id: p.id,
        kind: PeerKind::of(p.id),
        name: p.name,
        username: p.username,
        access_hash: p.access_hash,
        photo_id: p.photo_id,
        photo_dc: p.photo_dc,
        is_self: p.is_self,
        is_bot: p.is_bot,
        broadcast: p.broadcast,
        min: p.min,
        contact: p.contact,
    }
}

fn folder_from(f: proto::Folder) -> Folder {
    Folder {
        id: f.id,
        title: f.title,
        emoticon: f.emoticon,
        chatlist: f.chatlist,
        contacts: f.contacts,
        non_contacts: f.non_contacts,
        groups: f.groups,
        broadcasts: f.broadcasts,
        bots: f.bots,
        exclude_muted: f.exclude_muted,
        exclude_read: f.exclude_read,
        exclude_archived: f.exclude_archived,
        pinned: f.pinned,
        include: f.include,
        exclude: f.exclude,
    }
}

fn folder_to(f: Folder) -> proto::Folder {
    proto::Folder {
        id: f.id,
        title: f.title,
        emoticon: f.emoticon,
        chatlist: f.chatlist,
        contacts: f.contacts,
        non_contacts: f.non_contacts,
        groups: f.groups,
        broadcasts: f.broadcasts,
        bots: f.bots,
        exclude_muted: f.exclude_muted,
        exclude_read: f.exclude_read,
        exclude_archived: f.exclude_archived,
        pinned: f.pinned,
        include: f.include,
        exclude: f.exclude,
    }
}

fn tri(v: i32) -> Option<bool> {
    (v >= 0).then_some(v > 0)
}

fn message_from(m: proto::Message) -> Message {
    Message {
        peer_id: m.peer_id,
        id: m.id,
        date: m.date,
        from_id: m.from_id,
        out: m.out,
        text: m.text,
        reply_to: m.reply_to,
        media: m.media,
        service: m.service,
        random_id: m.random_id,
        edit_date: m.edit_date,
        send_state: 0,
        send_error: String::new(),
        fwd_from_id: m.fwd_from_id,
        fwd_from_name: m.fwd_from_name,
        pinned: m.pinned,
        entities: m.entities.into_iter().map(entity_from).collect(),
        media_info: m.media_info.map(|x| Box::new(media_from(x))),
        grouped_id: m.grouped_id,
    }
}

fn file_from(f: proto::FileRef) -> FileRef {
    FileRef {
        photo: f.kind == proto::file_ref::Kind::Photo as i32,
        id: f.id,
        access_hash: f.access_hash,
        file_reference: f.file_reference,
        dc: f.dc,
        thumb_size: f.thumb_size,
        size: f.size,
    }
}

fn file_to(f: FileRef) -> proto::FileRef {
    proto::FileRef {
        kind: if f.photo { proto::file_ref::Kind::Photo } else { proto::file_ref::Kind::Document } as i32,
        id: f.id,
        access_hash: f.access_hash,
        file_reference: f.file_reference,
        dc: f.dc,
        thumb_size: f.thumb_size,
        size: f.size,
    }
}

fn media_from(m: proto::Media) -> Media {
    use proto::media::Kind as K;
    let kind = match K::try_from(m.kind).unwrap_or(K::Other) {
        K::Photo => MediaKind::Photo,
        K::Document => MediaKind::Document,
        K::Video => MediaKind::Video,
        K::Voice => MediaKind::Voice,
        K::Audio => MediaKind::Audio,
        K::Sticker => MediaKind::Sticker,
        K::Gif => MediaKind::Gif,
        K::VideoNote => MediaKind::VideoNote,
        K::Webpage => MediaKind::WebPage,
        K::Poll => MediaKind::Poll,
        K::None | K::Other => MediaKind::Other,
    };
    Media {
        kind,
        file: m.file.map(file_from),
        full: m.full.map(file_from),
        thumb: m.thumb.map(file_from),
        stripped: m.stripped,
        width: m.width,
        height: m.height,
        duration: m.duration,
        mime: m.mime,
        name: m.name,
        size: m.size,
        waveform: m.waveform,
        title: m.title,
        performer: m.performer,
        emoji: m.emoji,
        webpage: m.webpage.map(|w| WebPage {
            url: w.url,
            site: w.site,
            title: w.title,
            description: w.description,
            photo: w.photo.map(file_from),
            photo_stripped: w.photo_stripped,
            photo_width: w.photo_width,
            photo_height: w.photo_height,
        }),
        local_path: String::new(),
        poll: m.poll.map(|p| Poll {
            id: p.id,
            question: p.question,
            options: p
                .options
                .into_iter()
                .map(|o| PollOption { text: o.text, option: o.option, voters: o.voters, chosen: o.chosen, correct: o.correct })
                .collect(),
            total_voters: p.total_voters,
            closed: p.closed,
            multiple: p.multiple,
            quiz: p.quiz,
        }),
    }
}

fn entity_from(e: proto::MessageEntity) -> Entity {
    Entity { kind: EntityKind::from_code(e.kind), offset: e.offset, length: e.length, url: e.url }
}

fn entity_to(e: Entity) -> proto::MessageEntity {
    proto::MessageEntity { kind: e.kind.code(), offset: e.offset, length: e.length, url: e.url }
}

fn dialog_from(d: proto::Dialog) -> Dialog {
    Dialog {
        peer_id: d.peer_id,
        top_message: d.top_message,
        unread_count: d.unread_count,
        pinned: d.pinned,
        folder: d.folder,
        read_inbox_max_id: d.read_inbox_max_id,
        read_outbox_max_id: d.read_outbox_max_id,
        muted: d.muted,
        unread_mark: d.unread_mark,
    }
}

fn batch_from(b: proto::UpdatesBatch) -> UpdatesBatch {
    use proto::domain_update::Body as U;
    let updates = b
        .updates
        .into_iter()
        .filter_map(|u| {
            Some(match u.body? {
                U::NewMessage(m) => DomainUpdate::NewMessage(message_from(m)),
                U::EditMessage(m) => DomainUpdate::EditMessage(message_from(m)),
                U::DeleteMessages(d) => DomainUpdate::DeleteMessages { peer_id: d.peer_id, ids: d.ids },
                U::ReadInbox(r) => DomainUpdate::ReadInbox { peer_id: r.peer_id, max_id: r.max_id, still_unread: r.still_unread },
                U::ReadOutbox(r) => DomainUpdate::ReadOutbox { peer_id: r.peer_id, max_id: r.max_id },
                U::Reset(_) => DomainUpdate::Reset,
                U::ResetChannel(r) => DomainUpdate::ResetChannel { peer_id: r.peer_id },
                U::Pinned(p) => DomainUpdate::Pinned { peer_id: p.peer_id, ids: p.ids, pinned: p.pinned },
                U::FoldersChanged(_) => DomainUpdate::FoldersChanged,
                U::PeerFolder(f) => DomainUpdate::PeerFolder { peer_id: f.peer_id, folder: f.folder },
                U::PeerSettings(p) => DomainUpdate::PeerSettings {
                    peer_id: p.peer_id,
                    muted: tri(p.muted),
                    pinned: tri(p.pinned),
                    unread_mark: tri(p.unread_mark),
                },
                U::PinnedDialogs(_) => DomainUpdate::PinnedDialogs,
            })
        })
        .collect();
    UpdatesBatch {
        updates,
        peers: b.peers.into_iter().map(peer_from).collect(),
        state: b.state.map(state_from),
        channel_states: b.channel_states.into_iter().map(|c| (c.peer_id, c.pts)).collect(),
    }
}

pub fn command_to(id: u64, cmd: Command) -> proto::Command {
    let body = match cmd {
        Command::Ping(message) => C::Ping(proto::Ping { message }),
        Command::RequestQr => C::RequestQr(proto::RequestQr {}),
        Command::SendCode { phone } => C::SendCode(proto::SendCode { phone }),
        Command::ResendCode => C::ResendCode(proto::ResendCode {}),
        Command::CancelCode => C::CancelCode(proto::CancelCode {}),
        Command::SignIn { code } => C::SignIn(proto::SignIn { code }),
        Command::CheckPassword { password } => C::CheckPassword(proto::CheckPassword { password }),
        Command::SignUp { first_name, last_name } => C::SignUp(proto::SignUp { first_name, last_name }),
        Command::LogOut => C::LogOut(proto::LogOut {}),
        Command::SetProxy(p) => C::SetProxy(proto::SetProxy { proxy: Some(proxy_to(&p)) }),
        Command::TestProxy(p) => C::TestProxy(proto::TestProxy { proxy: Some(proxy_to(&p)) }),
        Command::GetCountries { lang } => C::GetCountries(proto::GetCountries { lang_code: lang }),
        Command::RetryStartup => C::RetryStartup(proto::RetryStartup {}),
        Command::LoadDialogs { limit, offset_date, offset_id, offset_peer } => C::LoadDialogs(proto::LoadDialogs {
            limit,
            offset_date,
            offset_id,
            offset_peer: offset_peer.map(peer_ref_to),
        }),
        Command::LoadHistory { peer, offset_id, add_offset, limit } => C::LoadHistory(proto::LoadHistory {
            peer: Some(peer_ref_to(peer)),
            offset_id,
            limit,
            add_offset,
        }),
        Command::SendMessage { peer, text, random_id } => C::SendMessage(proto::SendMessage {
            peer: Some(peer_ref_to(peer)),
            text,
            random_id,
            reply_to: 0,
            entities: Vec::new(),
        }),
        Command::MarkRead { peer, max_id } => C::MarkRead(proto::MarkRead { peer: Some(peer_ref_to(peer)), max_id }),
        Command::DownloadAvatar { peer, photo_id, dc } => C::DownloadAvatar(proto::DownloadAvatar {
            peer: Some(peer_ref_to(peer)),
            photo_id,
            dc,
        }),
        Command::SendMessageReply { peer, text, random_id, reply_to, entities } => C::SendMessage(proto::SendMessage {
            peer: Some(peer_ref_to(peer)),
            text,
            random_id,
            reply_to,
            entities: entities.into_iter().map(entity_to).collect(),
        }),
        Command::EditMessage { peer, id, text, entities } => {
            C::EditMessage(proto::EditMessage { peer: Some(peer_ref_to(peer)), id, text,
                entities: entities.into_iter().map(entity_to).collect() })
        }
        Command::DeleteMessages { peer, ids, revoke } => {
            C::DeleteMessages(proto::DeleteMessagesCmd { peer: Some(peer_ref_to(peer)), ids, revoke })
        }
        Command::Forward { from, ids, targets } => C::Forward(proto::ForwardMessages {
            from_peer: Some(peer_ref_to(from)),
            ids,
            targets: targets
                .into_iter()
                .map(|(p, r)| proto::ForwardTarget { peer: Some(peer_ref_to(p)), random_ids: r })
                .collect(),
        }),
        Command::PinMessage { peer, id, unpin } => {
            C::PinMessage(proto::PinMessage { peer: Some(peer_ref_to(peer)), id, unpin, silent: false })
        }
        Command::LoadPinned { peer } => C::LoadPinned(proto::LoadPinned { peer: Some(peer_ref_to(peer)), limit: 20 }),
        Command::LoadMessages { peer, ids } => C::LoadMessages(proto::LoadMessages { peer: Some(peer_ref_to(peer)), ids }),
        Command::GetContacts => C::GetContacts(proto::GetContacts {}),
        Command::SearchPeers { query } => C::SearchPeers(proto::SearchPeers { query, limit: 20 }),
        Command::LoadFolders => C::LoadFolders(proto::LoadFolders {}),
        Command::SearchMessages {
            peer, query, filter, min_date, max_date, from, offset_id, limit, offset_rate, offset_peer,
        } => C::SearchMessages(proto::SearchMessages {
            peer: peer.map(peer_ref_to),
            query,
            filter: match filter {
                SearchFilter::All => proto::search_messages::Filter::All,
                SearchFilter::Media => proto::search_messages::Filter::Media,
                SearchFilter::Files => proto::search_messages::Filter::Files,
                SearchFilter::Links => proto::search_messages::Filter::Links,
                SearchFilter::Voice => proto::search_messages::Filter::Voice,
            } as i32,
            min_date,
            max_date,
            from: from.map(peer_ref_to),
            offset_id,
            limit,
            offset_rate,
            offset_peer: offset_peer.map(peer_ref_to),
        }),
        Command::GetChatInfo { peer, members_query } => {
            C::GetChatInfo(proto::GetChatInfo { peer: Some(peer_ref_to(peer)), members_query })
        }
        Command::ChatAction { peer, kind, revoke } => {
            use proto::chat_action::Kind as K;
            let kind = match kind {
                ChatActionKind::Mute => K::Mute,
                ChatActionKind::Unmute => K::Unmute,
                ChatActionKind::Archive => K::Archive,
                ChatActionKind::Unarchive => K::Unarchive,
                ChatActionKind::Pin => K::Pin,
                ChatActionKind::Unpin => K::Unpin,
                ChatActionKind::MarkUnread => K::MarkUnread,
                ChatActionKind::ClearHistory => K::ClearHistory,
                ChatActionKind::DeleteChat => K::DeleteChat,
                ChatActionKind::Leave => K::Leave,
                ChatActionKind::Block => K::Block,
                ChatActionKind::Unblock => K::Unblock,
            };
            C::ChatAction(proto::ChatAction { peer: Some(peer_ref_to(peer)), kind: kind as i32, revoke })
        }
        Command::DownloadFile { file, name, peer, msg_id } => C::DownloadFile(proto::DownloadFile {
            file: Some(file_to(file)),
            name,
            peer: Some(peer_ref_to(peer)),
            msg_id,
        }),
        Command::CancelTransfer { cmd_id } => C::CancelTransfer(proto::CancelTransfer { cmd_id }),
        Command::SendVote { peer, msg_id, options } => {
            C::SendVote(proto::SendVote { peer: Some(peer_ref_to(peer)), msg_id, options })
        }
        Command::PickFiles { multiple, title } => C::PickFiles(proto::PickFiles { multiple, title, directory: false }),
        Command::PickFolder { title } => C::PickFiles(proto::PickFiles { multiple: false, title, directory: true }),
        Command::GetProfile => C::GetProfile(proto::GetProfile {}),
        Command::UpdateProfile { first_name, last_name, about, username } => C::UpdateProfile(proto::UpdateProfile {
            first_name,
            last_name,
            about,
            set_username: username.is_some(),
            username: username.unwrap_or_default(),
        }),
        Command::SetProfilePhoto { path } => C::SetProfilePhoto(proto::SetProfilePhoto { path }),
        Command::GetSessions => C::GetSessions(proto::GetSessions {}),
        Command::WatchSystem => C::WatchSystem(proto::WatchSystem {}),
        Command::TerminateSession { hash } => C::TerminateSession(proto::TerminateSession { hash }),
        Command::SaveFileAs { src, name, title } => C::SaveFileAs(proto::SaveFileAs { src, name, title }),
        Command::SendMedia { peer, items, reply_to } => C::SendMedia(proto::SendMedia {
            peer: Some(peer_ref_to(peer)),
            items: items
                .into_iter()
                .map(|i| proto::MediaUpload {
                    path: i.path,
                    name: i.name,
                    mime: i.mime,
                    as_photo: i.as_photo,
                    random_id: i.random_id,
                    caption: i.caption,
                    entities: i.entities.into_iter().map(entity_to).collect(),
                })
                .collect(),
            reply_to,
        }),
        Command::UpdateFolder { folder, peers } => C::UpdateFolder(proto::UpdateFolder {
            folder: Some(folder_to(folder)),
            peers: peers.into_iter().map(peer_ref_to).collect(),
        }),
    };
    proto::Command { id, account_id: 1, body: Some(body) }
}

fn error_from(e: proto::Error) -> ErrorInfo {
    use proto::error::Kind as K;
    let kind = match K::try_from(e.kind).unwrap_or(K::Internal) {
        K::Network => ErrorKind::Network,
        K::Rpc => ErrorKind::Rpc,
        K::Flood => ErrorKind::Flood,
        K::Auth => ErrorKind::Auth,
        K::Cancelled => ErrorKind::Cancelled,
        K::Internal | K::Unspecified => ErrorKind::Internal,
    };
    ErrorInfo { kind, code: e.code, text: e.text, retry_after: e.retry_after }
}

fn country_from(c: proto::Country) -> Country {
    Country {
        iso2: c.iso2,
        name: c.name,
        default_name: c.default_name,
        codes: c
            .codes
            .into_iter()
            .map(|k| CountryCode { code: k.code, prefixes: k.prefixes, patterns: k.patterns })
            .collect(),
    }
}

fn auth_from(a: proto::AuthState) -> AuthState {
    use proto::auth_state::Step as S;
    let step = match S::try_from(a.step).unwrap_or(S::Unspecified) {
        S::Unspecified => AuthStep::Unknown,
        S::Qr => AuthStep::Qr,
        S::Code => AuthStep::Code,
        S::Password => AuthStep::Password,
        S::SignUp => AuthStep::SignUp,
        S::Ready => AuthStep::Ready,
        S::LoggedOut => AuthStep::LoggedOut,
    };
    AuthState {
        step,
        qr_url: a.qr_url,
        qr_expires_unix: a.qr_expires_unix,
        code: a
            .code
            .map(|c| CodeInfo { kind: c.kind, length: c.length, next_kind: c.next_kind, timeout: c.timeout })
            .unwrap_or_default(),
        password_hint: a.password_hint,
        has_recovery: a.has_recovery,
        terms: a.terms,
        user: a.user.map(|u| User { id: u.id, first_name: u.first_name, last_name: u.last_name, username: u.username }),
        reason: a.reason,
    }
}

/// Событие Go → доменное событие (`None` — пустое или неизвестное событие).
pub fn event_from(ev: proto::Event) -> Option<Event> {
    Some(match ev.body? {
        E::Result(r) => {
            use proto::command_result::Payload as P;
            let payload = match r.payload {
                None => Payload::None,
                Some(P::Pong(p)) => Payload::Pong { message: p.message, core_version: p.core_version },
                Some(P::Countries(c)) => Payload::Countries(c.countries.into_iter().map(country_from).collect()),
                Some(P::ProxyCheck(p)) => Payload::ProxyCheck { rtt_ms: p.rtt_ms },
                Some(P::Dialogs(d)) => Payload::Dialogs {
                    dialogs: d.dialogs.into_iter().map(dialog_from).collect(),
                    messages: d.messages.into_iter().map(message_from).collect(),
                    peers: d.peers.into_iter().map(peer_from).collect(),
                    total: d.total,
                    complete: d.complete,
                },
                Some(P::History(h)) => Payload::History {
                    messages: h.messages.into_iter().map(message_from).collect(),
                    peers: h.peers.into_iter().map(peer_from).collect(),
                    total: h.total,
                    next_rate: h.next_rate,
                },
                Some(P::Sent(s)) => Payload::Sent(s.message.map(message_from).unwrap_or_default()),
                Some(P::File(f)) => Payload::File(f.path),
                Some(P::Peers(p)) => Payload::Peers(p.peers.into_iter().map(peer_from).collect()),
                Some(P::Folders(f)) => Payload::Folders(f.folders.into_iter().map(folder_from).collect()),
                Some(P::FilePaths(f)) => Payload::FilePaths(f.paths),
                Some(P::Profile(p)) => Payload::Profile(domain::model::Profile {
                    me: p.self_.map(peer_from).unwrap_or_default(),
                    first_name: p.first_name,
                    last_name: p.last_name,
                    about: p.about,
                    username: p.username,
                    phone: p.phone,
                }),
                Some(P::Sessions(l)) => Payload::Sessions(
                    l.sessions
                        .into_iter()
                        .map(|x| domain::model::Session {
                            hash: x.hash,
                            current: x.current,
                            device: x.device,
                            platform: x.platform,
                            app: x.app,
                            ip: x.ip,
                            country: x.country,
                            date_active: x.date_active,
                        })
                        .collect(),
                ),
                Some(P::SentList(s)) => Payload::SentList(s.messages.into_iter().map(message_from).collect()),
                Some(P::ChatInfo(c)) => Payload::ChatInfo(ChatInfo {
                    peer_id: c.peer_id,
                    about: c.about,
                    members: c.members,
                    participants: c.participants.into_iter().map(|m| (m.user_id, m.role)).collect(),
                    peers: c.peers.into_iter().map(peer_from).collect(),
                    blocked: c.blocked,
                }),
            };
            Event::Result(CommandResult { cmd_id: r.cmd_id, error: r.error.map(error_from), payload })
        }
        E::Connection(c) => {
            use proto::connection_state::State as S;
            Event::Connection(match S::try_from(c.state).unwrap_or(S::Unspecified) {
                S::Connecting | S::Unspecified => ConnState::Connecting,
                S::Connected => ConnState::Connected,
                S::WaitingNetwork => ConnState::WaitingNetwork,
            })
        }
        E::Auth(a) => Event::Auth(auth_from(a)),
        E::Updates(b) => Event::Updates(batch_from(b)),
        E::Startup(s) => {
            use proto::startup_error::Kind as K;
            let kind = match K::try_from(s.kind).unwrap_or(K::Other) {
                K::KeyringUnavailable => StartupErrorKind::KeyringUnavailable,
                K::KeyringDismissed => StartupErrorKind::KeyringDismissed,
                K::Config => StartupErrorKind::Config,
                K::Other | K::Unspecified => StartupErrorKind::Other,
            };
            Event::Startup { kind, text: s.text }
        }
        E::Fatal(f) => Event::Fatal(f.text),
        E::Transfer(t) => Event::Transfer { cmd_id: t.cmd_id, done: t.done, total: t.total },
        E::System(s) => Event::SystemTheme(s.color_scheme),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn every_command_maps_and_survives_encoding() {
        let p = ProxySettings { kind: ProxyKind::MtProxy, addr: "a:1".into(), secret: "dd00".into(), ..Default::default() };
        for cmd in [
            Command::Ping("x".into()),
            Command::RequestQr,
            Command::SendCode { phone: "+7".into() },
            Command::ResendCode,
            Command::CancelCode,
            Command::SignIn { code: "1".into() },
            Command::CheckPassword { password: "p".into() },
            Command::SignUp { first_name: "a".into(), last_name: "b".into() },
            Command::LogOut,
            Command::SetProxy(p.clone()),
            Command::TestProxy(p.clone()),
            Command::GetCountries { lang: "ru".into() },
            Command::RetryStartup,
            Command::LoadDialogs { limit: 100, offset_date: 1, offset_id: 2, offset_peer: Some(PeerRef { id: -5, access_hash: 0 }) },
            Command::LoadHistory { peer: PeerRef { id: 5, access_hash: 6 }, offset_id: 0, add_offset: 0, limit: 50 },
            Command::SendMessage { peer: PeerRef { id: 5, access_hash: 6 }, text: "т".into(), random_id: 7 },
            Command::MarkRead { peer: PeerRef { id: 5, access_hash: 6 }, max_id: 9 },
            Command::DownloadAvatar { peer: PeerRef { id: 5, access_hash: 6 }, photo_id: 1, dc: 4 },
        ] {
            let pc = command_to(9, cmd.clone());
            assert_eq!(pc.id, 9);
            let back = proto::Command::decode(pc.encode_to_vec().as_slice()).unwrap();
            assert!(back.body.is_some(), "{cmd:?}");
        }
        assert_eq!(proxy_to(&p).kind, proto::proxy_settings::Kind::Mtproxy as i32);
    }

    #[test]
    fn events_map() {
        let ev = proto::Event {
            body: Some(E::Auth(proto::AuthState {
                step: proto::auth_state::Step::Code as i32,
                code: Some(proto::CodeInfo { kind: "app".into(), length: 5, next_kind: "sms".into(), timeout: 30 }),
                ..Default::default()
            })),
        };
        match event_from(ev) {
            Some(Event::Auth(a)) => assert_eq!((a.step, a.code.length), (AuthStep::Code, 5)),
            other => panic!("{other:?}"),
        }
        let ev = proto::Event {
            body: Some(E::Result(proto::CommandResult {
                cmd_id: 3,
                error: Some(proto::Error { kind: proto::error::Kind::Flood as i32, code: 420, text: "FLOOD_WAIT".into(),
                    retry_after: 42 }),
                payload: None,
            })),
        };
        match event_from(ev) {
            Some(Event::Result(r)) => assert_eq!(r.error.unwrap().kind, ErrorKind::Flood),
            other => panic!("{other:?}"),
        }
        assert_eq!(event_from(proto::Event { body: None }), None);
        let ev = proto::Event {
            body: Some(E::Updates(proto::UpdatesBatch {
                updates: vec![proto::DomainUpdate {
                    body: Some(proto::domain_update::Body::NewMessage(proto::Message { peer_id: -1_000_000_000_005, id: 3,
                        text: "т".into(), ..Default::default() })),
                }],
                peers: vec![proto::Peer { id: -1_000_000_000_005, name: "канал".into(), ..Default::default() }],
                state: Some(proto::UpdateState { pts: 9, ..Default::default() }),
                channel_states: vec![proto::ChannelState { peer_id: -1_000_000_000_005, pts: 3 }],
            })),
        };
        match event_from(ev) {
            Some(Event::Updates(b)) => {
                assert_eq!(b.state.unwrap().pts, 9);
                assert_eq!(b.peers[0].kind, PeerKind::Channel);
                assert!(matches!(&b.updates[0], DomainUpdate::NewMessage(m) if m.id == 3));
            }
            other => panic!("{other:?}"),
        }
    }
}
