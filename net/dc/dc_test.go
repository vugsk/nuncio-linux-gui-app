package dc

import (
	"context"
	"encoding/binary"
	"errors"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"nuncio/net/core"
	"nuncio/net/internal/fakeserver"
	"nuncio/net/rpc"
	"nuncio/net/tl"
	"nuncio/net/transport"
)

var fast = &rpc.Limits{PerMethodRate: 1000, PerMethodBurst: 1000, GlobalRate: 1000, GlobalBurst: 1000}

func startServer(t *testing.T, opt fakeserver.Options) *fakeserver.Server {
	t.Helper()
	if opt.DC == 0 {
		opt.DC = 2
	}
	s, err := fakeserver.Start(opt)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close() })
	n, e := s.PublicKey()
	fp, err := core.AddRSAKey(n, e)
	if err != nil {
		t.Fatal(err)
	}
	// Отпечаток считают независимо ядро (C++) и поддельный сервер (Go).
	if fp != s.Fingerprint() {
		t.Fatalf("отпечаток ядра %x, сервера %x", fp, s.Fingerprint())
	}
	return s
}

func newManager(addrs map[int]string, tr transport.Options, onUpdate func(int, []byte)) *Manager {
	return New(Options{
		App:   App{APIID: 1, DeviceModel: "test", SystemVersion: "linux", AppVersion: "0.0.0", SystemLangCode: "ru", LangCode: "ru"},
		Addrs: addrs, Home: 2, Transport: tr, Limits: fast, OnUpdate: onUpdate, DisablePing: true,
	})
}

func getConfig(t *testing.T, m *Manager) *tl.Config {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	v, err := m.Invoke(ctx, &tl.HelpGetConfigRequest{})
	if err != nil {
		t.Fatalf("help.getConfig: %v", err)
	}
	cfg, ok := v.(*tl.Config)
	if !ok {
		t.Fatalf("help.getConfig вернул %T", v)
	}
	return cfg
}

func TestAllFramings(t *testing.T) {
	s := startServer(t, fakeserver.Options{})
	for _, f := range []transport.Framing{transport.Abridged, transport.Intermediate, transport.PaddedIntermediate} {
		for _, obfs := range []bool{false, true} {
			m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: f, Obfuscate: obfs}, nil)
			if d, err := m.Ping(context.Background(), 2); err != nil {
				t.Fatalf("%v obfs=%v: ping: %v", f, obfs, err)
			} else if d > 5*time.Second {
				t.Fatalf("ping %v", d)
			}
			if cfg := getConfig(t, m); cfg.ThisDc != 2 || len(cfg.DcOptions) != 1 {
				t.Fatalf("%v obfs=%v: неверный config", f, obfs)
			}
			m.Close()
		}
	}
}

func TestServerQuirks(t *testing.T) {
	cases := map[string]fakeserver.Options{
		"dh_gen_retry":        {DHRetryOnce: true},
		"bad_server_salt":     {BadSaltFirst: true},
		"рассинхрон времени":  {TimeSkewFirst: true},
		"gzip_packed":         {GzipResults: true},
		"new_session_created": {NewSession: true},
		"всё сразу":           {DHRetryOnce: true, BadSaltFirst: true, GzipResults: true, NewSession: true},
	}
	for name, opt := range cases {
		t.Run(name, func(t *testing.T) {
			s := startServer(t, opt)
			m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate, Obfuscate: true}, nil)
			defer m.Close()
			getConfig(t, m)
			getConfig(t, m) // второй запрос в той же сессии тоже проходит
		})
	}
}

func TestUnknownObjectGoesToUpdates(t *testing.T) {
	s := startServer(t, fakeserver.Options{PushUnknown: true})
	got := make(chan []byte, 1)
	m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate},
		func(dc int, body []byte) { got <- body })
	defer m.Close()
	getConfig(t, m)
	select {
	case b := <-got:
		if binary.LittleEndian.Uint32(b) != 0xe317af7e {
			t.Fatalf("в обновления пришло %x", b[:4])
		}
	case <-time.After(5 * time.Second):
		t.Fatal("неизвестный объект не дошёл до OnUpdate")
	}
}

