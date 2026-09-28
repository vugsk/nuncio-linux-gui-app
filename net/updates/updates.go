// Package updates — поток обновлений: порядок по pts/seq, дубли, разрывы и
// getDifference (Архитектура/nuncio - Сетевой слой (Go).md, раздел 4).
//
// Одна горутина применяет обновления по очереди, поэтому Rust получает события
// в порядке применения. Каждый обработанный объект Updates даёт одну пачку
// (UpdatesBatch) с новым состоянием: Rust пишет события и состояние одной
// транзакцией (О-5), а на диск Go ничего не пишет.
//
// Веха В4: общий pts, seq, getDifference (включая differenceSlice и
// differenceTooLong). Разрывы pts каналов (getChannelDifference) и буфер
// ожидания перед getDifference — веха В5.
package updates

import (
	"context"
	"sync"
	"time"

	"nuncio/net/conv"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// Invoker — выполнение запросов (dc.Manager).
type Invoker interface {
	Invoke(ctx context.Context, req tlbin.Request) (any, error)
}

// Config — зависимости.
type Config struct {
	Inv  Invoker
	Emit func(*pb.UpdatesBatch)
	Log  func(format string, args ...any)
	// SelfID — id вошедшего пользователя (для updateShortMessage с out).
	SelfID func() int64
}

// state — pts/qts/seq/date (не protobuf: его нельзя копировать по значению).
type state struct {
	Pts, Qts, Seq int32
	Date          int64
}

type job struct {
	u    tl.UpdatesClass
	diff bool
	done chan struct{}
	sent *pb.Message // своё отправленное: для updateShortSentMessage (текст у отправителя)
}

// Manager — очередь и состояние обновлений одного аккаунта.
type Manager struct {
	cfg Config

	mu      sync.Mutex
	st      state
	running bool
	cancel  context.CancelFunc
	jobs    chan job
	sentIDs map[int64]int32 // random_id -> id (updateMessageID)
}

// New создаёт остановленный менеджер.
func New(cfg Config) *Manager {
	if cfg.Log == nil {
		cfg.Log = func(string, ...any) {}
	}
	return &Manager{cfg: cfg, sentIDs: map[int64]int32{}}
}

// State — текущее состояние.
func (m *Manager) State() *pb.UpdateState {
	m.mu.Lock()
	defer m.mu.Unlock()
	return &pb.UpdateState{Pts: m.st.Pts, Qts: m.st.Qts, Seq: m.st.Seq, Date: m.st.Date}
}

// Start запускает поток с состоянием из базы Rust. Пустое состояние —
// updates.getState (новый вход); иначе — getDifference от сохранённого.
func (m *Manager) Start(st *pb.UpdateState) {
	m.mu.Lock()
	if m.running {
		m.mu.Unlock()
		return
	}
	ctx, cancel := context.WithCancel(context.Background())
	m.running, m.cancel = true, cancel
	m.jobs = make(chan job, 1024)
	if st != nil {
		m.st = state{Pts: st.GetPts(), Qts: st.GetQts(), Seq: st.GetSeq(), Date: st.GetDate()}
	}
	jobs := m.jobs
	m.mu.Unlock()
	go m.loop(ctx, jobs)
	m.enqueue(job{diff: true})
}

// Stop останавливает поток и забывает состояние (выход из аккаунта).
func (m *Manager) Stop() {
	m.mu.Lock()
	defer m.mu.Unlock()
	if !m.running {
		return
	}
	m.cancel()
	m.running = false
	m.st = state{}
}

func (m *Manager) enqueue(j job) bool {
	m.mu.Lock()
	jobs, running := m.jobs, m.running
	m.mu.Unlock()
	if !running {
		return false
	}
	select {
	case jobs <- j:
		return true
	default:
		// Очередь переполнена: вместо потерь — getDifference.
		m.cfg.Log("updates: очередь переполнена, будет getDifference")
		select {
		case jobs <- job{diff: true}:
		default:
		}
		return false
	}
}

// Push — обновление от сервера (push). Не блокирует.
func (m *Manager) Push(u tl.UpdatesClass) { m.enqueue(job{u: u}) }

// Difference — запросить разницу (переподключение, new_session_created).
func (m *Manager) Difference() { m.enqueue(job{diff: true}) }

// Process — обработать ответ метода по порядку и дождаться. sent — своё
// отправленное сообщение (id и дата подставятся из updateShortSentMessage).
func (m *Manager) Process(ctx context.Context, u tl.UpdatesClass, sent *pb.Message) {
	done := make(chan struct{})
	if !m.enqueue(job{u: u, done: done, sent: sent}) {
		return
	}
	select {
	case <-done:
	case <-ctx.Done():
	}
}

// SentID — id сообщения по random_id (из updateMessageID); 0 — не пришёл.
func (m *Manager) SentID(randomID int64) int32 {
	m.mu.Lock()
	defer m.mu.Unlock()
	id := m.sentIDs[randomID]
	delete(m.sentIDs, randomID)
	return id
}

func (m *Manager) loop(ctx context.Context, jobs chan job) {
	for {
		select {
		case <-ctx.Done():
			return
		case j := <-jobs:
			if j.diff {
				m.difference(ctx)
			} else {
				m.apply(ctx, j.u, j.sent)
			}
			if j.done != nil {
				close(j.done)
			}
		}
	}
}

// batch собирает события одного объекта Updates.
type batch struct {
	b        pb.UpdatesBatch
	needDiff bool
	changed  bool
}

func (b *batch) add(u *pb.DomainUpdate) { b.b.Updates = append(b.b.Updates, u) }

func (m *Manager) flush(b *batch) {
	if len(b.b.Updates) == 0 && len(b.b.Peers) == 0 && !b.changed {
		return
	}
	b.b.State = m.State()
	if m.cfg.Emit != nil {
		m.cfg.Emit(&b.b)
	}
}

// checkPts — правило из документации: local+count == pts — применить;
// больше — дубль; меньше — разрыв.
func (m *Manager) checkPts(b *batch, pts, count int32) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	switch {
	case m.st.Pts == 0:
		m.st.Pts = pts
		b.changed = true
		return true
	case m.st.Pts+count == pts:
		m.st.Pts = pts
		b.changed = true
		return true
	case m.st.Pts+count > pts:
		return false // уже применено
	default:
		b.needDiff = true
		return false
	}
}

