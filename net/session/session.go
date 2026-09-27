// Package session — зашифрованная сессия MTProto 2.0 поверх одного соединения
// (core.telegram.org/mtproto/description, service_messages): msg_id и seq_no,
// соль, подтверждения, контейнеры, rpc_result, gzip_packed, служебные
// уведомления сервера, ping. Шифрование — в ядре (net/core), TL — net/tl.
//
// Повторы после разрыва и FLOOD_WAIT — забота пакета rpc; выбор DC — dc.
package session

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"sync"
	"time"

	"nuncio/net/core"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
	"nuncio/net/transport"
)

// Идентификаторы конструкторов, разбираемых вручную («parsed manually» в схеме).
const (
	idRPCResult    uint32 = 0xf35c6d01
	idMsgContainer uint32 = 0x73f1f8dc
	idGzipPacked   uint32 = 0x3072cfa1
	idRPCError     uint32 = 0x2144ca19
)

// Пределы.
const (
	maxGunzip       = 16 << 20 // защита от «gzip-бомбы»
	msgTimePast     = 300 * time.Second
	msgTimeFuture   = 30 * time.Second
	seenLimit       = 2048
	ackFlushDelay   = 200 * time.Millisecond
	ackMaxBatch     = 64
	pingInterval    = 60 * time.Second
	disconnectDelay = 75 // секунд: сервер закроет соединение, если пингов не будет
)

// RPCError — ответ rpc_error.
type RPCError struct {
	Code    int32
	Message string
}

func (e *RPCError) Error() string { return fmt.Sprintf("rpc_error %d: %s", e.Code, e.Message) }

// BadMsgError — сервер отверг сообщение (bad_msg_notification) с кодом, который
// не лечится повтором.
type BadMsgError struct{ Code int32 }

func (e *BadMsgError) Error() string { return fmt.Sprintf("bad_msg_notification: код %d", e.Code) }

// ErrClosed — сессия закрыта (разрыв соединения или Close).
var ErrClosed = errors.New("session: закрыта")

// Config — параметры сессии.
type Config struct {
	Conn       *transport.Conn
	Key        *core.Key
	Salt       int64
	TimeOffset time.Duration
	// OnUpdate получает тела объектов, которые не относятся к служебным сообщениям
	// и ответам (обновления: updates, updateShort ...). Вызывается из горутины чтения.
	OnUpdate func(body []byte)
	// OnNewSession — сервер создал новую сессию (возможны пропущенные обновления).
	OnNewSession func()
	// DisablePing — не посылать ping (для тестов).
	DisablePing bool
}

type result struct {
	v   any
	err error
}

type pending struct {
	req  tlbin.Request
	body []byte
	done chan result
}

// Session — одна зашифрованная сессия.
type Session struct {
	cfg       Config
	sessionID int64

	mu         sync.Mutex
	salt       int64
	offset     time.Duration
	lastMsgID  int64
	seqCount   int32
	pending    map[int64]*pending
	acks       []int64
	ackTimer   *time.Timer
	seen       map[int64]struct{}
	seenOrder  []int64
	containers map[int64][]int64 // id контейнера -> id вложенных запросов (для повторов)
	contOrder  []int64
	closed     bool
	closeErr   error
	done       chan struct{}
	pingCancel context.CancelFunc
}

// New запускает сессию: горутину чтения и (если не отключён) ping.
func New(cfg Config) *Session {
	var sid int64
	_ = binary.Read(bytes.NewReader(random(8)), binary.LittleEndian, &sid)
	s := &Session{
		cfg: cfg, sessionID: sid, salt: cfg.Salt, offset: cfg.TimeOffset,
		pending: map[int64]*pending{}, seen: map[int64]struct{}{}, containers: map[int64][]int64{},
		done: make(chan struct{}),
	}
	go s.readLoop()
	if !cfg.DisablePing {
		ctx, cancel := context.WithCancel(context.Background())
		s.pingCancel = cancel
		go s.pingLoop(ctx)
	}
	return s
}

// ID — идентификатор сессии.
func (s *Session) ID() int64 { return s.sessionID }

// Done закрывается, когда сессия перестала работать; Err — причина.
func (s *Session) Done() <-chan struct{} { return s.done }

func (s *Session) Err() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closeErr
}

// Salt — текущая соль (для сохранения вместе с ключом).
func (s *Session) Salt() int64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.salt
}

// nextMsgID: время (с поправкой на сервер) * 2^32, кратно 4, строго возрастает.
func nextMsgID(now time.Time, last int64) int64 {
	sec := now.Unix()
	frac := int64(uint64(now.Nanosecond()) << 32 / 1_000_000_000)
	id := (sec<<32 | frac) &^ 3
	if id <= last {
		id = last + 4
	}
	return id
}

