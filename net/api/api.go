// Package api — фасад сетевого слоя: принимает команды client.proto,
// исполняет их и складывает события в ограниченную очередь, из которой
// Rust забирает их через nn_next_event (см. Архитектура/nuncio - Границы и
// C ABI.md и nuncio - Сетевой слой (Go).md).
//
// Веха В3: вход (QR, телефон, код, пароль, регистрация, выход), прокси,
// список стран, состояние соединения, критические ошибки старта.
package api

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"path/filepath"
	"sync"
	"time"

	"google.golang.org/protobuf/proto"

	"nuncio/net/auth"
	"nuncio/net/core"
	"nuncio/net/dc"
	pb "nuncio/net/gen"
	"nuncio/net/keyring"
	"nuncio/net/rpc"
	"nuncio/net/session"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
	"nuncio/net/transport"
	"nuncio/net/updates"
)

// ProtoVersion — версия client.proto, которую понимает эта сборка.
// Rust передаёт свою в StartRequest; несовпадение — ошибка старта.
const ProtoVersion = 4

// QueueSize — предел очереди событий (обратное давление, правило 5).
const QueueSize = 1024

var (
	ErrAlreadyStarted = errors.New("сервис уже запущен (Go-рантайм нельзя перезапустить)")
	ErrNotStarted     = errors.New("сервис не запущен")
	ErrStopped        = errors.New("сервис остановлен")
	ErrQueueFull      = errors.New("очередь команд переполнена")
	ErrBadVersion     = errors.New("версия client.proto не совпадает")
)

// NextResult — исход ожидания события.
type NextResult int

const (
	GotEvent NextResult = iota
	Timeout
	Stopped
)

type command struct {
	id  uint64
	cmd *pb.Command
}

// Service — единственный экземпляр на процесс.
type Service struct {
	// Для тестов (задаются до Start): связка ключей в памяти и адреса DC.
	Keyring auth.Keyring
	Addrs   map[int]string

	mu       sync.Mutex
	started  bool
	stopped  bool
	commands chan command
	events   chan []byte
	done     chan struct{}
	wg       sync.WaitGroup
	ctx      context.Context
	cancel   context.CancelFunc

	req      *pb.StartRequest
	m        *dc.Manager
	auth     *auth.Client
	upd      *updates.Manager
	resumed  bool               // первый READY после старта — из файла сессии
	keepStop context.CancelFunc // поддержание соединения, пока вошли
	log      *log.Logger
	logFile  io.Closer
	ready    bool // связка ключей открыта, сессия восстановлена
	booting  bool
}

// New создаёт незапущенный сервис.
func New() *Service {
	return &Service{}
}