func (m *Manager) setDate(b *batch, date int32) {
	m.mu.Lock()
	if int64(date) > m.st.Date {
		m.st.Date = int64(date)
		b.changed = true
	}
	m.mu.Unlock()
}

func (m *Manager) self() int64 {
	if m.cfg.SelfID != nil {
		return m.cfg.SelfID()
	}
	return 0
}

func (m *Manager) apply(ctx context.Context, u tl.UpdatesClass, sent *pb.Message) {
	b := &batch{}
	switch v := u.(type) {
	case *tl.UpdatesTooLong:
		m.difference(ctx)
		return
	case *tl.UpdateShortMessage:
		if m.checkPts(b, v.Pts, v.PtsCount) {
			from := v.UserId
			if v.Out {
				from = m.self()
			}
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: &pb.Message{PeerId: v.UserId,
				Id: v.Id, Date: int64(v.Date), FromId: from, Out: v.Out, Text: v.Message,
				ReplyTo: replyTo(v.ReplyTo)}}})
		}
		m.setDate(b, v.Date)
	case *tl.UpdateShortChatMessage:
		if m.checkPts(b, v.Pts, v.PtsCount) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: &pb.Message{
				PeerId: conv.ChatID(v.ChatId), Id: v.Id, Date: int64(v.Date), FromId: v.FromId, Out: v.Out,
				Text: v.Message, ReplyTo: replyTo(v.ReplyTo)}}})
		}
		m.setDate(b, v.Date)
	case *tl.UpdateShortSentMessage:
		// Сообщение строится из запроса: текст у отправителя, id и дата — от сервера.
		if m.checkPts(b, v.Pts, v.PtsCount) && sent != nil {
			sent.Id, sent.Date, sent.Out = v.Id, int64(v.Date), true
			sent.Media = mediaKindShort(v.Media)
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: sent}})
		}
		m.setDate(b, v.Date)
	case *tl.UpdateShort:
		m.update(b, v.Update, true)
		m.setDate(b, v.Date)
	case *tl.Updates:
		m.combined(ctx, b, v.Updates, v.Users, v.Chats, v.Date, v.Seq, v.Seq)
	case *tl.UpdatesCombined:
		m.combined(ctx, b, v.Updates, v.Users, v.Chats, v.Date, v.SeqStart, v.Seq)
	default:
		return
	}
	m.flush(b)
	if b.needDiff {
		m.cfg.Log("updates: разрыв pts, getDifference")
		m.difference(ctx)
	}
}

