// Package keyring — ключ обёртки auth_key в системной связке ключей
// (Secret Service по D-Bus, решение О-6; Архитектура/nuncio - Данные и
// хранилище.md, раздел 4). Хранится 32-байтный ключ, которым ядро
// запечатывает auth_key в session.bin; сам auth_key в связку не попадает.
//
// Без Secret Service приложение не запускается (О-6): Open возвращает
// ErrUnavailable, интерфейс показывает критический экран (Со-4).
package keyring

import (
	"context"
	"errors"
	"fmt"

	"github.com/godbus/dbus/v5"

	"nuncio/net/core"
)

const (
	service      = "org.freedesktop.secrets"
	servicePath  = dbus.ObjectPath("/org/freedesktop/secrets")
	ifService    = "org.freedesktop.Secret.Service"
	ifItem       = "org.freedesktop.Secret.Item"
	ifPrompt     = "org.freedesktop.Secret.Prompt"
	defaultAlias = "default"
	noPrompt     = dbus.ObjectPath("/")
)

// ErrUnavailable — нет сессионной шины или службы Secret Service.
var ErrUnavailable = errors.New("keyring: связка ключей (Secret Service) недоступна")

// ErrDismissed — пользователь отказался разблокировать связку.
var ErrDismissed = errors.New("keyring: разблокировка связки ключей отменена")

// secret — структура Secret из спецификации: (oayays).
type secret struct {
	Session     dbus.ObjectPath
	Parameters  []byte
	Value       []byte
	ContentType string
}

// Keyring — соединение с Secret Service.
type Keyring struct {
	conn    *dbus.Conn
	svc     dbus.BusObject
	session dbus.ObjectPath
	attrs   map[string]string
	// Collection — куда класть новый ключ; пусто — связка по умолчанию
	// (alias default, обычно «Login»; если заблокирована, служба спросит пароль).
	Collection dbus.ObjectPath
}

// Open подключается к Secret Service. app различает записи (в тестах —
// отдельное имя, чтобы не трогать настоящий ключ приложения).
func Open(app string) (*Keyring, error) {
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrUnavailable, err)
	}
	var owner string
	if err := conn.BusObject().Call("org.freedesktop.DBus.GetNameOwner", 0, service).Store(&owner); err != nil {
		// Служба может запускаться по требованию (activatable).
		var ok bool
		if err2 := conn.BusObject().Call("org.freedesktop.DBus.NameHasOwner", 0, service).Store(&ok); err2 != nil || !ok {
			var activatable []string
			_ = conn.BusObject().Call("org.freedesktop.DBus.ListActivatableNames", 0).Store(&activatable)
			found := false
			for _, n := range activatable {
				if n == service {
					found = true
				}
			}
			if !found {
				conn.Close()
				return nil, ErrUnavailable
			}
		}
	}
	k := &Keyring{conn: conn, svc: conn.Object(service, servicePath),
		attrs: map[string]string{"application": app, "purpose": "session-wrap-key"}}
	var out dbus.Variant
	if err := k.svc.Call(ifService+".OpenSession", 0, "plain", dbus.MakeVariant("")).Store(&out, &k.session); err != nil {
		conn.Close()
		return nil, fmt.Errorf("%w: OpenSession: %v", ErrUnavailable, err)
	}
	return k, nil
}

// Close закрывает сессию и соединение.
func (k *Keyring) Close() {
	if k.session != "" {
		_ = k.conn.Object(service, k.session).Call("org.freedesktop.Secret.Session.Close", 0).Err
	}
	k.conn.Close()
}

// prompt выполняет запрос разблокировки и ждёт ответа пользователя.
func (k *Keyring) prompt(ctx context.Context, p dbus.ObjectPath) (dbus.Variant, error) {
	if p == noPrompt || p == "" {
		return dbus.Variant{}, nil
	}
	if err := k.conn.AddMatchSignal(dbus.WithMatchObjectPath(p), dbus.WithMatchInterface(ifPrompt),
		dbus.WithMatchMember("Completed")); err != nil {
		return dbus.Variant{}, err
	}
	defer func() {
		_ = k.conn.RemoveMatchSignal(dbus.WithMatchObjectPath(p), dbus.WithMatchInterface(ifPrompt),
			dbus.WithMatchMember("Completed"))
	}()
	ch := make(chan *dbus.Signal, 4)
	k.conn.Signal(ch)
	defer k.conn.RemoveSignal(ch)
	if err := k.conn.Object(service, p).Call(ifPrompt+".Prompt", 0, "").Err; err != nil {
		return dbus.Variant{}, err
	}
	for {
		select {
		case <-ctx.Done():
			_ = k.conn.Object(service, p).Call(ifPrompt+".Dismiss", 0).Err
			return dbus.Variant{}, ctx.Err()
		case s := <-ch:
			if s.Path != p || s.Name != ifPrompt+".Completed" || len(s.Body) < 2 {
				continue
			}
			if dismissed, _ := s.Body[0].(bool); dismissed {
				return dbus.Variant{}, ErrDismissed
			}
			v, _ := s.Body[1].(dbus.Variant)
			return v, nil
		}
	}
}