// allocLocked выдаёт msg_id и seq_no (content — «содержательное» сообщение).
func (s *Session) allocLocked(content bool) (int64, int32) {
	id := nextMsgID(time.Now().Add(s.offset), s.lastMsgID)
	s.lastMsgID = id
	seq := s.seqCount * 2
	if content {
		seq++
		s.seqCount++
	}
	return id, seq
}

type outMsg struct {
	id   int64
	seq  int32
	body []byte
}

// sendLocked шифрует и отправляет одно сообщение или контейнер (вызывать под s.mu).
func (s *Session) sendLocked(msgs []outMsg) error {
	var id int64
	var seq int32
	var body []byte
	if len(msgs) == 1 {
		id, seq, body = msgs[0].id, msgs[0].seq, msgs[0].body
	} else {
		var e tlbin.Encoder
		e.PutID(idMsgContainer)
		e.PutInt32(int32(len(msgs)))
		for _, m := range msgs {
			e.PutInt64(m.id)
			e.PutInt32(m.seq)
			e.PutInt32(int32(len(m.body)))
			e.PutRaw(m.body)
		}
		body = e.Bytes()
		id, seq = s.allocLocked(false) // контейнер получает id больше вложенных
		var inner []int64
		for _, m := range msgs {
			if _, ok := s.pending[m.id]; ok {
				inner = append(inner, m.id)
			}
		}
		if len(inner) > 0 {
			s.containers[id] = inner
			s.contOrder = append(s.contOrder, id)
			if len(s.contOrder) > 256 {
				delete(s.containers, s.contOrder[0])
				s.contOrder = s.contOrder[1:]
			}
		}
	}
	wire, err := s.cfg.Key.Encrypt(core.EnvHeader{Salt: uint64(s.salt), SessionID: uint64(s.sessionID),
		MsgID: uint64(id), SeqNo: uint32(seq)}, body)
	if err != nil {
		return err
	}
	return s.cfg.Conn.Send(wire)
}

// takeAcksLocked забирает накопленные подтверждения как сообщение msgs_ack.
func (s *Session) takeAcksLocked() (outMsg, bool) {
	if len(s.acks) == 0 {
		return outMsg{}, false
	}
	body, _ := tlbin.Marshal(&tl.MsgsAck{MsgIds: s.acks})
	s.acks = nil
	if s.ackTimer != nil {
		s.ackTimer.Stop()
		s.ackTimer = nil
	}
	id, seq := s.allocLocked(false)
	return outMsg{id, seq, body}, true
}

// Invoke отправляет запрос и ждёт ответ. Ошибка сервера — *RPCError.
func (s *Session) Invoke(ctx context.Context, req tlbin.Request) (any, error) {
	body, err := tlbin.Marshal(req)
	if err != nil {
		return nil, err
	}
	p := &pending{req: req, body: body, done: make(chan result, 1)}
	s.mu.Lock()
	if s.closed {
		err := s.closeErr
		s.mu.Unlock()
		return nil, err
	}
	var msgs []outMsg
	if ack, ok := s.takeAcksLocked(); ok {
		msgs = append(msgs, ack)
	}
	id, seq := s.allocLocked(true)
	msgs = append(msgs, outMsg{id, seq, body})
	s.pending[id] = p
	err = s.sendLocked(msgs)
	if err != nil {
		delete(s.pending, id)
	}
	s.mu.Unlock()
	if err != nil {
		s.fail(err)
		return nil, err
	}
	select {
	case r := <-p.done:
		return r.v, r.err
	case <-ctx.Done():
		s.mu.Lock()
		for k, v := range s.pending {
			if v == p {
				delete(s.pending, k)
			}
		}
		s.mu.Unlock()
		return nil, ctx.Err()
	case <-s.done:
		return nil, s.Err()
	}
}

// resendLocked повторяет запрос с новым msg_id (после смены соли или времени).
func (s *Session) resendLocked(oldID int64) {
	if inner, ok := s.containers[oldID]; ok {
		// Сервер отверг контейнер целиком: повторяем вложенные запросы.
		delete(s.containers, oldID)
		for _, id := range inner {
			s.resendLocked(id)
		}
		return
	}
	p, ok := s.pending[oldID]
	if !ok {
		return
	}
	delete(s.pending, oldID)
	id, seq := s.allocLocked(true)
	s.pending[id] = p
	if err := s.sendLocked([]outMsg{{id, seq, p.body}}); err != nil {
		delete(s.pending, id)
		p.done <- result{err: err}
	}
}

func (s *Session) resolveLocked(reqID int64, r result) {
	if p, ok := s.pending[reqID]; ok {
		delete(s.pending, reqID)
		p.done <- r
	}
}

// Close останавливает сессию и закрывает соединение.
func (s *Session) Close() { s.fail(ErrClosed) }

