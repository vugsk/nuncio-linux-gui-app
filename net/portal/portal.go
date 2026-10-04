// Package portal — системные диалоги выбора файлов через xdg-desktop-portal
// (org.freedesktop.portal.FileChooser, Св-4): открыть файлы для отправки,
// сохранить файл из кэша. Диалог рисует окружение рабочего стола; ответ
// приходит сигналом Response объекта запроса.
package portal

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"strings"

	"github.com/godbus/dbus/v5"
)

const (
	dest    = "org.freedesktop.portal.Desktop"
	objPath = "/org/freedesktop/portal/desktop"
	iface   = "org.freedesktop.portal.FileChooser"
)

// ErrCancelled — пользователь закрыл диалог.
var ErrCancelled = errors.New("portal: отменено")

// OpenFiles — выбрать файлы (multiple — несколько) или папку (directory). Пути локальные.
func OpenFiles(ctx context.Context, title string, multiple, directory bool) ([]string, error) {
	return call(ctx, "OpenFile", title, map[string]dbus.Variant{"multiple": dbus.MakeVariant(multiple),
		"directory": dbus.MakeVariant(directory)})
}

// SaveFile — выбрать, куда сохранить файл с предложенным именем.
func SaveFile(ctx context.Context, title, name string) (string, error) {
	paths, err := call(ctx, "SaveFile", title, map[string]dbus.Variant{"current_name": dbus.MakeVariant(name)})
	if err != nil {
		return "", err
	}
	if len(paths) == 0 {
		return "", ErrCancelled
	}
	return paths[0], nil
}

func call(ctx context.Context, method, title string, opts map[string]dbus.Variant) ([]string, error) {
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		return nil, fmt.Errorf("portal: сессионная шина: %w", err)
	}
	defer conn.Close()
	var tb [8]byte
	if _, err := rand.Read(tb[:]); err != nil {
		return nil, err
	}
	token := "nuncio" + hex.EncodeToString(tb[:])
	opts["handle_token"] = dbus.MakeVariant(token)
	// Путь запроса известен заранее: подписка до вызова, иначе быстрый ответ теряется.
	sender := strings.ReplaceAll(strings.TrimPrefix(conn.Names()[0], ":"), ".", "_")
	req := dbus.ObjectPath(objPath + "/request/" + sender + "/" + token)
	if err := conn.AddMatchSignal(dbus.WithMatchObjectPath(req), dbus.WithMatchInterface("org.freedesktop.portal.Request"),
		dbus.WithMatchMember("Response")); err != nil {
		return nil, err
	}
	sig := make(chan *dbus.Signal, 4)
	conn.Signal(sig)
	var handle dbus.ObjectPath
	if err := conn.Object(dest, objPath).CallWithContext(ctx, iface+"."+method, 0, "", title, opts).Store(&handle); err != nil {
		return nil, fmt.Errorf("portal: %s: %w", method, err)
	}
	for {
		select {
		case <-ctx.Done():
			_ = conn.Object(dest, handle).Call("org.freedesktop.portal.Request.Close", 0).Err
			return nil, ctx.Err()
		case s := <-sig:
			if s == nil || (s.Path != handle && s.Path != req) || len(s.Body) < 2 {
				continue
			}
			code, _ := s.Body[0].(uint32)
			if code != 0 {
				return nil, ErrCancelled
			}
			results, _ := s.Body[1].(map[string]dbus.Variant)
			var uris []string
			if v, ok := results["uris"]; ok {
				_ = v.Store(&uris)
			}
			return paths(uris), nil
		}
	}
}

// paths — file:// адреса в пути (прочие схемы пропускаются).
func paths(uris []string) []string {
	var out []string
	for _, u := range uris {
		p, err := url.Parse(u)
		if err != nil || p.Scheme != "file" {
			continue
		}
		out = append(out, p.Path)
	}
	return out
}