func TestFloodWaitShort(t *testing.T) {
	s := startServer(t, fakeserver.Options{FloodOnce: map[string]int{"help.getConfig": 1}})
	m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate}, nil)
	defer m.Close()
	start := time.Now()
	getConfig(t, m)
	if s.Calls("help.getConfig") != 2 || time.Since(start) < 2*time.Second {
		t.Fatalf("FLOOD_WAIT_1: вызовов %d за %v", s.Calls("help.getConfig"), time.Since(start))
	}
}

func TestFloodWaitLongIsReturned(t *testing.T) {
	s := startServer(t, fakeserver.Options{FloodOnce: map[string]int{"help.getConfig": 3600}})
	m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate}, nil)
	defer m.Close()
	_, err := m.Invoke(context.Background(), &tl.HelpGetConfigRequest{})
	var fe *rpc.FloodError
	if !errors.As(err, &fe) || fe.RetryAfter != time.Hour {
		t.Fatalf("ожидался FloodError на час: %v", err)
	}
}

func TestMigrate(t *testing.T) {
	s2 := startServer(t, fakeserver.Options{DC: 2, Migrate: map[string]int{"help.getConfig": 4}})
	s4 := startServer(t, fakeserver.Options{DC: 4})
	m := newManager(map[int]string{2: s2.Addr(), 4: s4.Addr()}, transport.Options{Framing: transport.Intermediate}, nil)
	defer m.Close()
	cfg := getConfig(t, m)
	if cfg.ThisDc != 4 || m.Home() != 4 {
		t.Fatalf("после USER_MIGRATE_4: ответ из DC %d, основной DC %d", cfg.ThisDc, m.Home())
	}
	if m.Key(2) == nil || m.Key(4) == nil || m.Key(2).Key.ID() == m.Key(4).Key.ID() {
		t.Fatal("для каждого DC должен быть свой ключ")
	}
}

func TestReconnectKeepsKey(t *testing.T) {
	s := startServer(t, fakeserver.Options{DropAfterReplies: 1})
	m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate}, nil)
	defer m.Close()
	getConfig(t, m)
	id := m.Key(2).Key.ID()
	// Сервер закрыл соединение после первого ответа: следующий запрос переподключается.
	time.Sleep(100 * time.Millisecond)
	getConfig(t, m)
	if m.Key(2).Key.ID() != id {
		t.Fatal("после переподключения ключ сменился")
	}
}

func TestNoAPIID(t *testing.T) {
	s := startServer(t, fakeserver.Options{})
	m := New(Options{Addrs: map[int]string{2: s.Addr()}, Limits: fast, DisablePing: true})
	defer m.Close()
	if _, err := m.Ping(context.Background(), 2); err != nil {
		t.Fatalf("ping без api_id: %v", err)
	}
	if _, err := m.Invoke(context.Background(), &tl.HelpGetConfigRequest{}); !errors.Is(err, ErrNoAPIID) {
		t.Fatalf("ожидалась ErrNoAPIID: %v", err)
	}
}

func TestConcurrentInvoke(t *testing.T) {
	s := startServer(t, fakeserver.Options{GzipResults: true})
	m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate, Obfuscate: true}, nil)
	defer m.Close()
	getConfig(t, m)
	var wg sync.WaitGroup
	var fails atomic.Int32
	for i := 0; i < 32; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			defer cancel()
			if _, err := m.Invoke(ctx, &tl.HelpGetNearestDcRequest{}); err != nil {
				t.Error(err)
				fails.Add(1)
			}
		}()
	}
	wg.Wait()
	if fails.Load() == 0 && s.Calls("help.getNearestDc") != 32 {
		t.Fatalf("сервер получил %d запросов из 32", s.Calls("help.getNearestDc"))
	}
}

func TestLimiterAppliesToRealCalls(t *testing.T) {
	s := startServer(t, fakeserver.Options{})
	lim := rpc.Limits{PerMethodRate: 5, PerMethodBurst: 1, GlobalRate: 100, GlobalBurst: 100}
	m := New(Options{App: App{APIID: 1}, Addrs: map[int]string{2: s.Addr()}, Limits: &lim, DisablePing: true})
	defer m.Close()
	start := time.Now()
	for i := 0; i < 6; i++ {
		if _, err := m.Invoke(context.Background(), &tl.HelpGetNearestDcRequest{}); err != nil {
			t.Fatal(err)
		}
	}
	// 1 сразу + 5 по 5 в секунду ≈ 1 с.
	if el := time.Since(start); el < 900*time.Millisecond {
		t.Fatalf("6 запросов при пределе 5/с заняли %v", el)
	}
}
