// Package auth — вход в аккаунт и владение файлом сессии
// (Архитектура/nuncio - Сетевой слой (Go).md, раздел 6; Дизайн/nuncio - Дизайн - Вход.md).
//
// Потоки: QR (auth.exportLoginToken, updateLoginToken, auth.importLoginToken
// при переходе в другой DC), телефон и код (auth.sendCode, auth.signIn),
// регистрация (auth.signUp), облачный пароль (account.getPassword + SRP в
// ядре + auth.checkPassword), выход (auth.logOut и удаление всего локального).
//
// Состояние входа отдаётся наружу через Config.Emit (событие AuthState для
// интерфейса). В журнал и в ошибки не попадают номер, код и пароль.
package auth

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	"nuncio/net/core"
	"nuncio/net/dc"
	"nuncio/net/session"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// Step — шаг входа (что показывать интерфейсу).
type Step int

const (
	StepUnknown   Step = iota
	StepQR             // показан QR-код (Вх-1)
	StepCode           // код отправлен, ждём ввода (Вх-6)
	StepPassword       // нужен облачный пароль
	StepSignUp         // номера нет в Telegram: регистрация (Вх-5)
	StepReady          // вошли
	StepLoggedOut      // не вошли: нет сессии, выход или сессия завершена (Reason)
)

// Причины StepLoggedOut.
const (
	ReasonNone          = ""
	ReasonLoggedOut     = "logged_out"     // пользователь вышел сам
	ReasonSessionEnded  = "session_ended"  // сессию завершили на другом устройстве (Со-4, Со-6)
	ReasonKeyringNoKey  = "keyring_no_key" // файл сессии есть, а ключа обёртки в связке нет
	ReasonSessionBroken = "session_broken" // файл сессии не читается
)

// CodeInfo — куда и какой код отправлен.
type CodeInfo struct {
	Kind     string // app, sms, call, flash_call, missed_call, email, fragment, other
	Length   int    // число ячеек (0 — неизвестно)
	NextKind string // чем можно запросить код повторно (пусто — нечем)
	Timeout  int    // через сколько секунд можно запросить повторно
}

// User — вошедший пользователь.
type User struct {
	ID        int64
	FirstName string
	LastName  string
	Username  string
}

// State — снимок состояния входа.
type State struct {
	Step         Step
	QRURL        string
	QRExpires    time.Time
	Code         CodeInfo
	PasswordHint string
	HasRecovery  bool
	Terms        string // текст условий использования при регистрации
	User         *User
	Reason       string
}

// Keyring — ключ обёртки из Secret Service (пакет keyring; в тестах — подделка).
type Keyring interface {
	WrapKey(ctx context.Context, create bool) ([]byte, bool, error)
	Delete(ctx context.Context) error
}

// Config — зависимости клиента входа.
type Config struct {
	DC      *dc.Manager
	Keyring Keyring
	Path    string      // путь session.bin
	Emit    func(State) // изменение состояния входа
	OnError func(error) // сбой фонового QR-входа (интерфейс показывает «Обновить»)
}

// Client — вход и сессия одного аккаунта.
type Client struct {
	cfg Config

	mu        sync.Mutex
	wrap      [32]byte
	haveWrap  bool
	userID    int64
	phone     string
	codeHash  string
	qrCancel  context.CancelFunc
	qrWake    chan struct{}
	lastState State
}

// ErrNoPhone — код или регистрация без предварительного SendCode.
var ErrNoPhone = errors.New("auth: сначала нужно запросить код")

// New создаёт клиента.
func New(cfg Config) *Client {
	return &Client{cfg: cfg, qrWake: make(chan struct{}, 1)}
}

func (c *Client) emit(s State) {
	c.mu.Lock()
	c.lastState = s
	c.mu.Unlock()
	if c.cfg.Emit != nil {
		c.cfg.Emit(s)
	}
}

// SetKeyring задаёт связку ключей (открывается при старте, после New).
func (c *Client) SetKeyring(k Keyring) {
	c.mu.Lock()
	c.cfg.Keyring = k
	c.mu.Unlock()
}