// Start проверяет StartRequest и запускает обработчик команд.
func (s *Service) Start(req []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.started {
		return ErrAlreadyStarted
	}
	var sr pb.StartRequest
	if err := proto.Unmarshal(req, &sr); err != nil {
		return fmt.Errorf("разбор StartRequest: %w", err)
	}
	if sr.GetProtoVersion() != ProtoVersion {
		return fmt.Errorf("%w: у приложения %d, у сети %d", ErrBadVersion, sr.GetProtoVersion(), ProtoVersion)
	}
	s.req = &sr
	s.commands = make(chan command, QueueSize)
	s.events = make(chan []byte, QueueSize)
	s.done = make(chan struct{})
	s.ctx, s.cancel = context.WithCancel(context.Background())
	s.log = log.New(io.Discard, "", 0)
	if dir := sr.GetLogDir(); dir != "" {
		if err := os.MkdirAll(dir, 0o700); err == nil {
			if f, err := os.OpenFile(filepath.Join(dir, "net.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600); err == nil {
				s.log, s.logFile = log.New(f, "", log.LstdFlags|log.Lmicroseconds), f
			}
		}
	}
	s.initNetwork()
	s.started = true
	s.wg.Add(1)
	go s.loop()
	if s.m != nil {
		s.booting = true
		go s.startup()
	}
	return nil
}

func proxyOf(p *pb.ProxySettings) transport.Proxy {
	if p == nil {
		return transport.Proxy{}
	}
	kind := transport.NoProxy
	switch p.GetKind() {
	case pb.ProxySettings_SOCKS5:
		kind = transport.SOCKS5
	case pb.ProxySettings_MTPROXY:
		kind = transport.MTProxy
	}
	return transport.Proxy{Kind: kind, Addr: p.GetAddr(), User: p.GetUser(), Password: p.GetPassword(),
		Secret: p.GetSecret()}
}

func (s *Service) initNetwork() {
	r := s.req
	if r.GetApiId() == 0 || r.GetApiHash() == "" || r.GetDataDir() == "" {
		// Без api_id вход невозможен: интерфейс покажет, что положить в api.env.
		return
	}
	app := dc.App{APIID: r.GetApiId(), APIHash: r.GetApiHash(), DeviceModel: r.GetDeviceModel(),
		SystemVersion: r.GetSystemVersion(), AppVersion: r.GetAppVersion(), SystemLangCode: r.GetSystemLangCode(),
		LangCode: r.GetLangCode()}
	var a *auth.Client
	s.m = dc.New(dc.Options{
		App: app, Addrs: s.Addrs, TestDCs: r.GetTestServers(),
		Transport: transport.Options{Framing: transport.Intermediate, Obfuscate: true, Proxy: proxyOf(r.GetProxy())},
		OnKey:     func(d int, k *session.AuthKey) { a.OnKey(d, k) },
		OnUpdate: func(_ int, body []byte) {
			if a.HandleUpdate(body) {
				return
			}
			u, err := tl.DecodeUpdatesClass(tlbin.NewDecoder(body))
			if err != nil {
				s.log.Printf("обновление не разобрано: %d байт", len(body))
				return
			}
			s.upd.Push(u)
		},
		OnNewSession: func(d int) {
			if d == s.m.Home() {
				s.upd.Difference()
			}
		},
		OnConn: func(st dc.ConnState) {
			if st == dc.Connected {
				s.upd.Difference() // после обрыва — догнать пропущенное
			}
			m := map[dc.ConnState]pb.ConnectionState_State{dc.Connecting: pb.ConnectionState_CONNECTING,
				dc.Connected: pb.ConnectionState_CONNECTED, dc.WaitingNetwork: pb.ConnectionState_WAITING_NETWORK}
			s.emit(&pb.Event{Body: &pb.Event_Connection{Connection: &pb.ConnectionState{State: m[st]}}})
		},
	})
	a = auth.New(auth.Config{DC: s.m, Path: filepath.Join(r.GetDataDir(), "session.bin"),
		Emit: s.emitAuth,
		OnError: func(err error) {
			s.log.Printf("QR: %s", errKind(err))
			s.emit(resultError(0, err))
		}})
	s.auth = a
	s.upd = updates.New(updates.Config{Inv: s.m, SelfID: a.UserID, Log: s.log.Printf,
		Emit: func(b *pb.UpdatesBatch) {
			s.emit(&pb.Event{Body: &pb.Event_Updates{Updates: b}})
		}})
}

// startup открывает связку ключей и восстанавливает сессию (Со-4 при ошибке).
func (s *Service) startup() {
	defer func() {
		s.mu.Lock()
		s.booting = false
		s.mu.Unlock()
	}()
	kr := s.Keyring
	if kr == nil {
		app := s.req.GetKeyringApp()
		if app == "" {
			app = "nuncio"
		}
		k, err := keyring.Open(app)
		if err != nil {
			s.log.Printf("старт: связка ключей недоступна")
			s.startupError(pb.StartupError_KEYRING_UNAVAILABLE, err)
			return
		}
		kr = k
	}
	s.auth.SetKeyring(kr)
	// Служба может спросить пароль связки: ждём пользователя до двух минут.
	ctx, cancel := context.WithTimeout(s.ctx, 2*time.Minute)
	defer cancel()
	if err := s.auth.Resume(ctx); err != nil {
		kind := pb.StartupError_OTHER
		if errors.Is(err, keyring.ErrDismissed) || errors.Is(err, context.DeadlineExceeded) {
			kind = pb.StartupError_KEYRING_DISMISSED
		}
		s.log.Printf("старт: восстановление сессии не удалось (%v)", kind)
		s.startupError(kind, err)
		return
	}
	s.mu.Lock()
	s.ready = true
	s.mu.Unlock()
	s.log.Printf("старт: готово, шаг входа %d", s.auth.State().Step)
}

func (s *Service) startupError(kind pb.StartupError_Kind, err error) {
	s.emit(&pb.Event{Body: &pb.Event_Startup{Startup: &pb.StartupError{Kind: kind, Text: err.Error()}}})
}

func (s *Service) emitAuth(st auth.State) {
	steps := map[auth.Step]pb.AuthState_Step{auth.StepQR: pb.AuthState_QR, auth.StepCode: pb.AuthState_CODE,
		auth.StepPassword: pb.AuthState_PASSWORD, auth.StepSignUp: pb.AuthState_SIGN_UP,
		auth.StepReady: pb.AuthState_READY, auth.StepLoggedOut: pb.AuthState_LOGGED_OUT}
	ev := &pb.AuthState{Step: steps[st.Step], QrUrl: st.QRURL, PasswordHint: st.PasswordHint,
		HasRecovery: st.HasRecovery, Terms: st.Terms, Reason: st.Reason}
	if !st.QRExpires.IsZero() {
		ev.QrExpiresUnix = st.QRExpires.Unix()
	}
	if st.Step == auth.StepCode {
		ev.Code = &pb.CodeInfo{Kind: st.Code.Kind, Length: uint32(st.Code.Length), NextKind: st.Code.NextKind,
			Timeout: uint32(st.Code.Timeout)}
	}
	if st.User != nil {
		ev.User = &pb.User{Id: st.User.ID, FirstName: st.User.FirstName, LastName: st.User.LastName,
			Username: st.User.Username}
	}
	// В журнал — только шаг и причина.
	s.log.Printf("вход: шаг %v %s", ev.Step, st.Reason)
	// Поток обновлений живёт, пока пользователь вошёл. Первый READY после
	// старта — сессия из файла: продолжаем с состояния из базы Rust (О-5).
	switch st.Step {
	case auth.StepReady:
		s.mu.Lock()
		resumed := !s.resumed
		s.resumed = true
		s.mu.Unlock()
		if resumed && s.req.GetUpdateState().GetPts() != 0 {
			s.upd.SetChannels(s.req.GetChannelStates())
			s.upd.Start(s.req.GetUpdateState())
		} else {
			s.upd.Start(nil)
		}
		s.mu.Lock()
		if s.keepStop == nil {
			kctx, cancel := context.WithCancel(s.ctx)
			s.keepStop = cancel
			s.m.Keep(kctx)
		}
		s.mu.Unlock()
	case auth.StepLoggedOut:
		s.mu.Lock()
		s.resumed = true
		if s.keepStop != nil {
			s.keepStop()
			s.keepStop = nil
		}
		s.mu.Unlock()
		s.upd.Stop()
	}
	s.emit(&pb.Event{Body: &pb.Event_Auth{Auth: ev}})
}

// Submit копирует команду в очередь и не блокирует.
func (s *Service) Submit(id uint64, data []byte) error {
	s.mu.Lock()
	started, stopped := s.started, s.stopped
	s.mu.Unlock()
	if !started {
		return ErrNotStarted
	}
	if stopped {
		return ErrStopped
	}
	var cmd pb.Command
	if err := proto.Unmarshal(data, &cmd); err != nil {
		return fmt.Errorf("разбор Command: %w", err)
	}
	select {
	case s.commands <- command{id: id, cmd: &cmd}:
		return nil
	default:
		return ErrQueueFull
	}
}

// NextEvent ждёт событие не дольше timeout.
func (s *Service) NextEvent(timeout time.Duration) ([]byte, NextResult) {
	s.mu.Lock()
	started := s.started
	s.mu.Unlock()
	if !started {
		return nil, Stopped
	}
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case ev := <-s.events:
		return ev, GotEvent
	case <-s.done:
		// Отдаём накопленное, прежде чем сообщить об остановке.
		select {
		case ev := <-s.events:
			return ev, GotEvent
		default:
			return nil, Stopped
		}
	case <-timer.C:
		return nil, Timeout
	}
}