func (s *Session) fail(err error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return
	}
	s.closed = true
	s.closeErr = err
	for id, p := range s.pending {
		p.done <- result{err: err}
		delete(s.pending, id)
	}
	if s.ackTimer != nil {
		s.ackTimer.Stop()
	}
	if s.pingCancel != nil {
		s.pingCancel()
	}
	close(s.done)
	s.mu.Unlock()
	_ = s.cfg.Conn.Close()
}

// ---------------------------------------------------------------- приём

func (s *Session) readLoop() {
	for {
		pkt, err := s.cfg.Conn.Recv()
		if err != nil {
			s.fail(fmt.Errorf("session: соединение: %w", err))
			return
		}
		h, body, err := s.cfg.Key.Decrypt(pkt)
		if err != nil {
			continue // чужой или испорченный пакет: не отвечаем и не падаем
		}
		if int64(h.SessionID) != s.sessionID {
			continue
		}
		s.handle(int64(h.MsgID), int32(h.SeqNo), body, true)
	}
}

// acceptID проверяет msg_id входящего сообщения: нечётный (от сервера), в окне
// времени, не повтор.
func (s *Session) acceptIDLocked(id int64) bool {
	if id%2 == 0 {
		return false
	}
	msgTime := time.Unix(id>>32, 0)
	now := time.Now().Add(s.offset)
	if msgTime.Before(now.Add(-msgTimePast)) || msgTime.After(now.Add(msgTimeFuture)) {
		return false
	}
	if _, dup := s.seen[id]; dup {
		return false
	}
	s.seen[id] = struct{}{}
	s.seenOrder = append(s.seenOrder, id)
	if len(s.seenOrder) > seenLimit {
		delete(s.seen, s.seenOrder[0])
		s.seenOrder = s.seenOrder[1:]
	}
	return true
}

func (s *Session) ackLocked(id int64) {
	s.acks = append(s.acks, id)
	if len(s.acks) >= ackMaxBatch {
		s.flushAcksLocked()
		return
	}
	if s.ackTimer == nil {
		s.ackTimer = time.AfterFunc(ackFlushDelay, func() {
			s.mu.Lock()
			defer s.mu.Unlock()
			s.ackTimer = nil
			if !s.closed {
				s.flushAcksLocked()
			}
		})
	}
}

func (s *Session) flushAcksLocked() {
	if m, ok := s.takeAcksLocked(); ok {
		_ = s.sendLocked([]outMsg{m})
	}
}

func gunzip(b []byte) ([]byte, error) {
	r, err := gzip.NewReader(bytes.NewReader(b))
	if err != nil {
		return nil, err
	}
	out, err := io.ReadAll(io.LimitReader(r, maxGunzip+1))
	if err != nil {
		return nil, err
	}
	if len(out) > maxGunzip {
		return nil, errors.New("session: распакованные данные больше предела")
	}
	return out, nil
}

// handle разбирает одно входящее сообщение (top — не внутри контейнера).
func (s *Session) handle(id int64, seq int32, body []byte, top bool) {
	s.mu.Lock()
	ok := s.acceptIDLocked(id)
	if ok && seq%2 == 1 {
		s.ackLocked(id)
	}
	s.mu.Unlock()
	if !ok {
		return
	}
	d := tlbin.NewDecoder(body)
	cid, err := d.PeekID()
	if err != nil {
		return
	}
	switch cid {
	case idMsgContainer:
		if !top {
			return // вложенные контейнеры запрещены
		}
		_, _ = d.ID()
		n, err := d.VectorHeader(false, 16)
		if err != nil {
			return
		}
		for i := 0; i < n; i++ {
			mid, err1 := d.Int64()
			mseq, err2 := d.Int32()
			ln, err3 := d.Int32()
			if err1 != nil || err2 != nil || err3 != nil || ln < 0 || ln%4 != 0 {
				return
			}
			inner, err := d.Raw(int(ln))
			if err != nil {
				return
			}
			s.handle(mid, mseq, append([]byte(nil), inner...), false)
		}
	case idGzipPacked:
		_, _ = d.ID()
		packed, err := d.Bytes()
		if err != nil {
			return
		}
		un, err := gunzip(packed)
		if err != nil {
			return
		}
		s.handleObject(un)
	default:
		s.handleObject(body)
	}
}

