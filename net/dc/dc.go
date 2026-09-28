// Package dc — соединения с дата-центрами Telegram: адреса DC, ключ авторизации
// на каждый DC, сессия, обёртка invokeWithLayer(initConnection(...)), переход в
// другой DC по *_MIGRATE_X и переподключение после разрыва
// (Архитектура/nuncio - Сетевой слой (Go).md, раздел 2).
//
// Веха В2: ключи живут в памяти (Keys); запечатывание в файл сессии — В3.
package dc

import (
	"context"
	"errors"
	"fmt"
	"math/rand/v2"
	"sync"
	"sync/atomic"
	"time"

	"nuncio/net/rpc"
	"nuncio/net/session"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
	"nuncio/net/transport"
)

// Production — встроенные адреса боевых DC (Telegram Desktop, mtproto_dc_options.cpp,
// kBuiltInDcs). Актуальный список приходит в help.getConfig.
var Production = map[int]string{
	1: "149.154.175.50:443",
	2: "149.154.167.51:443",
	3: "149.154.175.100:443",
	4: "149.154.167.91:443",
	5: "149.154.171.5:443",
}

// Test — встроенные адреса тестовых DC (kBuiltInDcsTest); номер DC в p_q_inner_data_dc +10000.
var Test = map[int]string{
	1: "149.154.175.10:443",
	2: "149.154.167.40:443",
	3: "149.154.175.117:443",
}

// App — поля initConnection.
type App struct {
	APIID          int32
	APIHash        string // для auth.sendCode и auth.exportLoginToken
	DeviceModel    string
	SystemVersion  string
	AppVersion     string
	SystemLangCode string
	LangPack       string
	LangCode       string
}

// Options — параметры менеджера.
type Options struct {
	App       App
	Addrs     map[int]string // адреса DC (по умолчанию Production)
	TestDCs   bool           // тестовые серверы: номер DC +10000 в обмене ключами
	Home      int            // основной DC (по умолчанию 2)
	Transport transport.Options
	Limits    *rpc.Limits // пределы частоты (по умолчанию rpc.DefaultLimits)
	OnUpdate  func(dc int, body []byte)
	// OnKey — создан новый ключ авторизации DC (сохранить в файл сессии).
	OnKey func(dc int, k *session.AuthKey)
	// OnConn — состояние соединения с основным DC (полоса «Подключение…»).
	OnConn func(ConnState)
	// OnNewSession — сервер создал новую сессию: возможны пропущенные обновления.
	OnNewSession func(dc int)
	// DisablePing — для тестов.
	DisablePing bool
}

type conn struct {
	dcID   int
	sess   *session.Session
	client *rpc.Client
	inited atomic.Bool // initConnection уже выполнен в этой сессии
}

// Manager — соединения со всеми DC; один на аккаунт.
type Manager struct {
	opt     Options
	limiter *rpc.Limiter

	mu    sync.Mutex
	home  int
	keys  map[int]*session.AuthKey
	conns map[int]*conn
	// dialing сериализует подключение к одному DC (ключ создаётся один раз).
	dialing map[int]*sync.Mutex
}

// ErrNoAPIID — запросы API без api_id невозможны (initConnection).
var ErrNoAPIID = errors.New("dc: не задан api_id (нужен для initConnection)")

// New создаёт менеджер; соединения открываются по требованию.
func New(opt Options) *Manager {
	if opt.Addrs == nil {
		opt.Addrs = Production
		if opt.TestDCs {
			opt.Addrs = Test
		}
	}
	if opt.Home == 0 {
		opt.Home = 2
	}
	lim := rpc.DefaultLimits
	if opt.Limits != nil {
		lim = *opt.Limits
	}
	if opt.Transport.Timeout == 0 {
		opt.Transport.Timeout = 15 * time.Second
	}
	return &Manager{opt: opt, limiter: rpc.NewLimiter(lim, nil), home: opt.Home,
		keys: map[int]*session.AuthKey{}, conns: map[int]*conn{}, dialing: map[int]*sync.Mutex{}}
}

// Home — текущий основной DC.
func (m *Manager) Home() int {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.home
}

// SetHome — сменить основной DC (после *_MIGRATE или из сохранённой сессии).
func (m *Manager) SetHome(dc int) {
	m.mu.Lock()
	m.home = dc
	m.mu.Unlock()
}

// Key — ключ авторизации DC (nil, если ещё не создан).
func (m *Manager) Key(dc int) *session.AuthKey {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.keys[dc]
}

// SetKey — задать готовый ключ (восстановление из файла сессии, В3).
func (m *Manager) SetKey(dc int, k *session.AuthKey) {
	m.mu.Lock()
	m.keys[dc] = k
	m.mu.Unlock()
}

// ConnState — состояние соединения с основным DC.
type ConnState int

const (
	Connecting ConnState = iota + 1
	Connected
	WaitingNetwork
)

func (m *Manager) connState(dc int, st ConnState) {
	if m.opt.OnConn != nil && dc == m.Home() {
		m.opt.OnConn(st)
	}
}