func (k *Keyring) unlock(ctx context.Context, objs []dbus.ObjectPath) error {
	if len(objs) == 0 {
		return nil
	}
	var unlocked []dbus.ObjectPath
	var p dbus.ObjectPath
	if err := k.svc.Call(ifService+".Unlock", 0, objs).Store(&unlocked, &p); err != nil {
		return err
	}
	_, err := k.prompt(ctx, p)
	return err
}

func (k *Keyring) find(ctx context.Context) ([]dbus.ObjectPath, error) {
	var unlocked, locked []dbus.ObjectPath
	if err := k.svc.Call(ifService+".SearchItems", 0, k.attrs).Store(&unlocked, &locked); err != nil {
		return nil, err
	}
	if len(locked) == 0 {
		return unlocked, nil
	}
	if err := k.unlock(ctx, locked); err != nil {
		return nil, err
	}
	return append(unlocked, locked...), nil
}

// WrapKey возвращает ключ обёртки; если его нет и create — создаёт новый.
// found=false и create=false — ключа нет (сессию прочитать нельзя).
func (k *Keyring) WrapKey(ctx context.Context, create bool) (key []byte, found bool, err error) {
	items, err := k.find(ctx)
	if err != nil {
		return nil, false, err
	}
	for _, it := range items {
		var s secret
		if err := k.conn.Object(service, it).Call(ifItem+".GetSecret", 0, k.session).Store(&s); err != nil {
			continue
		}
		if len(s.Value) == 32 {
			return s.Value, true, nil
		}
	}
	if !create {
		return nil, false, nil
	}
	key = make([]byte, 32)
	if err := core.Random(key); err != nil {
		return nil, false, err
	}
	coll := k.Collection
	if coll == "" {
		if err := k.svc.Call(ifService+".ReadAlias", 0, defaultAlias).Store(&coll); err != nil {
			return nil, false, err
		}
	}
	if coll == noPrompt || coll == "" {
		// Связки по умолчанию нет — просим службу создать (обычно с запросом пароля).
		props := map[string]dbus.Variant{"org.freedesktop.Secret.Collection.Label": dbus.MakeVariant("Login")}
		var p dbus.ObjectPath
		if err := k.svc.Call(ifService+".CreateCollection", 0, props, defaultAlias).Store(&coll, &p); err != nil {
			return nil, false, err
		}
		if coll == noPrompt {
			v, err := k.prompt(ctx, p)
			if err != nil {
				return nil, false, err
			}
			coll, _ = v.Value().(dbus.ObjectPath)
		}
	}
	if err := k.unlock(ctx, []dbus.ObjectPath{coll}); err != nil {
		return nil, false, err
	}
	props := map[string]dbus.Variant{
		"org.freedesktop.Secret.Item.Label":      dbus.MakeVariant("nuncio: ключ файла сессии"),
		"org.freedesktop.Secret.Item.Attributes": dbus.MakeVariant(k.attrs),
	}
	sec := secret{Session: k.session, Value: key, ContentType: "application/octet-stream"}
	var item, p dbus.ObjectPath
	if err := k.conn.Object(service, coll).Call("org.freedesktop.Secret.Collection.CreateItem", 0,
		props, sec, true).Store(&item, &p); err != nil {
		return nil, false, err
	}
	if _, err := k.prompt(ctx, p); err != nil {
		return nil, false, err
	}
	return key, true, nil
}

// Delete удаляет ключ обёртки (выход из аккаунта, Нс-6).
func (k *Keyring) Delete(ctx context.Context) error {
	items, err := k.find(ctx)
	if err != nil {
		return err
	}
	for _, it := range items {
		var p dbus.ObjectPath
		if err := k.conn.Object(service, it).Call(ifItem+".Delete", 0).Store(&p); err != nil {
			return err
		}
		if _, err := k.prompt(ctx, p); err != nil {
			return err
		}
	}
	return nil
}