func (m *Manager) combined(ctx context.Context, b *batch, ups []tl.UpdateClass, users []tl.UserClass,
	chats []tl.ChatClass, date, seqStart, seq int32) {
	if seq != 0 {
		m.mu.Lock()
		local := m.st.Seq
		m.mu.Unlock()
		switch {
		case local != 0 && seqStart > local+1:
			b.needDiff = true // разрыв seq
			return
		case local != 0 && seqStart <= local:
			return // уже применено
		}
	}
	b.b.Peers = append(b.b.Peers, conv.Peers(users, chats)...)
	for _, u := range ups {
		m.update(b, u, true)
	}
	if seq != 0 {
		m.mu.Lock()
		m.st.Seq = seq
		m.mu.Unlock()
		b.changed = true
	}
	m.setDate(b, date)
	_ = ctx
}

func mediaKindShort(m tl.MessageMediaClass) string {
	if m == nil {
		return ""
	}
	if _, ok := m.(*tl.MessageMediaWebPage); ok {
		return ""
	}
	return "other"
}

func replyTo(r tl.MessageReplyHeaderClass) int32 {
	if h, ok := r.(*tl.MessageReplyHeader); ok && h.ReplyToMsgId != nil {
		return *h.ReplyToMsgId
	}
	return 0
}

// update — одно обновление; check=false — из getDifference (pts уже учтён в state).
func (m *Manager) update(b *batch, u tl.UpdateClass, check bool) {
	pts := func(p, c int32) bool { return !check || m.checkPts(b, p, c) }
	switch v := u.(type) {
	case *tl.UpdateNewMessage:
		if pts(v.Pts, v.PtsCount) {
			if msg := conv.Message(v.Message); msg != nil {
				b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: msg}})
			}
		}
	case *tl.UpdateEditMessage:
		if pts(v.Pts, v.PtsCount) {
			if msg := conv.Message(v.Message); msg != nil {
				b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_EditMessage{EditMessage: msg}})
			}
		}
	case *tl.UpdateDeleteMessages:
		if pts(v.Pts, v.PtsCount) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_DeleteMessages{DeleteMessages: &pb.DeleteMessages{
				Ids: v.Messages}}})
		}
	case *tl.UpdateReadHistoryInbox:
		if pts(v.Pts, v.PtsCount) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ReadInbox{ReadInbox: &pb.ReadInbox{
				PeerId: conv.PeerID(v.Peer), MaxId: v.MaxId, StillUnread: v.StillUnreadCount}}})
		}
	case *tl.UpdateReadHistoryOutbox:
		if pts(v.Pts, v.PtsCount) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ReadOutbox{ReadOutbox: &pb.ReadOutbox{
				PeerId: conv.PeerID(v.Peer), MaxId: v.MaxId}}})
		}
	case *tl.UpdateReadMessagesContents, *tl.UpdateWebPage, *tl.UpdatePinnedMessages, *tl.UpdateFolderPeers:
		// Содержательно — В5–В7; pts учитывается, чтобы не было ложных разрывов.
		if check {
			var p, c int32
			switch x := v.(type) {
			case *tl.UpdateReadMessagesContents:
				p, c = x.Pts, x.PtsCount
			case *tl.UpdateWebPage:
				p, c = x.Pts, x.PtsCount
			case *tl.UpdatePinnedMessages:
				p, c = x.Pts, x.PtsCount
			case *tl.UpdateFolderPeers:
				p, c = x.Pts, x.PtsCount
			}
			m.checkPts(b, p, c)
		}
	// Каналы: свои pts (В5 — getChannelDifference); пока применяются как есть.
	case *tl.UpdateNewChannelMessage:
		if msg := conv.Message(v.Message); msg != nil {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: msg}})
		}
	case *tl.UpdateEditChannelMessage:
		if msg := conv.Message(v.Message); msg != nil {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_EditMessage{EditMessage: msg}})
		}
	case *tl.UpdateDeleteChannelMessages:
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_DeleteMessages{DeleteMessages: &pb.DeleteMessages{
			PeerId: conv.ChannelID(v.ChannelId), Ids: v.Messages}}})
	case *tl.UpdateReadChannelInbox:
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ReadInbox{ReadInbox: &pb.ReadInbox{
			PeerId: conv.ChannelID(v.ChannelId), MaxId: v.MaxId, StillUnread: v.StillUnreadCount}}})
	case *tl.UpdateReadChannelOutbox:
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ReadOutbox{ReadOutbox: &pb.ReadOutbox{
			PeerId: conv.ChannelID(v.ChannelId), MaxId: v.MaxId}}})
	case *tl.UpdateMessageID:
		m.mu.Lock()
		m.sentIDs[v.RandomId] = v.Id
		if len(m.sentIDs) > 1024 {
			m.sentIDs = map[int64]int32{v.RandomId: v.Id}
		}
		m.mu.Unlock()
	}
}