// Shutdown останавливает обработчик и сеть; повторный вызов безопасен.
func (s *Service) Shutdown(timeout time.Duration) error {
	s.mu.Lock()
	if !s.started || s.stopped {
		s.mu.Unlock()
		return nil
	}
	s.stopped = true
	close(s.done)
	s.cancel()
	s.mu.Unlock()
	if s.upd != nil {
		s.upd.Stop()
	}
	if s.m != nil {
		s.m.Close()
	}

	finished := make(chan struct{})
	go func() { s.wg.Wait(); close(finished) }()
	select {
	case <-finished:
	case <-time.After(timeout):
		return errors.New("обработчик не остановился за отведённое время")
	}
	s.log.Printf("остановка")
	if s.logFile != nil {
		_ = s.logFile.Close()
	}
	return nil
}

func (s *Service) loop() {
	defer s.wg.Done()
	for {
		select {
		case <-s.done:
			return
		case c := <-s.commands:
			if _, ok := c.cmd.GetBody().(*pb.Command_Ping); ok || c.cmd.GetBody() == nil {
				s.emit(s.handle(c)) // мгновенная команда — в порядке поступления
				continue
			}
			// Сетевые команды могут ждать сервер: не задерживают остальные.
			s.wg.Add(1)
			go func() {
				defer s.wg.Done()
				s.emit(s.handle(c))
			}()
		}
	}
}

