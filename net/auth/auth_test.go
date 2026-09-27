package auth

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"nuncio/net/core"
	"nuncio/net/dc"
	"nuncio/net/internal/fakeserver"
	"nuncio/net/rpc"
	"nuncio/net/session"
	"nuncio/net/transport"
)

// memKeyring — связка ключей в памяти.
type memKeyring struct {
	mu      sync.Mutex
	key     []byte
	deletes int
}

func (k *memKeyring) WrapKey(_ context.Context, create bool) ([]byte, bool, error) {
	k.mu.Lock()
	defer k.mu.Unlock()
	if k.key == nil && create {
		k.key = make([]byte, 32)
		_ = core.Random(k.key)
	}
	return k.key, k.key != nil, nil
}

func (k *memKeyring) Delete(context.Context) error {
	k.mu.Lock()
	defer k.mu.Unlock()
	k.key = nil
	k.deletes++
	return nil
}

var fast = &rpc.Limits{PerMethodRate: 1000, PerMethodBurst: 1000, GlobalRate: 1000, GlobalBurst: 1000}

type env struct {
	t       *testing.T
	acc     *fakeserver.Account
	servers map[int]*fakeserver.Server
	kr      *memKeyring
	path    string
}

func newEnv(t *testing.T, acc *fakeserver.Account, dcs ...int) *env {
	e := &env{t: t, acc: acc, servers: map[int]*fakeserver.Server{}, kr: &memKeyring{},
		path: filepath.Join(t.TempDir(), "session.bin")}
	for _, d := range dcs {
		s, err := fakeserver.Start(fakeserver.Options{DC: d, Account: acc})
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { s.Close() })
		n, ex := s.PublicKey()
		if _, err := core.AddRSAKey(n, ex); err != nil {
			t.Fatal(err)
		}
		e.servers[d] = s
	}
	return e
}

// client — «запуск приложения»: новый менеджер DC и клиент входа над тем же файлом и связкой.
type client struct {
	*Client
	m      *dc.Manager
	states chan State
	errs   chan error
}

func (e *env) start() *client {
	addrs := map[int]string{}
	for d, s := range e.servers {
		addrs[d] = s.Addr()
	}
	cl := &client{states: make(chan State, 64), errs: make(chan error, 8)}
	var c *Client
	cl.m = dc.New(dc.Options{
		App:   dc.App{APIID: 1, APIHash: "hash", DeviceModel: "test", SystemVersion: "linux", AppVersion: "0"},
		Addrs: addrs, Home: 2, Limits: fast, DisablePing: true,
		Transport: transport.Options{Framing: transport.Intermediate, Obfuscate: true},
		OnKey:     func(d int, k *session.AuthKey) { c.OnKey(d, k) },
		OnUpdate:  func(_ int, b []byte) { c.HandleUpdate(b) },
	})
	c = New(Config{DC: cl.m, Keyring: e.kr, Path: e.path,
		Emit:    func(s State) { cl.states <- s },
		OnError: func(err error) { cl.errs <- err }})
	cl.Client = c
	e.t.Cleanup(cl.m.Close)
	return cl
}

func (c *client) wait(t *testing.T, step Step) State {
	t.Helper()
	deadline := time.After(15 * time.Second)
	for {
		select {
		case s := <-c.states:
			if s.Step == step {
				return s
			}
		case err := <-c.errs:
			t.Fatalf("ошибка фонового входа: %v", err)
		case <-deadline:
			t.Fatalf("не дождались шага %d (последний %d)", step, c.State().Step)
		}
	}
}

func ctx(t *testing.T) context.Context {
	c, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	t.Cleanup(cancel)
	return c
}

func rpcMsg(err error) string {
	var re *session.RPCError
	if errors.As(err, &re) {
		return re.Message
	}
	return ""
}

func account() *fakeserver.Account {
	return &fakeserver.Account{Phone: "79990001122", Code: "12345", Registered: true, UserID: 777, FirstName: "Тест"}
}