// State — последнее отправленное состояние.
func (c *Client) State() State {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.lastState
}

// UserID — id вошедшего пользователя (0 — не вошли).
func (c *Client) UserID() int64 {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.userID
}

// ---------------------------------------------------------------- файл сессии

// Resume получает ключ обёртки (создаёт при первом запуске — здесь служба может
// спросить пароль связки) и восстанавливает сессию из файла. Ошибка — только
// когда недоступна связка ключей; всё остальное выражается состоянием.
func (c *Client) Resume(ctx context.Context) error {
	wrap, found, err := c.cfg.Keyring.WrapKey(ctx, false)
	if err != nil {
		return err
	}
	data, ferr := os.ReadFile(c.cfg.Path)
	if !found {
		reason := ReasonNone
		if ferr == nil {
			// Файл есть, а ключа нет (связку очистили): прочитать сессию нельзя.
			_ = os.Remove(c.cfg.Path)
			reason = ReasonKeyringNoKey
		}
		if wrap, _, err = c.cfg.Keyring.WrapKey(ctx, true); err != nil {
			return err
		}
		if err := c.setWrap(wrap); err != nil {
			return err
		}
		c.emit(State{Step: StepLoggedOut, Reason: reason})
		return nil
	}
	if err := c.setWrap(wrap); err != nil {
		return err
	}
	if ferr != nil {
		if errors.Is(ferr, os.ErrNotExist) {
			c.emit(State{Step: StepLoggedOut})
			return nil
		}
		return ferr
	}
	f, err := parseSessionFile(data)
	if err != nil {
		_ = os.Remove(c.cfg.Path)
		c.emit(State{Step: StepLoggedOut, Reason: ReasonSessionBroken})
		return nil
	}
	for _, e := range f.Entries {
		k, err := core.Unseal(e.Sealed, &c.wrap)
		if err != nil {
			_ = os.Remove(c.cfg.Path)
			c.emit(State{Step: StepLoggedOut, Reason: ReasonSessionBroken})
			return nil
		}
		c.cfg.DC.SetKey(e.DC, &session.AuthKey{Key: k, Salt: e.Salt, TimeOffset: e.TimeOffset})
	}
	if f.Home != 0 {
		c.cfg.DC.SetHome(f.Home)
	}
	if f.UserID == 0 {
		c.emit(State{Step: StepLoggedOut})
		return nil
	}
	c.mu.Lock()
	c.userID = f.UserID
	c.mu.Unlock()
	// Проверка, что сессия жива; без сети считаем, что вошли (данные есть у Rust).
	u := &User{ID: f.UserID}
	vctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	v, err := c.cfg.DC.Invoke(vctx, &tl.UsersGetUsersRequest{Id: []tl.InputUserClass{&tl.InputUserSelf{}}})
	if err != nil {
		if c.CheckAuthError(err) {
			return nil
		}
	} else if list, ok := v.([]tl.UserClass); ok && len(list) == 1 {
		if tu, ok := list[0].(*tl.User); ok {
			u = userOf(tu)
		}
	}
	c.emit(State{Step: StepReady, User: u})
	return nil
}

func (c *Client) setWrap(wrap []byte) error {
	if len(wrap) != 32 {
		return errors.New("auth: ключ обёртки не 32 байта")
	}
	c.mu.Lock()
	copy(c.wrap[:], wrap)
	c.haveWrap = true
	c.mu.Unlock()
	return nil
}

// Persist записывает файл сессии: все ключи DC, основной DC и пользователя.
func (c *Client) Persist() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if !c.haveWrap {
		return errors.New("auth: нет ключа обёртки")
	}
	f := sessionFile{Home: c.cfg.DC.Home(), UserID: c.userID}
	for dcID, k := range c.cfg.DC.Keys() {
		sealed, err := k.Key.Seal(&c.wrap)
		if err != nil {
			return err
		}
		f.Entries = append(f.Entries, fileEntry{DC: dcID, Salt: k.Salt, TimeOffset: k.TimeOffset, Sealed: sealed})
	}
	return writeAtomic(c.cfg.Path, f.marshal())
}