// handle исполняет команду; паника превращается в ошибку INTERNAL.
func (s *Service) handle(c command) (ev *pb.Event) {
	defer func() {
		if r := recover(); r != nil {
			ev = resultErrorKind(c.id, pb.Error_INTERNAL, fmt.Sprintf("паника: %v", r))
		}
	}()
	ok := func() *pb.Event { return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{CmdId: c.id}}} }
	switch b := c.cmd.GetBody().(type) {
	case *pb.Command_Ping:
		return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{CmdId: c.id,
			Payload: &pb.CommandResult_Pong{Pong: &pb.Pong{Message: b.Ping.GetMessage(), CoreVersion: core.Version()}}}}}
	case nil:
		return resultErrorKind(c.id, pb.Error_INTERNAL, "команда не поддерживается этой сборкой")
	}
	if s.m == nil {
		return resultErrorKind(c.id, pb.Error_INTERNAL, "не заданы api_id и api_hash (~/.config/nuncio/api.env)")
	}
	ctx, cancel := context.WithTimeout(s.ctx, 2*time.Minute)
	defer cancel()
	switch b := c.cmd.GetBody().(type) {
	case *pb.Command_RetryStartup:
		s.mu.Lock()
		busy := s.ready || s.booting
		if !busy {
			s.booting = true
		}
		s.mu.Unlock()
		if !busy {
			s.startup()
		}
		return ok()
	case *pb.Command_GetCountries:
		return s.countries(ctx, c.id, b.GetCountries.GetLangCode())
	case *pb.Command_TestProxy:
		return s.testProxy(ctx, c.id, b.TestProxy.GetProxy())
	case *pb.Command_SetProxy:
		s.m.SetProxy(proxyOf(b.SetProxy.GetProxy()))
		s.log.Printf("прокси изменён")
		return ok()
	}
	s.mu.Lock()
	ready := s.ready
	s.mu.Unlock()
	if !ready {
		return resultErrorKind(c.id, pb.Error_AUTH, "связка ключей ещё не открыта")
	}
	var err error
	switch b := c.cmd.GetBody().(type) {
	case *pb.Command_RequestQr:
		s.auth.StartQR()
	case *pb.Command_SendCode:
		err = s.auth.SendCode(ctx, b.SendCode.GetPhone())
	case *pb.Command_ResendCode:
		err = s.auth.ResendCode(ctx)
	case *pb.Command_CancelCode:
		s.auth.CancelCode(ctx)
	case *pb.Command_SignIn:
		err = s.auth.SignIn(ctx, b.SignIn.GetCode())
	case *pb.Command_CheckPassword:
		err = s.auth.CheckPassword(ctx, []byte(b.CheckPassword.GetPassword()))
	case *pb.Command_SignUp:
		err = s.auth.SignUp(ctx, b.SignUp.GetFirstName(), b.SignUp.GetLastName())
	case *pb.Command_LogOut:
		err = s.auth.LogOut(ctx)
	case *pb.Command_LoadDialogs:
		return s.loadDialogs(ctx, c.id, b.LoadDialogs)
	case *pb.Command_LoadHistory:
		return s.loadHistory(ctx, c.id, b.LoadHistory)
	case *pb.Command_SendMessage:
		return s.sendMessage(ctx, c.id, b.SendMessage)
	case *pb.Command_MarkRead:
		return s.markRead(ctx, c.id, b.MarkRead)
	case *pb.Command_DownloadAvatar:
		return s.downloadAvatar(ctx, c.id, b.DownloadAvatar)
	default:
		return resultErrorKind(c.id, pb.Error_INTERNAL, "команда не поддерживается этой сборкой")
	}
	if err != nil {
		s.log.Printf("команда %T: ошибка %s", c.cmd.GetBody(), errKind(err))
		return resultError(c.id, err)
	}
	return ok()
}

