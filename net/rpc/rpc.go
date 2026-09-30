// Package rpc — запросы поверх сессии: ограничитель частоты (правило безопасности
// Э-2: отладка идёт на реальном аккаунте), обработка FLOOD_WAIT, *_MIGRATE,
// повторы при внутренних ошибках сервера (Архитектура/nuncio - Сетевой слой (Go).md,
// раздел 3).
package rpc

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	"nuncio/net/session"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// FloodError — сервер попросил подождать дольше, чем мы готовы ждать сами.
type FloodError struct {
	Method     string
	RetryAfter time.Duration
}

func (e *FloodError) Error() string {
	return fmt.Sprintf("rpc: %s: FLOOD_WAIT, повторить через %v", e.Method, e.RetryAfter)
}

// MigrateError — запрос нужно выполнить в другом DC (PHONE_MIGRATE_X и т.п.).
type MigrateError struct {
	Kind string // PHONE, USER, NETWORK, FILE, STATS
	DC   int
}

func (e *MigrateError) Error() string { return fmt.Sprintf("rpc: %s_MIGRATE_%d", e.Kind, e.DC) }

// Classify разбирает rpc_error: FLOOD_WAIT_X / FLOOD_PREMIUM_WAIT_X -> *FloodError,
// *_MIGRATE_X -> *MigrateError, иначе исходная ошибка.
func Classify(method string, err error) error {
	var re *session.RPCError
	if !errors.As(err, &re) {
		return err
	}
	msg := re.Message
	for _, prefix := range []string{"FLOOD_WAIT_", "FLOOD_PREMIUM_WAIT_"} {
		if strings.HasPrefix(msg, prefix) {
			if sec, e := strconv.Atoi(msg[len(prefix):]); e == nil && sec >= 0 {
				return &FloodError{Method: method, RetryAfter: time.Duration(sec) * time.Second}
			}
		}
	}
	if i := strings.Index(msg, "_MIGRATE_"); i > 0 {
		if dc, e := strconv.Atoi(msg[i+len("_MIGRATE_"):]); e == nil && dc > 0 {
			return &MigrateError{Kind: msg[:i], DC: dc}
		}
	}
	return err
}

// Method — имя метода для ограничителя и журнала; обёртки invokeWithLayer и
// initConnection раскрываются до вложенного запроса.
func Method(o tlbin.Object) string {
	switch r := o.(type) {
	case *tl.InvokeWithLayerRequest:
		return Method(r.Query)
	case *tl.InitConnectionRequest:
		return Method(r.Query)
	}
	return o.TLName()
}

// ---------------------------------------------------------------- ограничитель

// Clock — время для ограничителя (подменяется в тестах).
type Clock interface {
	Now() time.Time
	Sleep(ctx context.Context, d time.Duration) error
}

type realClock struct{}