func TestPhoneCodePasswordAndRestart(t *testing.T) {
	acc := account()
	acc.Password, acc.Hint = "секретный пароль", "подсказка"
	e := newEnv(t, acc, 2)
	c := e.start()
	if err := c.Resume(ctx(t)); err != nil {
		t.Fatal(err)
	}
	c.wait(t, StepLoggedOut)

	if err := c.SignIn(ctx(t), "1"); !errors.Is(err, ErrNoPhone) {
		t.Fatalf("код без номера: %v", err)
	}
	if err := c.SendCode(ctx(t), "+7 999 000-11-22"); err != nil {
		t.Fatal(err)
	}
	s := c.wait(t, StepCode)
	if s.Code.Kind != "app" || s.Code.Length != 5 || s.Code.NextKind != "sms" || s.Code.Timeout != 30 {
		t.Fatalf("сведения о коде: %+v", s.Code)
	}
	if err := c.SignIn(ctx(t), "00000"); rpcMsg(err) != "PHONE_CODE_INVALID" {
		t.Fatalf("неверный код: %v", err)
	}
	if err := c.SignIn(ctx(t), "12345"); err != nil {
		t.Fatal(err)
	}
	s = c.wait(t, StepPassword)
	if s.PasswordHint != "подсказка" || !s.HasRecovery {
		t.Fatalf("шаг пароля: %+v", s)
	}
	if err := c.CheckPassword(ctx(t), []byte("не тот")); rpcMsg(err) != "PASSWORD_HASH_INVALID" {
		t.Fatalf("неверный пароль: %v", err)
	}
	if err := c.CheckPassword(ctx(t), []byte("секретный пароль")); err != nil {
		t.Fatal(err)
	}
	s = c.wait(t, StepReady)
	if s.User == nil || s.User.ID != 777 || s.User.FirstName != "Тест" {
		t.Fatalf("пользователь: %+v", s.User)
	}
	keyID := c.m.Key(2).Key.ID()

	// Файл сессии: права 0600, нет номера, кода и пароля в открытом виде.
	st, err := os.Stat(e.path)
	if err != nil || st.Mode().Perm() != 0o600 {
		t.Fatalf("файл сессии: %v %v", st, err)
	}
	data, _ := os.ReadFile(e.path)
	for _, secret := range []string{"79990001122", "12345", "секретный пароль"} {
		if bytes.Contains(data, []byte(secret)) {
			t.Fatalf("в файле сессии открытый секрет %q", secret)
		}
	}
	c.m.Close()

	// Перезапуск: вход без повторного ввода, тот же ключ.
	c2 := e.start()
	if err := c2.Resume(ctx(t)); err != nil {
		t.Fatal(err)
	}
	s = c2.wait(t, StepReady)
	if s.User == nil || s.User.FirstName != "Тест" || c2.m.Key(2).Key.ID() != keyID {
		t.Fatalf("после перезапуска: %+v", s.User)
	}
	if e.servers[2].Calls("auth.sendCode") != 1 {
		t.Fatal("после перезапуска снова запрашивался код")
	}
}

func TestQR(t *testing.T) {
	for _, tc := range []struct {
		name     string
		qrDC     int
		password string
	}{{"тот же DC", 0, ""}, {"переход в DC4", 4, ""}, {"с облачным паролем", 0, "pw"}} {
		t.Run(tc.name, func(t *testing.T) {
			acc := account()
			acc.QRDC, acc.Password = tc.qrDC, tc.password
			e := newEnv(t, acc, 2, 4)
			c := e.start()
			if err := c.Resume(ctx(t)); err != nil {
				t.Fatal(err)
			}
			c.StartQR()
			s := c.wait(t, StepQR)
			if len(s.QRURL) < 20 || s.QRURL[:17] != "tg://login?token=" || time.Until(s.QRExpires) < 20*time.Second {
				t.Fatalf("QR: %q до %v", s.QRURL, s.QRExpires)
			}
			if e.servers[2].ConfirmQR() != 1 {
				t.Fatal("токен не выдан")
			}
			if tc.password != "" {
				c.wait(t, StepPassword)
				if err := c.CheckPassword(ctx(t), []byte(tc.password)); err != nil {
					t.Fatal(err)
				}
			}
			c.wait(t, StepReady)
			want := 2
			if tc.qrDC != 0 {
				want = tc.qrDC
			}
			if c.m.Home() != want {
				t.Fatalf("основной DC %d, ожидался %d", c.m.Home(), want)
			}
			// Перезапуск помнит основной DC.
			c.m.Close()
			c2 := e.start()
			if err := c2.Resume(ctx(t)); err != nil {
				t.Fatal(err)
			}
			c2.wait(t, StepReady)
			if c2.m.Home() != want {
				t.Fatalf("после перезапуска основной DC %d", c2.m.Home())
			}
		})
	}
}

func TestQRRefreshesToken(t *testing.T) {
	e := newEnv(t, account(), 2)
	c := e.start()
	_ = c.Resume(ctx(t))
	c.StartQR()
	first := c.wait(t, StepQR).QRURL
	// Обновление приходит при любом updateLoginToken: второй токен — новый.
	c.qrWake <- struct{}{}
	second := c.wait(t, StepQR).QRURL
	if first == second {
		t.Fatal("токен QR не обновился")
	}
	// Вход по номеру останавливает QR.
	if err := c.SendCode(ctx(t), "79990001122"); err != nil {
		t.Fatal(err)
	}
	c.wait(t, StepCode)
	n := e.servers[2].Calls("auth.exportLoginToken")
	c.qrWake <- struct{}{}
	time.Sleep(200 * time.Millisecond)
	if e.servers[2].Calls("auth.exportLoginToken") != n {
		t.Fatal("QR продолжает обновляться после перехода к номеру")
	}
}