// difference — updates.getDifference до конца (differenceSlice — по кускам).
func (m *Manager) difference(ctx context.Context) {
	for i := 0; i < 1000 && ctx.Err() == nil; i++ {
		st := m.State()
		dctx, cancel := context.WithTimeout(ctx, 60*time.Second)
		var v any
		var err error
		if st.GetPts() == 0 {
			v, err = m.cfg.Inv.Invoke(dctx, &tl.UpdatesGetStateRequest{})
		} else {
			v, err = m.cfg.Inv.Invoke(dctx, &tl.UpdatesGetDifferenceRequest{Pts: st.GetPts(), Date: int32(st.GetDate()),
				Qts: st.GetQts()})
		}
		cancel()
		if err != nil {
			m.cfg.Log("updates: getDifference: %T", err)
			return // повторит переподключение или следующий разрыв
		}
		b := &batch{changed: true}
		setState := func(s tl.UpdatesStateClass) {
			if x, ok := s.(*tl.UpdatesState); ok {
				m.mu.Lock()
				m.st = state{Pts: x.Pts, Qts: x.Qts, Seq: x.Seq, Date: int64(x.Date)}
				m.mu.Unlock()
			}
		}
		more := false
		switch d := v.(type) {
		case *tl.UpdatesState:
			setState(d)
		case *tl.UpdatesDifferenceEmpty:
			m.mu.Lock()
			m.st.Date, m.st.Seq = int64(d.Date), d.Seq
			m.mu.Unlock()
		case *tl.UpdatesDifference:
			m.diffContent(b, d.NewMessages, d.OtherUpdates, d.Users, d.Chats)
			setState(d.State)
		case *tl.UpdatesDifferenceSlice:
			m.diffContent(b, d.NewMessages, d.OtherUpdates, d.Users, d.Chats)
			setState(d.IntermediateState)
			more = true
		case *tl.UpdatesDifferenceTooLong:
			m.mu.Lock()
			m.st.Pts = d.Pts
			m.mu.Unlock()
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_Reset_{Reset_: &pb.ResetDialogs{}}})
		}
		m.flush(b)
		if !more {
			return
		}
	}
}

func (m *Manager) diffContent(b *batch, msgs []tl.MessageClass, other []tl.UpdateClass, users []tl.UserClass,
	chats []tl.ChatClass) {
	b.b.Peers = append(b.b.Peers, conv.Peers(users, chats)...)
	for _, msg := range conv.Messages(msgs) {
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: msg}})
	}
	for _, u := range other {
		m.update(b, u, false)
	}
}