// OnKey — колбэк dc.Options.OnKey: новый ключ сразу попадает в файл.
func (c *Client) OnKey(int, *session.AuthKey) {
	c.mu.Lock()
	ok := c.haveWrap
	c.mu.Unlock()
	if ok {
		_ = c.Persist()
	}
}

// wipe удаляет локальную сессию: соединения, ключи DC, файл (выход, Со-6).
func (c *Client) wipe() {
	c.stopQR()
	c.cfg.DC.Reset()
	c.mu.Lock()
	c.userID, c.phone, c.codeHash = 0, "", ""
	c.mu.Unlock()
	_ = os.Remove(c.cfg.Path)
}

// ---------------------------------------------------------------- ошибки

func rpcMessage(err error) (int32, string) {
	var re *session.RPCError
	if errors.As(err, &re) {
		return re.Code, re.Message
	}
	return 0, ""
}

// IsPasswordNeeded — после входа нужен облачный пароль.
func IsPasswordNeeded(err error) bool {
	_, m := rpcMessage(err)
	return m == "SESSION_PASSWORD_NEEDED"
}

// IsUnauthorized — ключ больше не авторизован (сессия завершена или удалена).
func IsUnauthorized(err error) bool {
	code, m := rpcMessage(err)
	if code != 401 || m == "SESSION_PASSWORD_NEEDED" {
		return false
	}
	return true
}

// CheckAuthError: если ошибка означает, что сессия завершена, — удаляет
// локальную сессию и сообщает StepLoggedOut (Со-4, Со-6). true — так и было.
func (c *Client) CheckAuthError(err error) bool {
	if !IsUnauthorized(err) || c.UserID() == 0 {
		return false
	}
	c.wipe()
	c.emit(State{Step: StepLoggedOut, Reason: ReasonSessionEnded})
	return true
}

// ---------------------------------------------------------------- завершение входа

func userOf(u *tl.User) *User {
	out := &User{ID: u.Id}
	if u.FirstName != nil {
		out.FirstName = *u.FirstName
	}
	if u.LastName != nil {
		out.LastName = *u.LastName
	}
	if u.Username != nil {
		out.Username = *u.Username
	}
	return out
}

func (c *Client) finish(a tl.AuthAuthorizationClass) error {
	switch v := a.(type) {
	case *tl.AuthAuthorization:
		tu, ok := v.User.(*tl.User)
		if !ok {
			return fmt.Errorf("auth: неожиданный пользователь %T", v.User)
		}
		c.stopQR()
		c.mu.Lock()
		c.userID = tu.Id
		c.phone, c.codeHash = "", ""
		c.mu.Unlock()
		if err := c.Persist(); err != nil {
			return err
		}
		c.emit(State{Step: StepReady, User: userOf(tu)})
		return nil
	case *tl.AuthAuthorizationSignUpRequired:
		s := State{Step: StepSignUp}
		if tos, ok := v.TermsOfService.(*tl.HelpTermsOfService); ok {
			s.Terms = tos.Text
		}
		c.emit(s)
		return nil
	}
	return fmt.Errorf("auth: неожиданный ответ %T", a)
}

// passwordStep запрашивает параметры облачного пароля и показывает шаг ввода.
func (c *Client) passwordStep(ctx context.Context) error {
	v, err := c.cfg.DC.Invoke(ctx, &tl.AccountGetPasswordRequest{})
	if err != nil {
		return err
	}
	p, ok := v.(*tl.AccountPassword)
	if !ok {
		return fmt.Errorf("auth: неожиданный ответ %T", v)
	}
	s := State{Step: StepPassword, HasRecovery: p.HasRecovery}
	if p.Hint != nil {
		s.PasswordHint = *p.Hint
	}
	c.stopQR()
	c.emit(s)
	return nil
}

// ---------------------------------------------------------------- QR

