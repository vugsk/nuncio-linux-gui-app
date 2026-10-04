package fakeserver

import (
	"bytes"
	"crypto/pbkdf2"
	"crypto/rand"
	"crypto/sha256"
	"crypto/sha512"
	"encoding/binary"
	"fmt"
	"math/big"
	"strings"
	"sync"
	"time"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// Account — аккаунт поддельного сервера для проверки входа. Один объект можно
// отдать нескольким серверам (DC): токены QR и авторизации общие.
type Account struct {
	Phone      string // только цифры
	Code       string
	Password   string // пусто — без облачного пароля
	Hint       string
	Registered bool      // false — auth.signIn вернёт authorizationSignUpRequired
	UserID     int64     // id пользователя
	FirstName  string    // для ответа users.getUsers
	Terms      string    // текст условий при регистрации
	QRDC       int       // 0 — QR подтверждается в том же DC; иначе loginTokenMigrateTo на этот DC
	AutoQR     bool      // QR «сканируется» сразу (вход без человека в сквозных проверках)
	Contacts   []Contact // собеседники-пользователи с историей (диалоги)
	Folders    []FakeFolder
	Files      map[int64][]byte // содержимое файлов вложений по id (upload.getFile)
	Thumbs     map[int64][]byte // превью документов по id документа (thumb_size)
	// Свой профиль (настройки, В9).
	LastName  string
	SelfAbout string
	Username  string
	photoID   int64
	sessions  []*tl.Authorization
	PartDelay time.Duration // задержка на часть файла (виден ход скачивания)

	mu            sync.Mutex
	authorized    map[uint64]bool // auth_key_id -> вошли
	needPwd       map[uint64]bool // auth_key_id -> код принят, ждём пароль
	tokens        map[string]qrToken
	codeHash      string
	srp           *srpState
	loggedOut     int
	codesSent     int
	pts           int32
	msgs          []fakeMsg
	seeded        bool
	randoms       map[int64]int32
	readMax       map[int64]int32
	uploads       map[int64]map[int32][]byte // выгружаемые файлы: id → части
	uploadedMedia map[int64]tl.MessageMediaClass
	nextFile      int64
}

// ReadMax — до какого id клиент отметил чат прочитанным (messages.readHistory).
func (a *Account) ReadMax(peer int64) int32 {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.readMax[peer]
}

// Dump — все сообщения аккаунта (для сверки в тесте kill -9).
type DumpMsg struct {
	ID       int32    `json:"id"`
	Peer     int64    `json:"peer"`
	Out      bool     `json:"out"`
	Text     string   `json:"text"`
	Entities []string `json:"entities,omitempty"` // «имя смещение длина»
}

func (a *Account) Dump() []DumpMsg {
	a.mu.Lock()
	defer a.mu.Unlock()
	out := make([]DumpMsg, 0, len(a.msgs))
	for _, m := range a.msgs {
		d := DumpMsg{ID: m.id, Peer: m.peer, Out: m.out, Text: m.text}
		for _, e := range m.entities {
			var off, n int32
			switch v := e.(type) {
			case *tl.MessageEntityBold:
				off, n = v.Offset, v.Length
			case *tl.MessageEntityItalic:
				off, n = v.Offset, v.Length
			case *tl.MessageEntityTextUrl:
				off, n = v.Offset, v.Length
			case *tl.MessageEntityCode:
				off, n = v.Offset, v.Length
			}
			d.Entities = append(d.Entities, fmt.Sprintf("%s %d %d", e.TLName(), off, n))
		}
		out = append(out, d)
	}
	return out
}

func (a *Account) seed() {
	if a.seeded {
		return
	}
	a.seeded = true
	total := 0
	for _, c := range a.Contacts {
		total += len(c.Messages)
	}
	// По 10 минут между сообщениями: длинная история растягивается на дни.
	date := int32(time.Now().Unix()) - int32(total+1)*600
	for _, c := range a.Contacts {
		for i, t := range c.Messages {
			a.pts++
			date += 600
			out := c.OutEvery > 0 && i%c.OutEvery == c.OutEvery-1
			m := fakeMsg{id: int32(len(a.msgs) + 1), peer: c.ID, text: t, date: date, out: out}
			for _, at := range c.Attach {
				if at.Index == i {
					m.media, m.grouped = at.Media, at.Grouped
				}
			}
			a.msgs = append(a.msgs, m)
		}
	}
}

func (m fakeMsg) tl() *tl.Message {
	out := &tl.Message{Id: m.id, Out: m.out, PeerId: &tl.PeerUser{UserId: m.peer}, Date: m.date, Message: m.text,
		Pinned: m.pinned, Entities: m.entities}
	if m.edited != 0 {
		e := m.edited
		out.EditDate = &e
	}
	if m.media != nil {
		out.Media = m.media
	}
	if m.grouped != 0 {
		g := m.grouped
		out.GroupedId = &g
	}
	if m.fwdFrom != 0 {
		out.FwdFrom = &tl.MessageFwdHeader{FromId: &tl.PeerUser{UserId: m.fwdFrom}, Date: m.date}
	}
	return out
}

func (a *Account) find(id int32) *fakeMsg {
	for i := range a.msgs {
		if a.msgs[i].id == id && !a.msgs[i].deleted {
			return &a.msgs[i]
		}
	}
	return nil
}

func inputUser(p tl.InputPeerClass, self int64) (int64, bool) {
	switch v := p.(type) {
	case *tl.InputPeerUser:
		return v.UserId, true
	case *tl.InputPeerSelf:
		return self, true
	}
	return 0, false
}

func (a *Account) contactUsers() []tl.UserClass {
	out := []tl.UserClass{a.user()}
	for _, c := range a.Contacts {
		name := c.Name
		h := c.AccessHash
		out = append(out, &tl.User{Id: c.ID, FirstName: &name, AccessHash: &h})
	}
	return out
}

// PushMessage — входящее сообщение от контакта: сервер присылает
// updateShortMessage (pts по порядку). skip — «потерять» его (разрыв pts).
func (s *Server) PushMessage(from int64, text string, skip bool) int32 {
	a := s.opt.Account
	a.mu.Lock()
	a.init()
	a.seed()
	a.pts++
	m := fakeMsg{id: int32(len(a.msgs) + 1), peer: from, text: text, date: int32(time.Now().Unix())}
	a.msgs = append(a.msgs, m)
	pts := a.pts
	keys := map[uint64]bool{}
	for k := range a.authorized {
		keys[k] = true
	}
	a.mu.Unlock()
	if skip {
		return m.id
	}
	upd, _ := tlbin.Marshal(&tl.UpdateShortMessage{Id: m.id, UserId: from, Message: text, Pts: pts, PtsCount: 1,
		Date: m.date})
	s.mu.Lock()
	var conns []*conn
	for c := range s.conns {
		if keys[c.keyID] {
			conns = append(conns, c)
		}
	}
	s.mu.Unlock()
	for _, c := range conns {
		_ = c.sendMsgs(true, [][]byte{upd})
	}
	return m.id
}

// Contact — личный диалог поддельного аккаунта.
type Contact struct {
	ID         int64
	Name       string
	AccessHash int64
	Messages   []string // от старых к новым; id сообщений общие по аккаунту
	OutEvery   int      // каждое n-е из Messages — своё (0 — все входящие)
	Unread     int      // непрочитанных в диалоге (0 — по умолчанию одно; −1 — всё прочитано)
	About      string   // «о себе» для сведений о чате
	Archived   bool
	Muted      bool
	Attach     []Attach // вложения к сообщениям из Messages
}

// FakeFolder — папка поддельного аккаунта: явно включённые собеседники и виды.
type FakeFolder struct {
	ID       int32
	Title    string
	Include  []int64
	Contacts bool
}

type fakeMsg struct {
	id       int32
	peer     int64
	out      bool
	text     string
	date     int32
	pinned   bool
	deleted  bool
	edited   int32
	fwdFrom  int64
	entities []tl.MessageEntityClass
	media    tl.MessageMediaClass
	grouped  int64
}

type qrToken struct {
	key       uint64
	confirmed bool
}

// SignedIn — сколько ключей сейчас авторизовано.
func (a *Account) SignedIn() int {
	a.mu.Lock()
	defer a.mu.Unlock()
	return len(a.authorized)
}

// LoggedOut — сколько раз вызван auth.logOut.
func (a *Account) LoggedOut() int {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.loggedOut
}

// Revoke завершает все сессии аккаунта (как «завершить сеанс» с другого устройства).
func (a *Account) Revoke() {
	a.mu.Lock()
	a.authorized = map[uint64]bool{}
	a.mu.Unlock()
}

type rpcErr struct {
	code int32
	msg  string
}

func (e *rpcErr) Error() string { return fmt.Sprintf("%d %s", e.code, e.msg) }

func (a *Account) init() {
	if a.authorized == nil {
		a.authorized = map[uint64]bool{}
		a.needPwd = map[uint64]bool{}
		a.tokens = map[string]qrToken{}
	}
}

func (a *Account) user() *tl.User {
	name, last, phone := a.FirstName, a.LastName, a.Phone
	u := &tl.User{Self: true, Id: a.UserID, FirstName: &name, LastName: &last, Phone: &phone, AccessHash: &a.UserID}
	if a.Username != "" {
		un := a.Username
		u.Username = &un
	}
	if a.photoID != 0 {
		u.Photo = &tl.UserProfilePhoto{PhotoId: a.photoID, DcId: 2}
	}
	return u
}

func (a *Account) authorization() *tl.AuthAuthorization {
	return &tl.AuthAuthorization{User: a.user()}
}

// ConfirmQR — «телефон отсканировал код»: все выданные токены подтверждены, клиенты
// получают updateShort{updateLoginToken}. Возвращает число подтверждённых токенов.
func (s *Server) ConfirmQR() int {
	a := s.opt.Account
	a.mu.Lock()
	a.init()
	keys := map[uint64]bool{}
	for t, q := range a.tokens {
		q.confirmed = true
		a.tokens[t] = q
		keys[q.key] = true
	}
	a.mu.Unlock()
	upd, _ := tlbin.Marshal(&tl.UpdateShort{Update: &tl.UpdateLoginToken{}, Date: int32(time.Now().Unix())})
	s.mu.Lock()
	var conns []*conn
	for c := range s.conns {
		if keys[c.keyID] {
			conns = append(conns, c)
		}
	}
	s.mu.Unlock()
	for _, c := range conns {
		_ = c.sendMsgs(true, [][]byte{upd})
	}
	return len(keys)
}

func (c *conn) accountAnswer(o tlbin.Object) (tlbin.Object, error) {
	a := c.s.opt.Account
	a.mu.Lock()
	defer a.mu.Unlock()
	a.init()
	key := c.keyID
	unauthorized := &rpcErr{401, "AUTH_KEY_UNREGISTERED"}
	switch r := o.(type) {
	case *tl.AuthSendCodeRequest:
		if r.PhoneNumber != a.Phone {
			return nil, &rpcErr{400, "PHONE_NUMBER_INVALID"}
		}
		a.codesSent++
		a.codeHash = fmt.Sprintf("hash%d", a.codesSent)
		t := int32(30)
		return &tl.AuthSentCode{Type: &tl.AuthSentCodeTypeApp{Length: int32(len(a.Code))}, PhoneCodeHash: a.codeHash,
			NextType: &tl.AuthCodeTypeSms{}, Timeout: &t}, nil
	case *tl.AuthResendCodeRequest:
		if r.PhoneCodeHash != a.codeHash {
			return nil, &rpcErr{400, "PHONE_CODE_HASH_EMPTY"}
		}
		return &tl.AuthSentCode{Type: &tl.AuthSentCodeTypeSms{Length: int32(len(a.Code))}, PhoneCodeHash: a.codeHash}, nil
	case *tl.AuthCancelCodeRequest:
		a.codeHash = ""
		return boolTrue{}, nil
	case *tl.AuthSignInRequest:
		if r.PhoneNumber != a.Phone || r.PhoneCodeHash != a.codeHash || a.codeHash == "" {
			return nil, &rpcErr{400, "PHONE_CODE_EXPIRED"}
		}
		if r.PhoneCode == nil || *r.PhoneCode != a.Code {
			return nil, &rpcErr{400, "PHONE_CODE_INVALID"}
		}
		if !a.Registered {
			return &tl.AuthAuthorizationSignUpRequired{TermsOfService: &tl.HelpTermsOfService{
				Id: &tl.DataJSON{Data: "{}"}, Text: a.Terms, Entities: []tl.MessageEntityClass{}}}, nil
		}
		if a.Password != "" {
			a.needPwd[key] = true
			return nil, &rpcErr{401, "SESSION_PASSWORD_NEEDED"}
		}
		a.authorized[key] = true
		return a.authorization(), nil
	case *tl.AuthSignUpRequest:
		if r.PhoneNumber != a.Phone || r.PhoneCodeHash != a.codeHash || a.Registered {
			return nil, &rpcErr{400, "PHONE_NUMBER_OCCUPIED"}
		}
		if r.FirstName == "" {
			return nil, &rpcErr{400, "FIRSTNAME_INVALID"}
		}
		a.Registered, a.FirstName = true, r.FirstName
		a.authorized[key] = true
		return a.authorization(), nil
	case *tl.AccountGetPasswordRequest:
		if !a.authorized[key] && !a.needPwd[key] {
			return nil, unauthorized
		}
		st, err := newSRP(a.Password)
		if err != nil {
			return nil, err
		}
		a.srp = st
		id := st.id
		hint := a.Hint
		return &tl.AccountPassword{HasPassword: true, HasRecovery: true, Hint: &hint, SrpId: &id,
			SrpB: st.bBytes, CurrentAlgo: &tl.PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow{
				Salt1: st.salt1, Salt2: st.salt2, G: 3, P: dhPrime.FillBytes(make([]byte, 256))},
			NewAlgo: &tl.PasswordKdfAlgoUnknown{}, NewSecureAlgo: &tl.SecurePasswordKdfAlgoUnknown{},
			SecureRandom: make([]byte, 32)}, nil
	case *tl.AuthCheckPasswordRequest:
		in, ok := r.Password.(*tl.InputCheckPasswordSRP)
		if !a.needPwd[key] || a.srp == nil || !ok || in.SrpId != a.srp.id {
			return nil, &rpcErr{400, "SRP_ID_INVALID"}
		}
		st := a.srp
		a.srp = nil // одноразовый
		if !st.check(in.A, in.M1) {
			return nil, &rpcErr{400, "PASSWORD_HASH_INVALID"}
		}
		delete(a.needPwd, key)
		a.authorized[key] = true
		return a.authorization(), nil
	case *tl.AuthExportLoginTokenRequest:
		if a.authorized[key] {
			return a.loginSuccess(), nil
		}
		for t, q := range a.tokens {
			if q.key == key && q.confirmed {
				delete(a.tokens, t)
				if a.QRDC != 0 && a.QRDC != c.s.opt.DC {
					nt := randomToken()
					a.tokens[string(nt)] = qrToken{confirmed: true}
					return &tl.AuthLoginTokenMigrateTo{DcId: int32(a.QRDC), Token: nt}, nil
				}
				if a.Password != "" {
					a.needPwd[key] = true
					return nil, &rpcErr{401, "SESSION_PASSWORD_NEEDED"}
				}
				a.authorized[key] = true
				return a.loginSuccess(), nil
			}
		}
		tok := randomToken()
		a.tokens[string(tok)] = qrToken{key: key, confirmed: a.AutoQR}
		return &tl.AuthLoginToken{Expires: int32(time.Now().Unix() + 30), Token: tok}, nil
	case *tl.AuthImportLoginTokenRequest:
		q, ok := a.tokens[string(r.Token)]
		if !ok || !q.confirmed {
			return nil, &rpcErr{400, "AUTH_TOKEN_INVALID"}
		}
		delete(a.tokens, string(r.Token))
		if a.Password != "" {
			a.needPwd[key] = true
			return nil, &rpcErr{401, "SESSION_PASSWORD_NEEDED"}
		}
		a.authorized[key] = true
		return a.loginSuccess(), nil
	case *tl.UsersGetUsersRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		return &userVector{[]*tl.User{a.user()}}, nil
	case *tl.UpdatesGetStateRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		return &tl.UpdatesState{Pts: a.pts, Qts: 0, Date: int32(time.Now().Unix()), Seq: 0}, nil
	case *tl.AuthLogOutRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		delete(a.authorized, key)
		a.loggedOut++
		fmt.Println("выход из аккаунта")
		return &tl.AuthLoggedOut{}, nil
	case *tl.UpdatesGetDifferenceRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		st := &tl.UpdatesState{Pts: a.pts, Date: int32(time.Now().Unix()), Seq: 0}
		if r.Pts >= a.pts {
			return &tl.UpdatesDifferenceEmpty{Date: st.Date}, nil
		}
		var news []tl.MessageClass
		for _, m := range a.msgs {
			if m.id > r.Pts { // каждое сообщение увеличивает pts на 1: pts сообщения = его id
				news = append(news, m.tl())
			}
		}
		return &tl.UpdatesDifference{NewMessages: news, NewEncryptedMessages: []tl.EncryptedMessageClass{},
			OtherUpdates: []tl.UpdateClass{}, Chats: []tl.ChatClass{}, Users: a.contactUsers(), State: st}, nil
	case *tl.MessagesGetDialogsRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		out := &tl.MessagesDialogs{Chats: []tl.ChatClass{}, Users: a.contactUsers()}
		for _, c := range a.Contacts {
			var top *fakeMsg
			for i := range a.msgs {
				if a.msgs[i].peer == c.ID {
					top = &a.msgs[i]
				}
			}
			if top == nil {
				continue
			}
			unread := max(c.Unread, 0)
			if c.Unread == 0 {
				unread = 1
			}
			notify := &tl.PeerNotifySettings{}
			if c.Muted {
				until := int32(2147483647)
				notify.MuteUntil = &until
			}
			d := &tl.Dialog{Peer: &tl.PeerUser{UserId: c.ID}, TopMessage: top.id,
				UnreadCount: int32(unread), ReadInboxMaxId: top.id - int32(unread), NotifySettings: notify}
			if c.Archived {
				one := int32(1)
				d.FolderId = &one
			}
			out.Dialogs = append(out.Dialogs, d)
			out.Messages = append(out.Messages, top.tl())
		}
		return out, nil
	case *tl.MessagesGetHistoryRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		p, ok := r.Peer.(*tl.InputPeerUser)
		if !ok {
			return nil, &rpcErr{400, "PEER_ID_INVALID"}
		}
		out := &tl.MessagesMessages{Topics: []tl.ForumTopicClass{}, Chats: []tl.ChatClass{}, Users: a.contactUsers()}
		// Как Telegram: список от новых к старым, позиция — первое старше
		// offset_id, сдвиг add_offset (отрицательный — к новым), limit штук.
		var list []fakeMsg
		for i := len(a.msgs) - 1; i >= 0; i-- {
			if m := a.msgs[i]; m.peer == p.UserId && !m.deleted {
				list = append(list, m)
			}
		}
		pos := 0
		if r.OffsetId != 0 {
			for pos < len(list) && list[pos].id >= r.OffsetId {
				pos++
			}
		}
		start := max(pos+int(r.AddOffset), 0)
		for i := start; i < len(list) && len(out.Messages) < int(r.Limit); i++ {
			out.Messages = append(out.Messages, list[i].tl())
		}
		return out, nil
	case *tl.MessagesSendMessageRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		var peer int64
		switch p := r.Peer.(type) {
		case *tl.InputPeerUser:
			peer = p.UserId
		case *tl.InputPeerSelf:
			peer = a.UserID
		default:
			return nil, &rpcErr{400, "PEER_ID_INVALID"}
		}
		// Как настоящий сервер: повтор random_id не создаёт второе сообщение.
		if a.randoms == nil {
			a.randoms = map[int64]int32{}
		}
		if _, dup := a.randoms[r.RandomId]; dup {
			return nil, &rpcErr{400, "RANDOM_ID_DUPLICATE"}
		}
		a.pts++
		m := fakeMsg{id: int32(len(a.msgs) + 1), peer: peer, out: true, text: r.Message, date: int32(time.Now().Unix()),
			entities: r.Entities}
		a.msgs = append(a.msgs, m)
		a.randoms[r.RandomId] = m.id
		return &tl.UpdateShortSentMessage{Out: true, Id: m.id, Pts: a.pts, PtsCount: 1, Date: m.date}, nil
	case *tl.MessagesEditMessageRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		m := a.find(r.Id)
		if m == nil || !m.out || r.Message == nil {
			return nil, &rpcErr{400, "MESSAGE_ID_INVALID"}
		}
		m.text, m.edited = *r.Message, int32(time.Now().Unix())
		a.pts++
		return &tl.Updates{Updates: []tl.UpdateClass{&tl.UpdateEditMessage{Message: m.tl(), Pts: a.pts, PtsCount: 1}},
			Users: a.contactUsers(), Chats: []tl.ChatClass{}, Date: int32(time.Now().Unix())}, nil
	case *tl.MessagesDeleteMessagesRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		for _, id := range r.Id {
			if m := a.find(id); m != nil {
				m.deleted = true
			}
		}
		a.pts++
		return &tl.MessagesAffectedMessages{Pts: a.pts, PtsCount: 1}, nil
	case *tl.MessagesForwardMessagesRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		to, ok := inputUser(r.ToPeer, a.UserID)
		if !ok || len(r.RandomId) != len(r.Id) {
			return nil, &rpcErr{400, "PEER_ID_INVALID"}
		}
		out := &tl.Updates{Users: a.contactUsers(), Chats: []tl.ChatClass{}, Date: int32(time.Now().Unix())}
		for i, id := range r.Id {
			src := a.find(id)
			if src == nil {
				return nil, &rpcErr{400, "MESSAGE_ID_INVALID"}
			}
			if a.randoms == nil {
				a.randoms = map[int64]int32{}
			}
			if _, dup := a.randoms[r.RandomId[i]]; dup {
				return nil, &rpcErr{400, "RANDOM_ID_DUPLICATE"}
			}
			a.pts++
			m := fakeMsg{id: int32(len(a.msgs) + 1), peer: to, out: true, text: src.text, date: int32(time.Now().Unix()),
				fwdFrom: src.peer}
			a.msgs = append(a.msgs, m)
			a.randoms[r.RandomId[i]] = m.id
			out.Updates = append(out.Updates, &tl.UpdateMessageID{Id: m.id, RandomId: r.RandomId[i]},
				&tl.UpdateNewMessage{Message: m.tl(), Pts: a.pts, PtsCount: 1})
		}
		return out, nil
	case *tl.MessagesUpdatePinnedMessageRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		m := a.find(r.Id)
		if m == nil {
			return nil, &rpcErr{400, "MESSAGE_ID_INVALID"}
		}
		m.pinned = !r.Unpin
		a.pts++
		return &tl.Updates{Updates: []tl.UpdateClass{&tl.UpdatePinnedMessages{Pinned: m.pinned,
			Peer: &tl.PeerUser{UserId: m.peer}, Messages: []int32{m.id}, Pts: a.pts, PtsCount: 1}},
			Users: []tl.UserClass{}, Chats: []tl.ChatClass{}, Date: int32(time.Now().Unix())}, nil
	case *tl.MessagesSearchRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		peer, _ := inputUser(r.Peer, a.UserID)
		_, pinnedOnly := r.Filter.(*tl.InputMessagesFilterPinned)
		var from int64
		if f, ok := r.FromId.(*tl.InputPeerUser); ok {
			from = f.UserId
		}
		return a.search(func(m fakeMsg) bool {
			return m.peer == peer && (!pinnedOnly || m.pinned) &&
				(from == 0 || (from == a.UserID) == m.out) && filterMatch(r.Filter, m)
		}, r.Q, r.MinDate, r.OffsetId, r.Limit), nil
	case *tl.MessagesSearchGlobalRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		return a.search(func(fakeMsg) bool { return true }, r.Q, r.MinDate, r.OffsetId, r.Limit), nil
	case *tl.MessagesGetDialogFiltersRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		out := &tl.MessagesDialogFilters{Filters: []tl.DialogFilterClass{&tl.DialogFilterDefault{}}}
		for _, f := range a.Folders {
			df := &tl.DialogFilter{Id: f.ID, Title: &tl.TextWithEntities{Text: f.Title, Entities: []tl.MessageEntityClass{}},
				Contacts: f.Contacts, PinnedPeers: []tl.InputPeerClass{}, ExcludePeers: []tl.InputPeerClass{}}
			for _, id := range f.Include {
				df.IncludePeers = append(df.IncludePeers, &tl.InputPeerUser{UserId: id, AccessHash: a.hashOf(id)})
			}
			if df.IncludePeers == nil {
				df.IncludePeers = []tl.InputPeerClass{}
			}
			out.Filters = append(out.Filters, df)
		}
		return out, nil
	case *tl.UsersGetFullUserRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		u, _ := r.Id.(*tl.InputUser)
		full := &tl.UserFull{Settings: &tl.PeerSettings{}, NotifySettings: &tl.PeerNotifySettings{}}
		if _, self := r.Id.(*tl.InputUserSelf); self {
			full.Id = a.UserID
			about := a.SelfAbout
			full.About = &about
			return &tl.UsersUserFull{FullUser: full, Chats: []tl.ChatClass{}, Users: []tl.UserClass{a.user()}}, nil
		}
		if u != nil {
			full.Id = u.UserId
			for _, c := range a.Contacts {
				if c.ID == u.UserId && c.About != "" {
					about := c.About
					full.About = &about
				}
			}
		}
		return &tl.UsersUserFull{FullUser: full, Chats: []tl.ChatClass{}, Users: a.contactUsers()}, nil
	case *tl.AccountUpdateNotifySettingsRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		if np, ok := r.Peer.(*tl.InputNotifyPeer); ok {
			if c := a.contact(np.Peer); c != nil {
				if s, ok := r.Settings.(*tl.InputPeerNotifySettings); ok && s.MuteUntil != nil {
					c.Muted = *s.MuteUntil > int32(time.Now().Unix())
				}
			}
		}
		return boolTrue{}, nil
	case *tl.FoldersEditPeerFoldersRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		var peers []tl.FolderPeerClass
		for _, fp := range r.FolderPeers {
			if f, ok := fp.(*tl.InputFolderPeer); ok {
				if c := a.contact(f.Peer); c != nil {
					c.Archived = f.FolderId == 1
					peers = append(peers, &tl.FolderPeer{Peer: &tl.PeerUser{UserId: c.ID}, FolderId: f.FolderId})
				}
			}
		}
		a.pts++
		return &tl.Updates{Updates: []tl.UpdateClass{&tl.UpdateFolderPeers{FolderPeers: peers, Pts: a.pts, PtsCount: 1}},
			Users: []tl.UserClass{}, Chats: []tl.ChatClass{}, Date: int32(time.Now().Unix())}, nil
	case *tl.MessagesUpdateDialogFilterRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		for i := range a.Folders {
			if a.Folders[i].ID != r.Id {
				continue
			}
			a.Folders[i].Include = nil
			if f, ok := r.Filter.(*tl.DialogFilter); ok {
				for _, p := range f.IncludePeers {
					if u, ok := p.(*tl.InputPeerUser); ok {
						a.Folders[i].Include = append(a.Folders[i].Include, u.UserId)
					}
				}
			}
		}
		return boolTrue{}, nil
	case *tl.MessagesToggleDialogPinRequest, *tl.MessagesMarkDialogUnreadRequest, *tl.ContactsBlockRequest,
		*tl.ContactsUnblockRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		return boolTrue{}, nil
	case *tl.MessagesDeleteHistoryRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		peer, _ := inputUser(r.Peer, a.UserID)
		for i := range a.msgs {
			if a.msgs[i].peer == peer {
				a.msgs[i].deleted = true
			}
		}
		a.pts++
		return &tl.MessagesAffectedHistory{Pts: a.pts, PtsCount: 1}, nil
	case *tl.MessagesGetMessagesRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		out := &tl.MessagesMessages{Topics: []tl.ForumTopicClass{}, Chats: []tl.ChatClass{}, Users: a.contactUsers()}
		for _, im := range r.Id {
			if x, ok := im.(*tl.InputMessageID); ok {
				if m := a.find(x.Id); m != nil {
					out.Messages = append(out.Messages, m.tl())
				}
			}
		}
		return out, nil
	case *tl.ContactsGetContactsRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		return &tl.ContactsContacts{Contacts: []tl.ContactClass{}, Users: a.contactUsers()[1:]}, nil
	case *tl.ContactsSearchRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		out := &tl.ContactsFound{MyResults: []tl.PeerClass{}, Results: []tl.PeerClass{}, Chats: []tl.ChatClass{}}
		for _, c := range a.Contacts {
			if strings.Contains(strings.ToLower(c.Name), strings.ToLower(r.Q)) {
				name, h := c.Name, c.AccessHash
				out.Users = append(out.Users, &tl.User{Id: c.ID, FirstName: &name, AccessHash: &h})
				out.Results = append(out.Results, &tl.PeerUser{UserId: c.ID})
			}
		}
		if out.Users == nil {
			out.Users = []tl.UserClass{}
		}
		return out, nil
	case *tl.MessagesReadHistoryRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		if p, ok := r.Peer.(*tl.InputPeerUser); ok {
			if a.readMax == nil {
				a.readMax = map[int64]int32{}
			}
			a.readMax[p.UserId] = max(a.readMax[p.UserId], r.MaxId)
		}
		a.pts++
		return &tl.MessagesAffectedMessages{Pts: a.pts, PtsCount: 1}, nil
	case *tl.UploadGetFileRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		data, ok := a.fileBytes(r.Location, r.Offset, r.Limit)
		if !ok {
			return nil, &rpcErr{400, "FILE_ID_INVALID"}
		}
		return &tl.UploadFile{Type: &tl.StorageFilePartial{}, Mtime: 1, Bytes: data}, nil
	case *tl.UploadSaveFilePartRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.savePart(r.FileId, r.FilePart, r.Bytes)
		return boolTrue{}, nil
	case *tl.UploadSaveBigFilePartRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.savePart(r.FileId, r.FilePart, r.Bytes)
		return boolTrue{}, nil
	case *tl.MessagesUploadMediaRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		m, err := a.mediaOf(r.Media)
		if err != nil {
			return nil, err
		}
		a.remember(m)
		return m, nil
	case *tl.MessagesSendMediaRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		peer, err := a.peerOf(r.Peer)
		if err != nil {
			return nil, err
		}
		m, err := a.mediaOf(r.Media)
		if err != nil {
			return nil, err
		}
		u := a.newUpdates()
		if err := a.postMedia(peer, r.Message, m, 0, r.RandomId, u); err != nil {
			return nil, err
		}
		return u, nil
	case *tl.MessagesSendMultiMediaRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.seed()
		peer, err := a.peerOf(r.Peer)
		if err != nil {
			return nil, err
		}
		u := a.newUpdates()
		grouped := time.Now().UnixNano()
		for _, x := range r.MultiMedia {
			s := x.(*tl.InputSingleMedia)
			m, err := a.mediaOf(s.Media)
			if err != nil {
				return nil, err
			}
			if err := a.postMedia(peer, s.Message, m, grouped, s.RandomId, u); err != nil {
				return nil, err
			}
		}
		return u, nil
	case *tl.AccountUpdateProfileRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		if r.FirstName != nil {
			a.FirstName = *r.FirstName
		}
		if r.LastName != nil {
			a.LastName = *r.LastName
		}
		if r.About != nil {
			a.SelfAbout = *r.About
		}
		fmt.Printf("профиль: %s|%s|%s\n", a.FirstName, a.LastName, a.SelfAbout)
		return a.user(), nil
	case *tl.AccountUpdateUsernameRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		if r.Username == "taken" {
			return nil, &rpcErr{400, "USERNAME_OCCUPIED"}
		}
		a.Username = r.Username
		fmt.Printf("имя пользователя: %s\n", a.Username)
		return a.user(), nil
	case *tl.PhotosUploadProfilePhotoRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		data, _, err := a.assemble(r.File)
		if err != nil {
			return nil, err
		}
		a.photoID = a.newFileID()
		a.Files[a.photoID] = data
		return &tl.PhotosPhoto{Photo: &tl.Photo{Id: a.photoID, AccessHash: 1, FileReference: []byte{1}, DcId: 2,
			Sizes: []tl.PhotoSizeClass{}}, Users: []tl.UserClass{a.user()}}, nil
	case *tl.AccountGetAuthorizationsRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		return &tl.AccountAuthorizations{AuthorizationTtlDays: 180, Authorizations: a.authorizations()}, nil
	case *tl.AccountResetAuthorizationRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.authorizations()
		for i, s := range a.sessions {
			if s.Hash == r.Hash && !s.Current {
				a.sessions = append(a.sessions[:i], a.sessions[i+1:]...)
				fmt.Printf("сеансов: %d\n", len(a.sessions))
				break
			}
		}
		return boolTrue{}, nil
	case *tl.AuthResetAuthorizationsRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		a.authorizations()
		a.sessions = a.sessions[:1]
		fmt.Printf("сеансов: %d\n", len(a.sessions))
		return boolTrue{}, nil
	case *tl.MessagesSendVoteRequest:
		if !a.authorized[key] {
			return nil, unauthorized
		}
		m := a.find(r.MsgId)
		if m == nil || !m.vote(r.Options) {
			return nil, &rpcErr{400, "MESSAGE_POLL_CLOSED"}
		}
		return &tl.Updates{Updates: []tl.UpdateClass{}, Users: []tl.UserClass{}, Chats: []tl.ChatClass{},
			Date: int32(time.Now().Unix())}, nil
	case *tl.HelpGetCountriesListRequest:
		ru, en := "Россия", "Russia"
		name := &ru
		if r.LangCode == "en" {
			name = &en
		}
		return &tl.HelpCountriesList{Countries: []tl.HelpCountryClass{
			&tl.HelpCountry{Iso2: "RU", DefaultName: "Russia", Name: name, CountryCodes: []tl.HelpCountryCodeClass{
				&tl.HelpCountryCode{CountryCode: "7", Patterns: []string{"XXX XXX XXXX"}}}},
		}, Hash: 1}, nil
	}
	return nil, &rpcErr{400, "METHOD_NOT_IMPLEMENTED_IN_FAKESERVER"}
}

func (a *Account) loginSuccess() *tl.AuthLoginTokenSuccess {
	return &tl.AuthLoginTokenSuccess{Authorization: a.authorization()}
}

func randomToken() []byte {
	b := make([]byte, 32)
	_, _ = rand.Read(b)
	return b
}

// search — сообщения по подстроке (без учёта регистра), новые первыми.
// filterMatch — фильтр messages.search по типу (общие медиа, поиск по чату).
func filterMatch(f tl.MessagesFilterClass, m fakeMsg) bool {
	var doc *tl.Document
	var md *tl.MessageMediaDocument
	if x, ok := m.media.(*tl.MessageMediaDocument); ok {
		md = x
		doc, _ = x.Document.(*tl.Document)
	}
	attr := func(video bool) bool {
		if doc == nil {
			return false
		}
		for _, a := range doc.Attributes {
			switch at := a.(type) {
			case *tl.DocumentAttributeVideo:
				if video {
					return true
				}
			case *tl.DocumentAttributeAudio:
				if !video && at.Voice {
					return true
				}
			}
		}
		return false
	}
	switch f.(type) {
	case *tl.InputMessagesFilterPhotoVideo:
		_, photo := m.media.(*tl.MessageMediaPhoto)
		return photo || (md != nil && (md.Video || attr(true)) && !md.Round)
	case *tl.InputMessagesFilterDocument:
		return md != nil && !md.Voice && !md.Video && !md.Round && !attr(true) && !attr(false)
	case *tl.InputMessagesFilterUrl:
		if _, ok := m.media.(*tl.MessageMediaWebPage); ok {
			return true
		}
		for _, e := range m.entities {
			switch e.(type) {
			case *tl.MessageEntityUrl, *tl.MessageEntityTextUrl:
				return true
			}
		}
		return false
	case *tl.InputMessagesFilterVoice:
		return md != nil && (md.Voice || attr(false))
	}
	return true
}

func (a *Account) search(match func(fakeMsg) bool, q string, minDate, offsetID, limit int32) *tl.MessagesMessagesSlice {
	q = strings.ToLower(q)
	out := &tl.MessagesMessagesSlice{Topics: []tl.ForumTopicClass{}, Chats: []tl.ChatClass{}, Users: a.contactUsers()}
	for i := len(a.msgs) - 1; i >= 0; i-- {
		m := a.msgs[i]
		if m.deleted || !match(m) || (q != "" && !strings.Contains(strings.ToLower(m.text), q)) || m.date < minDate {
			continue
		}
		out.Count++
		if (offsetID == 0 || m.id < offsetID) && len(out.Messages) < int(limit) {
			out.Messages = append(out.Messages, m.tl())
		}
	}
	return out
}

func (a *Account) contact(p tl.InputPeerClass) *Contact {
	if u, ok := p.(*tl.InputPeerUser); ok {
		for i := range a.Contacts {
			if a.Contacts[i].ID == u.UserId {
				return &a.Contacts[i]
			}
		}
	}
	return nil
}

func (a *Account) hashOf(id int64) int64 {
	for _, c := range a.Contacts {
		if c.ID == id {
			return c.AccessHash
		}
	}
	return 0
}

// boolTrue — boolTrue#997275b5 (Bool в кодеке — встроенный тип, не объект).
type boolTrue struct{}

func (boolTrue) TLID() uint32                      { return 0x997275b5 }
func (boolTrue) TLName() string                    { return "boolTrue" }
func (boolTrue) TLEncodeBare(*tlbin.Encoder) error { return nil }
func (boolTrue) TLDecodeBare(*tlbin.Decoder) error { return nil }

// userVector — Vector<User> как ответ (голый вектор не является Object).
type userVector struct{ users []*tl.User }

func (*userVector) TLID() uint32   { return 0x1cb5c415 }
func (*userVector) TLName() string { return "vector" }
func (v *userVector) TLEncodeBare(e *tlbin.Encoder) error {
	e.PutInt32(int32(len(v.users)))
	for _, u := range v.users {
		if err := tlbin.EncodeBoxed(e, u); err != nil {
			return err
		}
	}
	return nil
}
func (v *userVector) TLDecodeBare(*tlbin.Decoder) error { return fmt.Errorf("не нужен") }

// ---------------------------------------------------------------- SRP (сервер)
//
// По core.telegram.org/api/srp, независимо от ядра: стандартная криптография Go.

type srpState struct {
	id           int64
	salt1, salt2 []byte
	v, b, bBig   *big.Int
	bBytes       []byte
}

func h(parts ...[]byte) []byte {
	s := sha256.New()
	for _, p := range parts {
		s.Write(p)
	}
	return s.Sum(nil)
}

func sh(data, salt []byte) []byte { return h(salt, data, salt) }

func pad256(x *big.Int) []byte { return x.FillBytes(make([]byte, 256)) }

func passwordX(pw string, salt1, salt2 []byte) *big.Int {
	ph1 := sh(sh([]byte(pw), salt1), salt2)
	dk, _ := pbkdf2.Key(sha512.New, string(ph1), salt1, 100000, 64)
	return new(big.Int).SetBytes(sh(dk, salt2))
}

func newSRP(pw string) (*srpState, error) {
	st := &srpState{salt1: make([]byte, 40), salt2: make([]byte, 16)}
	_, _ = rand.Read(st.salt1)
	_, _ = rand.Read(st.salt2)
	var idb [8]byte
	_, _ = rand.Read(idb[:])
	st.id = int64(binary.LittleEndian.Uint64(idb[:]) >> 1)
	g := big.NewInt(3)
	x := passwordX(pw, st.salt1, st.salt2)
	st.v = new(big.Int).Exp(g, x, dhPrime)
	k := new(big.Int).SetBytes(h(pad256(dhPrime), pad256(g)))
	b, err := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 2048))
	if err != nil {
		return nil, err
	}
	st.b = b
	bb := new(big.Int).Mul(k, st.v)
	bb.Add(bb, new(big.Int).Exp(g, b, dhPrime))
	bb.Mod(bb, dhPrime)
	st.bBig, st.bBytes = bb, pad256(bb)
	return st, nil
}

func (st *srpState) check(aBytes, m1 []byte) bool {
	A := new(big.Int).SetBytes(aBytes)
	if A.Sign() <= 0 || A.Cmp(dhPrime) >= 0 {
		return false
	}
	g := big.NewInt(3)
	u := new(big.Int).SetBytes(h(pad256(A), st.bBytes))
	S := new(big.Int).Exp(new(big.Int).Mul(A, new(big.Int).Exp(st.v, u, dhPrime)), st.b, dhPrime)
	K := h(pad256(S))
	hp, hg := h(pad256(dhPrime)), h(pad256(g))
	x := make([]byte, 32)
	for i := range x {
		x[i] = hp[i] ^ hg[i]
	}
	want := h(x, h(st.salt1), h(st.salt2), pad256(A), st.bBytes, K)
	return bytes.Equal(want, m1)
}