func (realClock) Now() time.Time { return time.Now() }
func (realClock) Sleep(ctx context.Context, d time.Duration) error {
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-t.C:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

type bucket struct {
	tokens float64
	last   time.Time
}

// Limits — пределы частоты (запросов в секунду и запас).
type Limits struct {
	PerMethodRate  float64
	PerMethodBurst float64
	GlobalRate     float64
	GlobalBurst    float64
}

// DefaultLimits — осторожные пределы для реального аккаунта (Э-2): не больше
// 2 запросов в секунду на метод (запас 4) и 8 в секунду всего (запас 16).
var DefaultLimits = Limits{PerMethodRate: 2, PerMethodBurst: 4, GlobalRate: 8, GlobalBurst: 16}

// Limiter — ведро токенов на метод и общее ведро. Потокобезопасен.
type Limiter struct {
	mu      sync.Mutex
	lim     Limits
	clock   Clock
	global  bucket
	methods map[string]*bucket
}

// NewLimiter создаёт ограничитель; clock == nil — системное время.
func NewLimiter(lim Limits, clock Clock) *Limiter {
	if clock == nil {
		clock = realClock{}
	}
	now := clock.Now()
	return &Limiter{lim: lim, clock: clock, global: bucket{tokens: lim.GlobalBurst, last: now},
		methods: map[string]*bucket{}}
}

func refill(b *bucket, now time.Time, rate, burst float64) {
	if el := now.Sub(b.last).Seconds(); el > 0 {
		b.tokens += el * rate
		if b.tokens > burst {
			b.tokens = burst
		}
		b.last = now
	}
}

// fileMethods — передача файлов частями: идёт сериями параллельных запросов
// (официальные клиенты — десятками), под общими пределами файл шёл бы 1 МБ/с.
// Параллельность ограничивает сам загрузчик (net/files).
var fileMethods = map[string]bool{"upload.getFile": true, "upload.saveFilePart": true, "upload.saveBigFilePart": true}

// Wait ждёт, пока можно выполнить запрос method, и забирает токены.
func (l *Limiter) Wait(ctx context.Context, method string) error {
	if fileMethods[method] {
		return nil
	}
	for {
		l.mu.Lock()
		now := l.clock.Now()
		m, ok := l.methods[method]
		if !ok {
			m = &bucket{tokens: l.lim.PerMethodBurst, last: now}
			l.methods[method] = m
		}
		refill(m, now, l.lim.PerMethodRate, l.lim.PerMethodBurst)
		refill(&l.global, now, l.lim.GlobalRate, l.lim.GlobalBurst)
		if m.tokens >= 1 && l.global.tokens >= 1 {
			m.tokens--
			l.global.tokens--
			l.mu.Unlock()
			return nil
		}
		need := 0.0
		if m.tokens < 1 {
			need = (1 - m.tokens) / l.lim.PerMethodRate
		}
		if l.global.tokens < 1 {
			if g := (1 - l.global.tokens) / l.lim.GlobalRate; g > need {
				need = g
			}
		}
		l.mu.Unlock()
		if err := l.clock.Sleep(ctx, time.Duration(need*float64(time.Second))+time.Millisecond); err != nil {
			return err
		}
	}
}

// ---------------------------------------------------------------- клиент

// Invoker — то, что умеет выполнить запрос (сессия; в тестах — подделка).
type Invoker interface {
	Invoke(ctx context.Context, req tlbin.Request) (any, error)
}

// Client — запросы с ограничителем частоты и повторами.
type Client struct {
	Inv     Invoker
	Limiter *Limiter
	Clock   Clock
	// MaxFloodWait — сколько готовы ждать FLOOD_WAIT сами; дольше — ошибка в интерфейс.
	MaxFloodWait time.Duration
	// Retries500 — повторы при внутренней ошибке сервера (код 500 и -500)
	// и при таймауте попытки; счётчик общий.
	Retries500 int
	// AttemptTimeout — предел одной попытки (0 — без предела). Истёк он, а не
	// контекст вызывающего, — попытка повторяется, как при 500; повтор отправки
	// сообщений безопасен благодаря random_id (Сетевой слой, раздел 3).
	AttemptTimeout time.Duration
}

// ErrTimeout — сервер не ответил за AttemptTimeout во всех попытках.
var ErrTimeout = errors.New("rpc: сервер не ответил вовремя")

// NewClient создаёт клиента с пределами по умолчанию.
func NewClient(inv Invoker) *Client {
	return &Client{Inv: inv, Limiter: NewLimiter(DefaultLimits, nil), Clock: realClock{},
		MaxFloodWait: 10 * time.Second, Retries500: 3, AttemptTimeout: 30 * time.Second}
}

// Invoke выполняет запрос. Ошибки: *FloodError, *MigrateError, *session.RPCError, сетевые.
func (c *Client) Invoke(ctx context.Context, req tlbin.Request) (any, error) {
	method := Method(req)
	backoff := 500 * time.Millisecond
	fails500, floods := 0, 0
	for {
		if err := c.Limiter.Wait(ctx, method); err != nil {
			return nil, err
		}
		v, err := c.attempt(ctx, req)
		if err == nil {
			return v, nil
		}
		err = Classify(method, err)
		var fe *FloodError
		var re *session.RPCError
		switch {
		case errors.As(err, &fe):
			// Ждём сами только короткие паузы и не больше двух раз подряд.
			if fe.RetryAfter > c.MaxFloodWait || floods >= 2 {
				return nil, fe
			}
			floods++
			if err := c.Clock.Sleep(ctx, fe.RetryAfter+time.Second); err != nil {
				return nil, err
			}
		case (errors.Is(err, ErrTimeout) || errors.As(err, &re) && (re.Code == 500 || re.Code == -500)) &&
			fails500 < c.Retries500:
			fails500++
			if err := c.Clock.Sleep(ctx, backoff); err != nil {
				return nil, err
			}
			backoff *= 2
		default:
			return nil, err
		}
	}
}

// attempt — одна попытка с собственным пределом времени.
func (c *Client) attempt(ctx context.Context, req tlbin.Request) (any, error) {
	if c.AttemptTimeout <= 0 {
		return c.Inv.Invoke(ctx, req)
	}
	actx, cancel := context.WithTimeout(ctx, c.AttemptTimeout)
	defer cancel()
	v, err := c.Inv.Invoke(actx, req)
	if err != nil && ctx.Err() == nil && errors.Is(err, context.DeadlineExceeded) {
		return nil, ErrTimeout
	}
	return v, err
}