// StartQR показывает QR-код и обновляет его, пока вход не подтвердят с
// телефона (или не начнут вход по номеру). Работает в фоне.
func (c *Client) StartQR() {
	c.stopQR()
	ctx, cancel := context.WithCancel(context.Background())
	c.mu.Lock()
	c.qrCancel = cancel
	c.mu.Unlock()
	go c.qrLoop(ctx)
}

func (c *Client) stopQR() {
	c.mu.Lock()
	if c.qrCancel != nil {
		c.qrCancel()
		c.qrCancel = nil
	}
	c.mu.Unlock()
}

func (c *Client) qrLoop(ctx context.Context) {
	app := c.cfg.DC.App()
	for ctx.Err() == nil {
		v, err := c.cfg.DC.Invoke(ctx, &tl.AuthExportLoginTokenRequest{ApiId: app.APIID, ApiHash: app.APIHash,
			ExceptIds: []int64{}})
		if err == nil {
			err = c.qrResult(ctx, v)
		}
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			if IsPasswordNeeded(err) {
				if perr := c.passwordStep(ctx); perr != nil {
					c.fail(perr)
				}
				return
			}
			c.fail(err)
			return
		}
		if c.State().Step != StepQR {
			return // вход завершён или сменился шаг
		}
		// Ждём подтверждения (updateLoginToken) или истечения токена.
		wait := time.Until(c.State().QRExpires)
		if wait < 5*time.Second {
			wait = 5 * time.Second
		}
		if wait > 60*time.Second {
			wait = 60 * time.Second
		}
		select {
		case <-ctx.Done():
			return
		case <-c.qrWake:
		case <-time.After(wait):
		}
	}
}

func (c *Client) fail(err error) {
	if c.CheckAuthError(err) {
		return
	}
	if c.cfg.OnError != nil {
		c.cfg.OnError(err)
	}
}

func (c *Client) qrResult(ctx context.Context, v any) error {
	switch t := v.(type) {
	case *tl.AuthLoginToken:
		offset := time.Duration(0)
		if k := c.cfg.DC.Key(c.cfg.DC.Home()); k != nil {
			offset = k.TimeOffset
		}
		c.emit(State{Step: StepQR, QRURL: "tg://login?token=" + base64.URLEncoding.EncodeToString(t.Token),
			QRExpires: time.Unix(int64(t.Expires), 0).Add(-offset)})
		return nil
	case *tl.AuthLoginTokenMigrateTo:
		c.cfg.DC.SetHome(int(t.DcId))
		r, err := c.cfg.DC.InvokeDC(ctx, int(t.DcId), &tl.AuthImportLoginTokenRequest{Token: t.Token})
		if err != nil {
			return err
		}
		return c.qrResult(ctx, r)
	case *tl.AuthLoginTokenSuccess:
		return c.finish(t.Authorization)
	}
	return fmt.Errorf("auth: неожиданный ответ %T", v)
}

// HandleUpdate разбирает обновления, важные для входа (updateLoginToken).
// Возвращает true, если обновление относилось к входу.
func (c *Client) HandleUpdate(body []byte) bool {
	u, err := tl.DecodeUpdatesClass(tlbin.NewDecoder(body))
	if err != nil {
		return false
	}
	var list []tl.UpdateClass
	switch v := u.(type) {
	case *tl.UpdateShort:
		list = []tl.UpdateClass{v.Update}
	case *tl.Updates:
		list = v.Updates
	case *tl.UpdatesCombined:
		list = v.Updates
	}
	for _, x := range list {
		if _, ok := x.(*tl.UpdateLoginToken); ok {
			select {
			case c.qrWake <- struct{}{}:
			default:
			}
			return true
		}
	}
	return false
}

// ---------------------------------------------------------------- телефон и код

