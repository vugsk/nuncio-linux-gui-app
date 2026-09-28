// Package updates — поток обновлений: порядок по pts/seq, дубли, разрывы и
// getDifference (Архитектура/nuncio - Сетевой слой (Go).md, раздел 4).
//
// Одна горутина применяет обновления по очереди, поэтому Rust получает события
// в порядке применения. Каждый обработанный объект Updates даёт одну пачку
// (UpdatesBatch) с новым состоянием: Rust пишет события и состояние одной
// транзакцией (О-5), а на диск Go ничего не пишет.
//
// Общий pts и seq, getDifference (срезы и TooLong); у каждого канала свой pts
// и getChannelDifference (channels.go). Разрыв не сразу ведёт к разнице:
// обновление откладывается на GapWait — недостающие часто приходят следом.
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
	u        tl.UpdatesClass
	diff     bool
	gapCheck bool // истекло ожидание недостающих обновлений
	done     chan struct{}
	sent     *pb.Message // своё отправленное: для updateShortSentMessage (текст у отправителя)
}

// GapWait — сколько ждать недостающие обновления перед getDifference.
var GapWait = 500 * time.Millisecond

type stashed struct {
	u    tl.UpdatesClass
	sent *pb.Message
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

	chans    map[int64]int32 // помеченный id канала -> pts
	hashes   map[int64]int64 // помеченный id канала -> access_hash
	stash    []stashed       // обновления после разрыва, ждут недостающих
	gapTimer *time.Timer
}