func (s *Service) countries(ctx context.Context, id uint64, lang string) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.HelpGetCountriesListRequest{LangCode: lang})
	if err != nil {
		return resultError(id, err)
	}
	list, ok := v.(*tl.HelpCountriesList)
	if !ok {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	out := &pb.Countries{}
	for _, cc := range list.Countries {
		c, ok := cc.(*tl.HelpCountry)
		if !ok || c.Hidden {
			continue
		}
		pc := &pb.Country{Iso2: c.Iso2, DefaultName: c.DefaultName, Name: c.DefaultName}
		if c.Name != nil {
			pc.Name = *c.Name
		}
		for _, code := range c.CountryCodes {
			if k, ok := code.(*tl.HelpCountryCode); ok {
				pc.Codes = append(pc.Codes, &pb.CountryCode{Code: k.CountryCode, Prefixes: k.Prefixes,
					Patterns: k.Patterns})
			}
		}
		out.Countries = append(out.Countries, pc)
	}
	return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{CmdId: id,
		Payload: &pb.CommandResult_Countries{Countries: out}}}}
}

// testProxy — отдельный временный менеджер: обмен ключами и ping через прокси.
func (s *Service) testProxy(ctx context.Context, id uint64, p *pb.ProxySettings) *pb.Event {
	m := dc.New(dc.Options{App: s.m.App(), Addrs: s.Addrs, TestDCs: s.req.GetTestServers(), DisablePing: true,
		Transport: transport.Options{Framing: transport.Intermediate, Obfuscate: true, Proxy: proxyOf(p),
			Timeout: 10 * time.Second}})
	defer m.Close()
	pctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	rtt, err := m.Ping(pctx, 2)
	if err != nil {
		return resultError(id, err)
	}
	return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{CmdId: id,
		Payload: &pb.CommandResult_ProxyCheck{ProxyCheck: &pb.ProxyCheck{RttMs: uint32(rtt.Milliseconds())}}}}}
}

// errKind — краткое описание ошибки для журнала (без текста, который мог бы нести данные).
func errKind(err error) string {
	var re *session.RPCError
	var fe *rpc.FloodError
	switch {
	case errors.As(err, &re):
		return fmt.Sprintf("rpc %d %s", re.Code, re.Message)
	case errors.As(err, &fe):
		return fmt.Sprintf("flood %v", fe.RetryAfter)
	}
	return fmt.Sprintf("%T", err)
}

// toError — единая ошибка стыка (правило 4 заметки о стыках).
func toError(err error) *pb.Error {
	var re *session.RPCError
	var fe *rpc.FloodError
	var ne net.Error
	var te *transport.Error
	switch {
	case errors.As(err, &fe):
		return &pb.Error{Kind: pb.Error_FLOOD, Code: 420, Text: "FLOOD_WAIT", RetryAfter: uint32(fe.RetryAfter / time.Second)}
	case errors.As(err, &re):
		kind := pb.Error_RPC
		if auth.IsUnauthorized(err) {
			kind = pb.Error_AUTH
		}
		return &pb.Error{Kind: kind, Code: re.Code, Text: re.Message}
	case errors.Is(err, context.Canceled):
		return &pb.Error{Kind: pb.Error_CANCELLED, Text: "отменено"}
	case errors.Is(err, transport.ErrFakeTLS), errors.Is(err, transport.ErrBadSecret):
		return &pb.Error{Kind: pb.Error_NETWORK, Code: 1, Text: err.Error()}
	case errors.As(err, &ne), errors.As(err, &te), errors.Is(err, session.ErrClosed),
		errors.Is(err, context.DeadlineExceeded), errors.Is(err, rpc.ErrTimeout), errors.Is(err, io.EOF),
		errors.Is(err, io.ErrUnexpectedEOF):
		return &pb.Error{Kind: pb.Error_NETWORK, Text: err.Error()}
	}
	return &pb.Error{Kind: pb.Error_INTERNAL, Text: err.Error()}
}

func resultError(id uint64, err error) *pb.Event {
	return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{CmdId: id, Error: toError(err)}}}
}

func resultErrorKind(id uint64, kind pb.Error_Kind, text string) *pb.Event {
	return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{
		CmdId: id,
		Error: &pb.Error{Kind: kind, Text: text},
	}}}
}

func (s *Service) emit(ev *pb.Event) {
	data, err := proto.Marshal(ev)
	if err != nil {
		data, _ = proto.Marshal(&pb.Event{Body: &pb.Event_Fatal{Fatal: &pb.FatalError{Text: err.Error()}}})
	}
	// Обратное давление: отправитель ждёт, пока Rust заберёт события.
	select {
	case s.events <- data:
	case <-s.done:
	}
}