func codeKind(t tl.AuthSentCodeTypeClass) (string, int) {
	switch v := t.(type) {
	case *tl.AuthSentCodeTypeApp:
		return "app", int(v.Length)
	case *tl.AuthSentCodeTypeSms:
		return "sms", int(v.Length)
	case *tl.AuthSentCodeTypeCall:
		return "call", int(v.Length)
	case *tl.AuthSentCodeTypeMissedCall:
		return "missed_call", int(v.Length)
	case *tl.AuthSentCodeTypeFlashCall:
		return "flash_call", 0
	case *tl.AuthSentCodeTypeEmailCode:
		return "email", int(v.Length)
	case *tl.AuthSentCodeTypeFragmentSms:
		return "fragment", int(v.Length)
	}
	return "other", 0
}

func nextKind(t tl.AuthCodeTypeClass) string {
	switch t.(type) {
	case *tl.AuthCodeTypeSms:
		return "sms"
	case *tl.AuthCodeTypeCall:
		return "call"
	case *tl.AuthCodeTypeFlashCall:
		return "flash_call"
	case *tl.AuthCodeTypeMissedCall:
		return "missed_call"
	case *tl.AuthCodeTypeFragmentSms:
		return "fragment"
	}
	return ""
}

func (c *Client) sentCode(v any) error {
	switch s := v.(type) {
	case *tl.AuthSentCode:
		kind, n := codeKind(s.Type)
		info := CodeInfo{Kind: kind, Length: n, NextKind: nextKind(s.NextType)}
		if s.Timeout != nil {
			info.Timeout = int(*s.Timeout)
		}
		c.mu.Lock()
		c.codeHash = s.PhoneCodeHash
		c.mu.Unlock()
		c.emit(State{Step: StepCode, Code: info})
		return nil
	case *tl.AuthSentCodeSuccess:
		return c.finish(s.Authorization)
	case *tl.AuthSentCodePaymentRequired:
		return errors.New("auth: для этого номера Telegram требует оплату входа — в nuncio не поддерживается")
	}
	return fmt.Errorf("auth: неожиданный ответ %T", v)
}

// SendCode запрашивает код для номера (переход в нужный DC — в dc.Manager).
func (c *Client) SendCode(ctx context.Context, phone string) error {
	c.stopQR()
	phone = normalizePhone(phone)
	app := c.cfg.DC.App()
	v, err := c.cfg.DC.Invoke(ctx, &tl.AuthSendCodeRequest{PhoneNumber: phone, ApiId: app.APIID, ApiHash: app.APIHash,
		Settings: &tl.CodeSettings{}})
	if err != nil {
		return err
	}
	c.mu.Lock()
	c.phone = phone
	c.mu.Unlock()
	return c.sentCode(v)
}

func normalizePhone(p string) string {
	var b strings.Builder
	for _, r := range p {
		if r >= '0' && r <= '9' {
			b.WriteRune(r)
		}
	}
	return b.String()
}

func (c *Client) phoneAndHash() (string, string, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.phone == "" || c.codeHash == "" {
		return "", "", ErrNoPhone
	}
	return c.phone, c.codeHash, nil
}

// ResendCode запрашивает код другим способом (next_type).
func (c *Client) ResendCode(ctx context.Context) error {
	phone, hash, err := c.phoneAndHash()
	if err != nil {
		return err
	}
	v, err := c.cfg.DC.Invoke(ctx, &tl.AuthResendCodeRequest{PhoneNumber: phone, PhoneCodeHash: hash})
	if err != nil {
		return err
	}
	return c.sentCode(v)
}

// CancelCode отменяет вход по номеру («Изменить номер»); ошибка сервера не важна.
func (c *Client) CancelCode(ctx context.Context) {
	phone, hash, err := c.phoneAndHash()
	if err == nil {
		_, _ = c.cfg.DC.Invoke(ctx, &tl.AuthCancelCodeRequest{PhoneNumber: phone, PhoneCodeHash: hash})
	}
	c.mu.Lock()
	c.phone, c.codeHash = "", ""
	c.mu.Unlock()
}