// New создаёт остановленный менеджер.
func New(cfg Config) *Manager {
	if cfg.Log == nil {
		cfg.Log = func(string, ...any) {}
	}
	return &Manager{cfg: cfg, sentIDs: map[int64]int32{}, chans: map[int64]int32{}, hashes: map[int64]int64{}}
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
	m.chans = map[int64]int32{}
	m.hashes = map[int64]int64{}
	m.stash = nil
	if m.gapTimer != nil {
		m.gapTimer.Stop()
	}
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
			switch {
			case j.diff:
				m.difference(ctx)
			case j.gapCheck:
				m.gapExpired(ctx)
			default:
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
	b         pb.UpdatesBatch
	needDiff  bool // разрыв общего pts или seq
	changed   bool
	progress  bool // общий pts сдвинулся
	chanGaps  []int64
	chanDirty map[int64]bool
}

func (b *batch) add(u *pb.DomainUpdate) { b.b.Updates = append(b.b.Updates, u) }

func (m *Manager) flush(b *batch) {
	if len(b.b.Updates) == 0 && len(b.b.Peers) == 0 && !b.changed {
		return
	}
	b.b.State = m.State()
	m.mu.Lock()
	for id := range b.chanDirty {
		b.b.ChannelStates = append(b.b.ChannelStates, &pb.ChannelState{PeerId: id, Pts: m.chans[id]})
	}
	m.mu.Unlock()
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
		b.changed, b.progress = true, true
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
	if _, ok := u.(*tl.UpdatesTooLong); ok {
		m.difference(ctx)
		return
	}
	gap, progress := m.applyOnce(ctx, u, sent)
	if gap {
		// Разрыв: ждём недостающие GapWait, потом — getDifference.
		m.mu.Lock()
		m.stash = append(m.stash, stashed{u, sent})
		if m.gapTimer == nil || len(m.stash) == 1 {
			m.gapTimer = time.AfterFunc(GapWait, func() { m.enqueue(job{gapCheck: true}) })
		}
		m.mu.Unlock()
		return
	}
	if progress {
		m.retryStash(ctx)
	}
}

// retryStash — пришли недостающие: применить отложенные по порядку (pts защищает от повторов).
func (m *Manager) retryStash(ctx context.Context) {
	for {
		m.mu.Lock()
		st := m.stash
		m.stash = nil
		m.mu.Unlock()
		if len(st) == 0 {
			return
		}
		moved := false
		var left []stashed
		for _, x := range st {
			gap, progress := m.applyOnce(ctx, x.u, x.sent)
			if gap {
				left = append(left, x)
			}
			moved = moved || progress
		}
		m.mu.Lock()
		m.stash = append(left, m.stash...)
		m.mu.Unlock()
		if !moved || len(left) == 0 {
			return
		}
	}
}

func (m *Manager) gapExpired(ctx context.Context) {
	m.retryStash(ctx)
	m.mu.Lock()
	pending := len(m.stash) > 0
	m.stash = nil
	m.mu.Unlock()
	if pending {
		m.cfg.Log("updates: разрыв pts не закрылся, getDifference")
		m.difference(ctx)
	}
}

// applyOnce применяет объект Updates; возвращает, был ли разрыв общего pts/seq
// и сдвинулся ли pts. Разрывы каналов закрываются здесь же (getChannelDifference).
func (m *Manager) applyOnce(ctx context.Context, u tl.UpdatesClass, sent *pb.Message) (gap, progress bool) {
	b := &batch{}
	switch v := u.(type) {
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
			// Сервер дописывает найденное сам (ссылки, упоминания): объединяем без повторов.
			for _, e := range conv.Entities(v.Entities) {
				dup := false
				for _, o := range sent.Entities {
					dup = dup || (o.GetKind() == e.GetKind() && o.GetOffset() == e.GetOffset() && o.GetLength() == e.GetLength())
				}
				if !dup {
					sent.Entities = append(sent.Entities, e)
				}
			}
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
		return false, false
	}
	m.flush(b)
	for _, ch := range b.chanGaps {
		m.channelDifference(ctx, ch)
	}
	return b.needDiff, b.progress
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
	peers := conv.Peers(users, chats)
	m.RememberPeers(peers)
	b.b.Peers = append(b.b.Peers, peers...)
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
	case *tl.UpdatePinnedMessages:
		if pts(v.Pts, v.PtsCount) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_Pinned{Pinned: &pb.Pinned{PeerId: conv.PeerID(v.Peer),
				Ids: v.Messages, Pinned: v.Pinned}}})
		}
	case *tl.UpdatePinnedChannelMessages:
		ch := conv.ChannelID(v.ChannelId)
		if m.channelPts(b, ch, v.Pts, v.PtsCount, check) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_Pinned{Pinned: &pb.Pinned{PeerId: ch, Ids: v.Messages,
				Pinned: v.Pinned}}})
		}
	case *tl.UpdateReadMessagesContents, *tl.UpdateWebPage, *tl.UpdateFolderPeers:
		// Содержательно — В5–В7; pts учитывается, чтобы не было ложных разрывов.
		if check {
			var p, c int32
			switch x := v.(type) {
			case *tl.UpdateReadMessagesContents:
				p, c = x.Pts, x.PtsCount
			case *tl.UpdateWebPage:
				p, c = x.Pts, x.PtsCount
			case *tl.UpdateFolderPeers:
				p, c = x.Pts, x.PtsCount
			}
			m.checkPts(b, p, c)
		}
	// Каналы: свои pts; разрыв — getChannelDifference после пачки.
	case *tl.UpdateNewChannelMessage:
		if msg := conv.Message(v.Message); msg != nil && m.channelPts(b, msg.PeerId, v.Pts, v.PtsCount, check) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: msg}})
		}
	case *tl.UpdateEditChannelMessage:
		if msg := conv.Message(v.Message); msg != nil && m.channelPts(b, msg.PeerId, v.Pts, v.PtsCount, check) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_EditMessage{EditMessage: msg}})
		}
	case *tl.UpdateDeleteChannelMessages:
		ch := conv.ChannelID(v.ChannelId)
		if m.channelPts(b, ch, v.Pts, v.PtsCount, check) {
			b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_DeleteMessages{DeleteMessages: &pb.DeleteMessages{
				PeerId: ch, Ids: v.Messages}}})
		}
	case *tl.UpdateReadChannelInbox:
		ch := conv.ChannelID(v.ChannelId)
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ReadInbox{ReadInbox: &pb.ReadInbox{
			PeerId: ch, MaxId: v.MaxId, StillUnread: v.StillUnreadCount}}})
		m.setChannelPts(b, ch, v.Pts)
	case *tl.UpdateReadChannelOutbox:
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_ReadOutbox{ReadOutbox: &pb.ReadOutbox{
			PeerId: conv.ChannelID(v.ChannelId), MaxId: v.MaxId}}})
	case *tl.UpdateChannelTooLong:
		b.chanGaps = append(b.chanGaps, conv.ChannelID(v.ChannelId))
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
		for _, ch := range b.chanGaps {
			m.channelDifference(ctx, ch)
		}
		if !more {
			return
		}
	}
}

func (m *Manager) diffContent(b *batch, msgs []tl.MessageClass, other []tl.UpdateClass, users []tl.UserClass,
	chats []tl.ChatClass) {
	peers := conv.Peers(users, chats)
	m.RememberPeers(peers)
	b.b.Peers = append(b.b.Peers, peers...)
	for _, msg := range conv.Messages(msgs) {
		b.add(&pb.DomainUpdate{Body: &pb.DomainUpdate_NewMessage{NewMessage: msg}})
	}
	for _, u := range other {
		m.update(b, u, false)
	}
}