func TestSignUp(t *testing.T) {
	acc := account()
	acc.Registered, acc.Terms = false, "Условия"
	e := newEnv(t, acc, 2)
	c := e.start()
	_ = c.Resume(ctx(t))
	if err := c.SendCode(ctx(t), "79990001122"); err != nil {
		t.Fatal(err)
	}
	if err := c.SignIn(ctx(t), "12345"); err != nil {
		t.Fatal(err)
	}
	if s := c.wait(t, StepSignUp); s.Terms != "Условия" {
		t.Fatalf("условия: %q", s.Terms)
	}
	if err := c.SignUp(ctx(t), "", ""); rpcMsg(err) != "FIRSTNAME_INVALID" {
		t.Fatalf("пустое имя: %v", err)
	}
	if err := c.SignUp(ctx(t), "Новый", "Пользователь"); err != nil {
		t.Fatal(err)
	}
	if s := c.wait(t, StepReady); s.User.FirstName != "Новый" {
		t.Fatalf("имя после регистрации: %+v", s.User)
	}
}

func login(t *testing.T, c *client) {
	t.Helper()
	if err := c.SendCode(ctx(t), "79990001122"); err != nil {
		t.Fatal(err)
	}
	if err := c.SignIn(ctx(t), "12345"); err != nil {
		t.Fatal(err)
	}
	c.wait(t, StepReady)
}

func TestLogOut(t *testing.T) {
	e := newEnv(t, account(), 2)
	c := e.start()
	_ = c.Resume(ctx(t))
	login(t, c)
	if err := c.LogOut(ctx(t)); err != nil {
		t.Fatal(err)
	}
	if s := c.wait(t, StepLoggedOut); s.Reason != ReasonLoggedOut {
		t.Fatalf("причина: %q", s.Reason)
	}
	if e.acc.LoggedOut() != 1 || e.acc.SignedIn() != 0 {
		t.Fatal("сервер не получил auth.logOut")
	}
	if _, err := os.Stat(e.path); !os.IsNotExist(err) {
		t.Fatal("файл сессии остался после выхода")
	}
	if e.kr.deletes != 1 || c.m.Key(2) != nil {
		t.Fatal("ключ обёртки или ключ DC остались")
	}
	// Повторный вход после выхода работает (новый ключ обёртки).
	login(t, c)
	if _, err := os.Stat(e.path); err != nil {
		t.Fatal("после повторного входа нет файла сессии")
	}
}

func TestSessionEndedElsewhere(t *testing.T) {
	e := newEnv(t, account(), 2)
	c := e.start()
	_ = c.Resume(ctx(t))
	login(t, c)
	c.m.Close()
	e.acc.Revoke()
	c2 := e.start()
	if err := c2.Resume(ctx(t)); err != nil {
		t.Fatal(err)
	}
	if s := c2.wait(t, StepLoggedOut); s.Reason != ReasonSessionEnded {
		t.Fatalf("причина: %q", s.Reason)
	}
	if _, err := os.Stat(e.path); !os.IsNotExist(err) {
		t.Fatal("файл завершённой сессии не удалён (Со-6)")
	}
}

func TestKeyringLostKey(t *testing.T) {
	e := newEnv(t, account(), 2)
	c := e.start()
	_ = c.Resume(ctx(t))
	login(t, c)
	c.m.Close()
	e.kr.key = nil // связку очистили
	c2 := e.start()
	if err := c2.Resume(ctx(t)); err != nil {
		t.Fatal(err)
	}
	if s := c2.wait(t, StepLoggedOut); s.Reason != ReasonKeyringNoKey {
		t.Fatalf("причина: %q", s.Reason)
	}
}

func TestBrokenSessionFile(t *testing.T) {
	e := newEnv(t, account(), 2)
	c := e.start()
	_ = c.Resume(ctx(t))
	login(t, c)
	c.m.Close()
	data, _ := os.ReadFile(e.path)
	data[len(data)-1] ^= 1 // порча запечатанного ключа (GCM не сойдётся)
	_ = os.WriteFile(e.path, data, 0o600)
	c2 := e.start()
	if err := c2.Resume(ctx(t)); err != nil {
		t.Fatal(err)
	}
	if s := c2.wait(t, StepLoggedOut); s.Reason != ReasonSessionBroken {
		t.Fatalf("причина: %q", s.Reason)
	}
	_ = os.WriteFile(e.path, []byte("мусор"), 0o600)
	c3 := e.start()
	_ = c3.Resume(ctx(t))
	if s := c3.wait(t, StepLoggedOut); s.Reason != ReasonSessionBroken {
		t.Fatalf("мусор: %q", s.Reason)
	}
}

func TestSessionFileRoundtrip(t *testing.T) {
	f := sessionFile{Home: 4, UserID: 42, Entries: []fileEntry{{DC: 2, Salt: -5, TimeOffset: 1500 * time.Millisecond,
		Sealed: []byte("abc")}, {DC: 4, Salt: 7, Sealed: make([]byte, 288)}}}
	back, err := parseSessionFile(f.marshal())
	if err != nil || back.Home != 4 || back.UserID != 42 || len(back.Entries) != 2 ||
		back.Entries[0].TimeOffset != 1500*time.Millisecond || string(back.Entries[0].Sealed) != "abc" {
		t.Fatalf("%+v %v", back, err)
	}
	raw := f.marshal()
	for i := 0; i < len(raw); i++ { // любое усечение — ошибка, а не паника
		if _, err := parseSessionFile(raw[:i]); err == nil {
			t.Fatalf("усечённый до %d файл принят", i)
		}
	}
}
