package rpc

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	"nuncio/net/session"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// fakeClock — время, которое идёт только при Sleep (тесты мгновенные и точные).
type fakeClock struct {
	mu    sync.Mutex
	now   time.Time
	slept time.Duration
}

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) Sleep(ctx context.Context, d time.Duration) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.slept += d
	c.mu.Unlock()
	return nil
}

func TestLimiterPerMethodAndGlobal(t *testing.T) {
	clk := &fakeClock{now: time.Unix(1_000_000, 0)}
	l := NewLimiter(Limits{PerMethodRate: 2, PerMethodBurst: 4, GlobalRate: 8, GlobalBurst: 16}, clk)
	ctx := context.Background()
	// 4 запроса одного метода — сразу (запас), следующие 20 — по 2 в секунду: ≈10 с.
	for i := 0; i < 24; i++ {
		if err := l.Wait(ctx, "help.getConfig"); err != nil {
			t.Fatal(err)
		}
	}
	if clk.slept < 9900*time.Millisecond || clk.slept > 10200*time.Millisecond {
		t.Fatalf("24 запроса одного метода заняли %v, ожидалось ≈10 с", clk.slept)
	}
	// Общий предел: 100 запросов разных методов — не быстрее 8 в секунду.
	clk2 := &fakeClock{now: time.Unix(1_000_000, 0)}
	l2 := NewLimiter(Limits{PerMethodRate: 2, PerMethodBurst: 4, GlobalRate: 8, GlobalBurst: 16}, clk2)
	for i := 0; i < 100; i++ {
		if err := l2.Wait(ctx, string(rune('a'+i%50))); err != nil {
			t.Fatal(err)
		}
	}
	if min := time.Duration(float64(100-16) / 8 * float64(time.Second)); clk2.slept < min-100*time.Millisecond {
		t.Fatalf("100 запросов заняли %v, общий предел не соблюдён (минимум %v)", clk2.slept, min)
	}
}

func TestLimiterRespectsContext(t *testing.T) {
	l := NewLimiter(Limits{PerMethodRate: 0.001, PerMethodBurst: 1, GlobalRate: 10, GlobalBurst: 10}, nil)
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	_ = l.Wait(ctx, "m")
	start := time.Now()
	if err := l.Wait(ctx, "m"); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("ожидалась отмена по контексту: %v", err)
	}
	if time.Since(start) > time.Second {
		t.Fatal("ожидание не прервалось по контексту")
	}
}

func TestClassify(t *testing.T) {
	cases := map[string]error{
		"FLOOD_WAIT_42":         &FloodError{RetryAfter: 42 * time.Second},
		"FLOOD_PREMIUM_WAIT_3":  &FloodError{RetryAfter: 3 * time.Second},
		"PHONE_MIGRATE_4":       &MigrateError{Kind: "PHONE", DC: 4},
		"USER_MIGRATE_2":        &MigrateError{Kind: "USER", DC: 2},
		"NETWORK_MIGRATE_5":     &MigrateError{Kind: "NETWORK", DC: 5},
		"PHONE_NUMBER_INVALID":  nil,
		"FLOOD_WAIT_X":          nil,
		"SOMETHING_MIGRATE_abc": nil,
	}
	for msg, want := range cases {
		got := Classify("m", &session.RPCError{Code: 420, Message: msg})
		switch w := want.(type) {
		case *FloodError:
			var fe *FloodError
			if !errors.As(got, &fe) || fe.RetryAfter != w.RetryAfter {
				t.Errorf("%s: %v", msg, got)
			}
		case *MigrateError:
			var me *MigrateError
			if !errors.As(got, &me) || *me != *w {
				t.Errorf("%s: %v", msg, got)
			}
		default:
			var re *session.RPCError
			if !errors.As(got, &re) {
				t.Errorf("%s: ожидалась исходная ошибка, получено %v", msg, got)
			}
		}
	}
	if Method(&tl.InvokeWithLayerRequest{Layer: 1, Query: &tl.InitConnectionRequest{Query: &tl.HelpGetConfigRequest{}}}) != "help.getConfig" {
		t.Fatal("Method не раскрыл обёртки")
	}
}

type scripted struct {
	errs  []error
	calls int
}

func (s *scripted) Invoke(ctx context.Context, req tlbin.Request) (any, error) {
	s.calls++
	if len(s.errs) > 0 {
		e := s.errs[0]
		s.errs = s.errs[1:]
		return nil, e
	}
	return "ok", nil
}