// handleObject разбирает тело, уже освобождённое от контейнера и gzip.
func (s *Session) handleObject(body []byte) {
	d := tlbin.NewDecoder(body)
	cid, err := d.PeekID()
	if err != nil {
		return
	}
	if cid == idRPCResult {
		_, _ = d.ID()
		reqID, err := d.Int64()
		if err != nil {
			return
		}
		s.handleResult(reqID, d.Rest())
		return
	}
	if cid == idGzipPacked {
		_, _ = d.ID()
		packed, err := d.Bytes()
		if err == nil {
			if un, err := gunzip(packed); err == nil {
				s.handleObject(un)
			}
		}
		return
	}
	o, err := tl.DecodeObject(tlbin.NewDecoder(body))
	if err != nil {
		// Не служебный и не известный кодеку объект — это обновление (В4 разбирает сам).
		if s.cfg.OnUpdate != nil {
			s.cfg.OnUpdate(body)
		}
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	switch m := o.(type) {
	case *tl.BadServerSalt:
		s.salt = m.NewServerSalt
		s.resendLocked(m.BadMsgId)
	case *tl.BadMsgNotification:
		switch m.ErrorCode {
		case 16, 17: // msg_id слишком мал / велик: синхронизируем время по msg_id сервера
			// Время сервера берём из последнего принятого msg_id (он и есть это уведомление).
			if n := len(s.seenOrder); n > 0 {
				server := time.Unix(s.seenOrder[n-1]>>32, 0)
				s.offset = time.Until(server)
			}
			s.resendLocked(m.BadMsgId)
		case 32: // seq_no слишком мал — сдвигаем счётчик и повторяем
			s.seqCount += 64
			s.resendLocked(m.BadMsgId)
		default:
			ids := []int64{m.BadMsgId}
			if inner, ok := s.containers[m.BadMsgId]; ok {
				ids = inner
			}
			for _, id := range ids {
				s.resolveLocked(id, result{err: &BadMsgError{Code: m.ErrorCode}})
			}
		}
	case *tl.NewSessionCreated:
		s.salt = m.ServerSalt
		if s.cfg.OnNewSession != nil {
			go s.cfg.OnNewSession()
		}
	case *tl.Pong:
		s.resolveLocked(m.MsgId, result{v: m})
	case *tl.FutureSalts:
		s.resolveLocked(m.ReqMsgId, result{v: m})
	case *tl.MsgsAck, *tl.MsgDetailedInfo, *tl.MsgNewDetailedInfo, *tl.MsgsStateInfo, *tl.MsgsAllInfo:
		// Подтверждения и служебные сведения: действий не требуют.
	default:
		if s.cfg.OnUpdate != nil {
			s.cfg.OnUpdate(body)
		}
	}
}

func (s *Session) handleResult(reqID int64, res []byte) {
	d := tlbin.NewDecoder(res)
	cid, err := d.PeekID()
	if err != nil {
		return
	}
	if cid == idGzipPacked {
		_, _ = d.ID()
		packed, err := d.Bytes()
		if err != nil {
			return
		}
		if res, err = gunzip(packed); err != nil {
			s.mu.Lock()
			s.resolveLocked(reqID, result{err: err})
			s.mu.Unlock()
			return
		}
		d = tlbin.NewDecoder(res)
		if cid, err = d.PeekID(); err != nil {
			return
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	p, ok := s.pending[reqID]
	if !ok {
		return
	}
	if cid == idRPCError {
		o, err := tl.DecodeObject(d)
		if e, ok := o.(*tl.RpcError); ok && err == nil {
			s.resolveLocked(reqID, result{err: &RPCError{Code: e.ErrorCode, Message: e.ErrorMessage}})
		} else {
			s.resolveLocked(reqID, result{err: fmt.Errorf("session: rpc_error не разобран: %v", err)})
		}
		return
	}
	v, err := p.req.DecodeResult(d)
	s.resolveLocked(reqID, result{v: v, err: err})
}

// ---------------------------------------------------------------- ping

func (s *Session) pingLoop(ctx context.Context) {
	t := time.NewTicker(pingInterval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			var id int64
			_ = binary.Read(bytes.NewReader(random(8)), binary.LittleEndian, &id)
			pctx, cancel := context.WithTimeout(ctx, 30*time.Second)
			_, err := s.Invoke(pctx, &tl.PingDelayDisconnectRequest{PingId: id, DisconnectDelay: disconnectDelay})
			cancel()
			if err != nil && ctx.Err() == nil {
				s.fail(fmt.Errorf("session: ping без ответа: %w", err))
				return
			}
		}
	}
}

// Ping — явная проверка связи (для диагностики и тестов).
func (s *Session) Ping(ctx context.Context) (*tl.Pong, error) {
	var id int64
	_ = binary.Read(bytes.NewReader(random(8)), binary.LittleEndian, &id)
	v, err := s.Invoke(ctx, &tl.PingRequest{PingId: id})
	if err != nil {
		return nil, err
	}
	pong, ok := v.(*tl.Pong)
	if !ok || pong.PingId != id {
		return nil, fmt.Errorf("session: неверный pong")
	}
	return pong, nil
}
