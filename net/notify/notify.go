// Package notify — уведомления рабочего стола через
// org.freedesktop.Notifications (В9, Нс-4). Одно уведомление на чат: новое
// заменяет прежнее (replaces_id). Щелчок по уведомлению (действие "default")
// сообщается обратно номером чата.
//
// Шина — сессионная; NUNCIO_NOTIFY_BUS задаёт другой адрес (сквозные проверки
// поднимают свою шину с поддельным сервером уведомлений), "off" — не показывать.
package notify

import (
	"context"
	"os"
	"sync"

	"github.com/godbus/dbus/v5"
)

const (
	dest    = "org.freedesktop.Notifications"
	objPath = "/org/freedesktop/Notifications"
	iface   = "org.freedesktop.Notifications"
	appName = "nuncio"
	// Через сколько мс сервер скрывает уведомление сам (-1 — по своему усмотрению).
	timeout = int32(-1)
)

// Notifier держит соединение и соответствие «уведомление ↔ чат».
type Notifier struct {
	clicked func(peer int64)

	mu     sync.Mutex
	conn   *dbus.Conn
	failed bool
	markup bool // сервер понимает разметку в теле
	byPeer map[int64]uint32
	byID   map[uint32]int64
}

// New — clicked вызывается из горутины слушателя при щелчке.
func New(clicked func(peer int64)) *Notifier {
	return &Notifier{clicked: clicked, byPeer: map[int64]uint32{}, byID: map[uint32]int64{}}
}

func connect() (*dbus.Conn, error) {
	addr := os.Getenv("NUNCIO_NOTIFY_BUS")
	if addr == "" {
		return dbus.ConnectSessionBus()
	}
	return dbus.Connect(addr)
}

// Off — уведомления выключены переменной окружения.
func Off() bool { return os.Getenv("NUNCIO_NOTIFY_BUS") == "off" }

// conn — соединение, при первом обращении — с подпиской на сигналы.
// Без шины или сервера уведомлений — nil, и больше не пытаться.
func (n *Notifier) connLocked(ctx context.Context) *dbus.Conn {
	if n.conn != nil || n.failed {
		return n.conn
	}
	if Off() {
		n.failed = true
		return nil
	}
	conn, err := connect()
	if err != nil {
		n.failed = true
		return nil
	}
	for _, m := range []string{"ActionInvoked", "NotificationClosed"} {
		if err := conn.AddMatchSignal(dbus.WithMatchObjectPath(objPath), dbus.WithMatchInterface(iface),
			dbus.WithMatchMember(m)); err != nil {
			_ = conn.Close()
			n.failed = true
			return nil
		}
	}
	var caps []string
	_ = conn.Object(dest, objPath).CallWithContext(ctx, iface+".GetCapabilities", 0).Store(&caps)
	for _, c := range caps {
		n.markup = n.markup || c == "body-markup"
	}
	sig := make(chan *dbus.Signal, 16)
	conn.Signal(sig)
	n.conn = conn
	go n.listen(ctx, conn, sig)
	return conn
}

func (n *Notifier) listen(ctx context.Context, conn *dbus.Conn, sig chan *dbus.Signal) {
	defer func() { _ = conn.Close() }()
	for {
		select {
		case <-ctx.Done():
			return
		case s, ok := <-sig:
			if !ok {
				return
			}
			if s == nil || len(s.Body) < 2 {
				continue
			}
			id, _ := s.Body[0].(uint32)
			switch s.Name {
			case iface + ".ActionInvoked":
				action, _ := s.Body[1].(string)
				n.mu.Lock()
				peer, known := n.byID[id]
				n.mu.Unlock()
				if known && action == "default" {
					n.clicked(peer)
				}
			case iface + ".NotificationClosed":
				n.forget(id)
			}
		}
	}
}

func (n *Notifier) forget(id uint32) {
	n.mu.Lock()
	defer n.mu.Unlock()
	if peer, ok := n.byID[id]; ok {
		delete(n.byID, id)
		if n.byPeer[peer] == id {
			delete(n.byPeer, peer)
		}
	}
}

// Show — показать (или заменить) уведомление чата peer. Ошибки молча: без
// сервера уведомлений приложение работает как прежде.
func (n *Notifier) Show(ctx context.Context, peer int64, title, body string, sound bool) {
	n.mu.Lock()
	conn := n.connLocked(ctx)
	replaces := n.byPeer[peer]
	if n.markup {
		body = escape(body)
	}
	n.mu.Unlock()
	if conn == nil {
		return
	}
	hints := map[string]dbus.Variant{
		"category":      dbus.MakeVariant("im.received"),
		"desktop-entry": dbus.MakeVariant(appName),
		"urgency":       dbus.MakeVariant(byte(1)),
	}
	if sound {
		hints["sound-name"] = dbus.MakeVariant("message-new-instant")
	} else {
		hints["suppress-sound"] = dbus.MakeVariant(true)
	}
	var id uint32
	err := conn.Object(dest, objPath).CallWithContext(ctx, iface+".Notify", 0, appName, replaces, "",
		title, body, []string{"default", ""}, hints, timeout).Store(&id)
	if err != nil {
		return
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if replaces != 0 && replaces != id {
		delete(n.byID, replaces)
	}
	n.byPeer[peer] = id
	n.byID[id] = peer
}

// escape — если сервер понимает разметку (<b>, &amp;), текст сообщения
// показывается как есть, а не как разметка.
func escape(s string) string {
	out := make([]rune, 0, len(s))
	for _, r := range s {
		switch r {
		case '&':
			out = append(out, []rune("&amp;")...)
		case '<':
			out = append(out, []rune("&lt;")...)
		case '>':
			out = append(out, []rune("&gt;")...)
		default:
			out = append(out, r)
		}
	}
	return string(out)
}