// SetProxy меняет прокси для новых соединений и закрывает текущие (они
// переподключатся при следующем запросе). Ключи сохраняются.
func (m *Manager) SetProxy(p transport.Proxy) {
	m.mu.Lock()
	m.opt.Transport.Proxy = p
	m.mu.Unlock()
	m.Close()
}

// App — параметры приложения (api_id, api_hash, сведения об устройстве).
func (m *Manager) App() App { return m.opt.App }

// Keys — снимок ключей по DC (для файла сессии).
func (m *Manager) Keys() map[int]*session.AuthKey {
	m.mu.Lock()
	defer m.mu.Unlock()
	out := make(map[int]*session.AuthKey, len(m.keys))
	for k, v := range m.keys {
		out[k] = v
	}
	return out
}

// Reset закрывает соединения и забывает все ключи (выход из аккаунта):
// следующий запрос начнёт с нового обмена ключами.
func (m *Manager) Reset() {
	m.Close()
	m.mu.Lock()
	keys := m.keys
	m.keys = map[int]*session.AuthKey{}
	m.home = m.opt.Home
	m.mu.Unlock()
	for _, k := range keys {
		k.Key.Free()
	}
}

// Close закрывает все соединения.
func (m *Manager) Close() {
	m.mu.Lock()
	conns := m.conns
	m.conns = map[int]*conn{}
	m.mu.Unlock()
	for _, c := range conns {
		c.sess.Close()
	}
}

func (m *Manager) dialLock(dc int) *sync.Mutex {
	m.mu.Lock()
	defer m.mu.Unlock()
	l, ok := m.dialing[dc]
	if !ok {
		l = &sync.Mutex{}
		m.dialing[dc] = l
	}
	return l
}

// connect возвращает живое соединение с DC, создавая ключ при необходимости.
func (m *Manager) connect(ctx context.Context, dc int) (*conn, error) {
	lock := m.dialLock(dc)
	lock.Lock()
	defer lock.Unlock()
	m.mu.Lock()
	if c, ok := m.conns[dc]; ok {
		select {
		case <-c.sess.Done():
			delete(m.conns, dc)
		default:
			m.mu.Unlock()
			return c, nil
		}
	}
	addr, ok := m.opt.Addrs[dc]
	key := m.keys[dc]
	m.mu.Unlock()
	if !ok {
		return nil, fmt.Errorf("dc: неизвестный DC %d", dc)
	}

	m.mu.Lock()
	topt := m.opt.Transport
	m.mu.Unlock()
	topt.DC = dc
	if m.opt.TestDCs {
		topt.DC = dc + 10000
	}
	m.connState(dc, Connecting)
	tc, err := transport.Dial(addr, topt)
	if err != nil {
		m.connState(dc, WaitingNetwork)
		return nil, fmt.Errorf("dc %d: %w", dc, err)
	}
	if key == nil {
		wire := dc
		if m.opt.TestDCs {
			wire += 10000
		}
		kctx, cancel := context.WithTimeout(ctx, 30*time.Second)
		key, err = session.CreateAuthKey(kctx, tc, wire)
		cancel()
		if err != nil {
			tc.Close()
			m.connState(dc, WaitingNetwork)
			return nil, fmt.Errorf("dc %d: обмен ключами: %w", dc, err)
		}
		m.mu.Lock()
		m.keys[dc] = key
		m.mu.Unlock()
		if m.opt.OnKey != nil {
			m.opt.OnKey(dc, key)
		}
	}
	dcID := dc
	s := session.New(session.Config{Conn: tc, Key: key.Key, Salt: key.Salt, TimeOffset: key.TimeOffset,
		DisablePing: m.opt.DisablePing,
		OnNewSession: func() {
			if m.opt.OnNewSession != nil {
				m.opt.OnNewSession(dcID)
			}
		},
		OnUpdate: func(body []byte) {
			if m.opt.OnUpdate != nil {
				m.opt.OnUpdate(dcID, body)
			}
		}})
	cl := rpc.NewClient(s)
	cl.Limiter = m.limiter // общий ограничитель на все DC
	c := &conn{dcID: dc, sess: s, client: cl}
	m.mu.Lock()
	m.conns[dc] = c
	m.mu.Unlock()
	m.connState(dc, Connected)
	go func() {
		<-s.Done()
		m.mu.Lock()
		current := m.conns[dc] == c
		m.mu.Unlock()
		if current { // обрыв, а не закрытие менеджером
			m.connState(dc, WaitingNetwork)
		}
	}()
	return c, nil
}

// wrap оборачивает первый запрос соединения в invokeWithLayer(initConnection(...)).
func (m *Manager) wrap(c *conn, req tlbin.Request) (tlbin.Request, error) {
	if c.inited.Load() {
		return req, nil
	}
	if m.opt.App.APIID == 0 {
		return nil, ErrNoAPIID
	}
	a := m.opt.App
	return &tl.InvokeWithLayerRequest{Layer: tl.Layer, Query: &tl.InitConnectionRequest{
		ApiId: a.APIID, DeviceModel: a.DeviceModel, SystemVersion: a.SystemVersion, AppVersion: a.AppVersion,
		SystemLangCode: a.SystemLangCode, LangPack: a.LangPack, LangCode: a.LangCode, Query: req,
	}}, nil
}