// SignIn вводит код.
func (c *Client) SignIn(ctx context.Context, code string) error {
	phone, hash, err := c.phoneAndHash()
	if err != nil {
		return err
	}
	code = normalizePhone(code) // только цифры
	v, err := c.cfg.DC.Invoke(ctx, &tl.AuthSignInRequest{PhoneNumber: phone, PhoneCodeHash: hash, PhoneCode: &code})
	if err != nil {
		if IsPasswordNeeded(err) {
			return c.passwordStep(ctx)
		}
		return err
	}
	a, ok := v.(tl.AuthAuthorizationClass)
	if !ok {
		return fmt.Errorf("auth: неожиданный ответ %T", v)
	}
	return c.finish(a)
}

// SignUp регистрирует новый аккаунт (Вх-5); условия приняты в интерфейсе (Вх-9).
func (c *Client) SignUp(ctx context.Context, first, last string) error {
	phone, hash, err := c.phoneAndHash()
	if err != nil {
		return err
	}
	v, err := c.cfg.DC.Invoke(ctx, &tl.AuthSignUpRequest{PhoneNumber: phone, PhoneCodeHash: hash,
		FirstName: first, LastName: last})
	if err != nil {
		return err
	}
	a, ok := v.(tl.AuthAuthorizationClass)
	if !ok {
		return fmt.Errorf("auth: неожиданный ответ %T", v)
	}
	return c.finish(a)
}

// CheckPassword проверяет облачный пароль по SRP (A и M1 считает ядро).
func (c *Client) CheckPassword(ctx context.Context, password []byte) error {
	v, err := c.cfg.DC.Invoke(ctx, &tl.AccountGetPasswordRequest{})
	if err != nil {
		return err
	}
	p, ok := v.(*tl.AccountPassword)
	if !ok || p.SrpId == nil || p.SrpB == nil {
		return errors.New("auth: у аккаунта нет облачного пароля")
	}
	algo, ok := p.CurrentAlgo.(*tl.PasswordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow)
	if !ok {
		return errors.New("auth: неизвестный алгоритм облачного пароля — нужна новая версия клиента")
	}
	a, m1, err := core.SRPCheck(password, core.SRPParams{Salt1: algo.Salt1, Salt2: algo.Salt2, G: uint32(algo.G),
		P: algo.P, SRPB: p.SrpB})
	if err != nil {
		return fmt.Errorf("auth: SRP: %w", err)
	}
	r, err := c.cfg.DC.Invoke(ctx, &tl.AuthCheckPasswordRequest{Password: &tl.InputCheckPasswordSRP{SrpId: *p.SrpId,
		A: a, M1: m1}})
	if err != nil {
		return err
	}
	auth, ok := r.(tl.AuthAuthorizationClass)
	if !ok {
		return fmt.Errorf("auth: неожиданный ответ %T", r)
	}
	return c.finish(auth)
}

// ---------------------------------------------------------------- выход

// LogOut завершает сессию на сервере и удаляет всё локальное: файл сессии,
// ключи и ключ обёртки в связке (Нс-6). Ошибка сервера не мешает локальной очистке.
func (c *Client) LogOut(ctx context.Context) error {
	c.stopQR()
	var serverErr error
	if c.UserID() != 0 {
		lctx, cancel := context.WithTimeout(ctx, 10*time.Second)
		_, serverErr = c.cfg.DC.Invoke(lctx, &tl.AuthLogOutRequest{})
		cancel()
		if IsUnauthorized(serverErr) {
			serverErr = nil // сессия уже завершена — результат тот же
		}
	}
	c.wipe()
	kerr := c.cfg.Keyring.Delete(ctx)
	c.mu.Lock()
	c.haveWrap = false
	c.wrap = [32]byte{}
	c.mu.Unlock()
	// Новый ключ обёртки для следующего входа.
	if wrap, _, err := c.cfg.Keyring.WrapKey(ctx, true); err == nil && len(wrap) == 32 {
		c.mu.Lock()
		copy(c.wrap[:], wrap)
		c.haveWrap = true
		c.mu.Unlock()
	}
	c.emit(State{Step: StepLoggedOut, Reason: ReasonLoggedOut})
	if serverErr != nil {
		return fmt.Errorf("auth: локальные данные удалены, но сервер не подтвердил выход: %w", serverErr)
	}
	return kerr
}