func TestClientRetries(t *testing.T) {
	newClient := func(inv Invoker) (*Client, *fakeClock) {
		clk := &fakeClock{now: time.Unix(1_000_000, 0)}
		c := NewClient(inv)
		c.Limiter = NewLimiter(DefaultLimits, clk)
		c.Clock = clk
		return c, clk
	}
	ctx := context.Background()
	req := &tl.HelpGetConfigRequest{}

	// Короткий FLOOD_WAIT ждём сами.
	inv := &scripted{errs: []error{&session.RPCError{Code: 420, Message: "FLOOD_WAIT_3"}}}
	c, clk := newClient(inv)
	if v, err := c.Invoke(ctx, req); err != nil || v != "ok" || inv.calls != 2 || clk.slept < 4*time.Second {
		t.Fatalf("короткий FLOOD_WAIT: %v %v вызовов %d, ждали %v", v, err, inv.calls, clk.slept)
	}
	// Длинный — сразу ошибка в интерфейс, без повторов.
	inv = &scripted{errs: []error{&session.RPCError{Code: 420, Message: "FLOOD_WAIT_3600"}}}
	c, _ = newClient(inv)
	var fe *FloodError
	if _, err := c.Invoke(ctx, req); !errors.As(err, &fe) || fe.RetryAfter != time.Hour || inv.calls != 1 {
		t.Fatalf("длинный FLOOD_WAIT: %v, вызовов %d", err, inv.calls)
	}
	// FLOOD_WAIT подряд трижды — после двух ожиданий ошибка (не зацикливаемся).
	f := &session.RPCError{Code: 420, Message: "FLOOD_WAIT_1"}
	inv = &scripted{errs: []error{f, f, f, f}}
	c, _ = newClient(inv)
	if _, err := c.Invoke(ctx, req); !errors.As(err, &fe) || inv.calls != 3 {
		t.Fatalf("повторные FLOOD_WAIT: %v, вызовов %d", err, inv.calls)
	}
	// 500 — повторы с нарастающей паузой, потом успех.
	e500 := &session.RPCError{Code: 500, Message: "INTERNAL"}
	inv = &scripted{errs: []error{e500, e500}}
	c, _ = newClient(inv)
	if v, err := c.Invoke(ctx, req); err != nil || v != "ok" || inv.calls != 3 {
		t.Fatalf("500: %v %v, вызовов %d", v, err, inv.calls)
	}
	// 500 больше Retries500 раз — ошибка.
	inv = &scripted{errs: []error{e500, e500, e500, e500, e500}}
	c, _ = newClient(inv)
	if _, err := c.Invoke(ctx, req); err == nil || inv.calls != 4 {
		t.Fatalf("много 500: %v, вызовов %d", err, inv.calls)
	}
	// MIGRATE — типизированная ошибка без повторов.
	inv = &scripted{errs: []error{&session.RPCError{Code: 303, Message: "PHONE_MIGRATE_4"}}}
	c, _ = newClient(inv)
	var me *MigrateError
	if _, err := c.Invoke(ctx, req); !errors.As(err, &me) || me.DC != 4 || inv.calls != 1 {
		t.Fatalf("MIGRATE: %v", err)
	}
	// Обычная ошибка — как есть.
	inv = &scripted{errs: []error{&session.RPCError{Code: 400, Message: "PHONE_NUMBER_INVALID"}}}
	c, _ = newClient(inv)
	var re *session.RPCError
	if _, err := c.Invoke(ctx, req); !errors.As(err, &re) || re.Code != 400 || inv.calls != 1 {
		t.Fatalf("400: %v", err)
	}
}

// hanging не отвечает первые n вызовов, пока не истечёт контекст попытки.
type hanging struct{ n, calls int }

func (h *hanging) Invoke(ctx context.Context, req tlbin.Request) (any, error) {
	h.calls++
	if h.calls <= h.n {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	return "ok", nil
}

func TestClientAttemptTimeout(t *testing.T) {
	newClient := func(inv Invoker) *Client {
		clk := &fakeClock{now: time.Unix(1_000_000, 0)}
		c := NewClient(inv)
		c.Limiter = NewLimiter(DefaultLimits, clk)
		c.Clock = clk
		c.AttemptTimeout = 20 * time.Millisecond
		return c
	}
	req := &tl.HelpGetConfigRequest{}
	// Попытка зависла — повтор, затем успех.
	h := &hanging{n: 2}
	if v, err := newClient(h).Invoke(context.Background(), req); err != nil || v != "ok" || h.calls != 3 {
		t.Fatalf("таймаут попытки: %v %v, вызовов %d", v, err, h.calls)
	}
	// Зависают все — ErrTimeout после Retries500 повторов.
	h = &hanging{n: 100}
	if _, err := newClient(h).Invoke(context.Background(), req); !errors.Is(err, ErrTimeout) || h.calls != 4 {
		t.Fatalf("постоянный таймаут: %v, вызовов %d", err, h.calls)
	}
	// Истёк контекст вызывающего — не повторяем и возвращаем его ошибку.
	h = &hanging{n: 100}
	c := newClient(h)
	c.AttemptTimeout = time.Hour
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := c.Invoke(ctx, req); !errors.Is(err, context.DeadlineExceeded) || h.calls != 1 {
		t.Fatalf("контекст вызывающего: %v, вызовов %d", err, h.calls)
	}
}

func TestLimiterSkipsFileParts(t *testing.T) {
	clk := &fakeClock{now: time.Unix(1_000_000, 0)}
	l := NewLimiter(DefaultLimits, clk)
	for i := 0; i < 200; i++ {
		if err := l.Wait(context.Background(), "upload.getFile"); err != nil {
			t.Fatal(err)
		}
	}
	if clk.slept != 0 {
		t.Fatalf("части файла ждали ограничителя: %v", clk.slept)
	}
	// И не съедают общий запас: обычный запрос идёт сразу.
	if err := l.Wait(context.Background(), "help.getConfig"); err != nil || clk.slept != 0 {
		t.Fatalf("общий запас потрачен частями файла: %v", clk.slept)
	}
}