// Invoke выполняет запрос API в основном DC: с initConnection на новом соединении,
// переходом по *_MIGRATE и одним переподключением после разрыва.
func (m *Manager) Invoke(ctx context.Context, req tlbin.Request) (any, error) {
	return m.invoke(ctx, m.Home(), req, true)
}

// InvokeDC выполняет запрос в указанном DC без перехода по MIGRATE.
func (m *Manager) InvokeDC(ctx context.Context, dc int, req tlbin.Request) (any, error) {
	return m.invoke(ctx, dc, req, false)
}

func (m *Manager) invoke(ctx context.Context, dc int, req tlbin.Request, follow bool) (any, error) {
	reconnected, migrated := false, false
	for {
		c, err := m.connect(ctx, dc)
		if err != nil {
			return nil, err
		}
		wrapped, err := m.wrap(c, req)
		if err != nil {
			return nil, err
		}
		v, err := c.client.Invoke(ctx, wrapped)
		if err == nil {
			c.inited.Store(true)
			return v, nil
		}
		var me *rpc.MigrateError
		switch {
		case errors.As(err, &me) && follow && !migrated:
			migrated = true
			dc = me.DC
			m.SetHome(dc)
		case isConnError(c, err) && !reconnected && ctx.Err() == nil:
			reconnected = true // ключ остаётся, сессия новая
		default:
			return nil, err
		}
	}
}

// InvokeOn выполняет запрос в указанном DC от имени вошедшего пользователя:
// если ключ этого DC ещё не авторизован (401), авторизация переносится из
// основного DC (auth.exportAuthorization / auth.importAuthorization) и запрос
// повторяется; FILE_MIGRATE_X переводит запрос в указанный DC.
func (m *Manager) InvokeOn(ctx context.Context, dc int, req tlbin.Request) (any, error) {
	for attempt := 0; attempt < 3; attempt++ {
		home := m.Home()
		if dc == 0 || dc == home {
			return m.Invoke(ctx, req)
		}
		v, err := m.InvokeDC(ctx, dc, req)
		var re *session.RPCError
		var me *rpc.MigrateError
		switch {
		case err == nil:
			return v, nil
		case errors.As(err, &me) && me.DC != dc:
			dc = me.DC
		case errors.As(err, &re) && re.Code == 401:
			if err := m.transferAuth(ctx, dc); err != nil {
				return nil, err
			}
		default:
			return nil, err
		}
	}
	return nil, fmt.Errorf("dc: запрос в DC %d не удался после переноса авторизации", dc)
}

func (m *Manager) transferAuth(ctx context.Context, dc int) error {
	v, err := m.Invoke(ctx, &tl.AuthExportAuthorizationRequest{DcId: int32(dc)})
	if err != nil {
		return fmt.Errorf("dc: exportAuthorization: %w", err)
	}
	ex, ok := v.(*tl.AuthExportedAuthorization)
	if !ok {
		return fmt.Errorf("dc: неожиданный ответ %T", v)
	}
	if _, err := m.InvokeDC(ctx, dc, &tl.AuthImportAuthorizationRequest{Id: ex.Id, Bytes: ex.Bytes}); err != nil {
		return fmt.Errorf("dc: importAuthorization: %w", err)
	}
	return nil
}

// Keep поддерживает соединение с основным DC, пока жив ctx: после обрыва —
// переподключение с экспоненциальной задержкой и разбросом (1, 2, 4 … 60 с,
// ±20 %), Сетевой слой, раздел 7. Успешное подключение сообщается через OnConn
// (Connected), после него поток обновлений запрашивает разницу.
func (m *Manager) Keep(ctx context.Context) {
	go func() {
		delay := KeepMinDelay
		for ctx.Err() == nil {
			c, err := m.connect(ctx, m.Home())
			if err != nil {
				jitter := time.Duration(float64(delay) * (0.8 + 0.4*rand.Float64()))
				select {
				case <-ctx.Done():
					return
				case <-time.After(jitter):
				}
				delay = min(delay*2, KeepMaxDelay)
				continue
			}
			delay = KeepMinDelay
			select {
			case <-ctx.Done():
				return
			case <-c.sess.Done():
			}
		}
	}()
}

// Задержки переподключения (переменные — для тестов).
var (
	KeepMinDelay = time.Second
	KeepMaxDelay = 60 * time.Second
)

// Ping — проверка связи с DC без initConnection (ping — служебная функция MTProto).
func (m *Manager) Ping(ctx context.Context, dc int) (time.Duration, error) {
	c, err := m.connect(ctx, dc)
	if err != nil {
		return 0, err
	}
	start := time.Now()
	if _, err := c.sess.Ping(ctx); err != nil {
		return 0, err
	}
	return time.Since(start), nil
}

func isConnError(c *conn, err error) bool {
	select {
	case <-c.sess.Done():
		return true
	default:
	}
	return errors.Is(err, session.ErrClosed)
}
