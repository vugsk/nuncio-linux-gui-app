// Поддельный рабочий стол для сквозных проверок на своей шине
// (DBUS_SESSION_BUS_ADDRESS): сервер уведомлений
// (org.freedesktop.Notifications) и, с -tray, наблюдатель значков в трее
// (org.kde.StatusNotifierWatcher) с хостом (-host=false — без хоста).
// Печатает вызовы Notify и состояние значка; сигналы:
//
//	SIGUSR1 — щелчок по последнему уведомлению (ActionInvoked "default");
//	SIGUSR2 — щелчок по значку (Activate);
//	SIGHUP  — пункт меню значка «Выйти» (dbusmenu Event 3 "clicked").
//
//	dbus-run-session -- go run ./cmd/fakedesk -tray
package main

import (
	"flag"
	"fmt"
	"os"
	"os/signal"
	"strings"
	"sync"
	"syscall"

	"github.com/godbus/dbus/v5"
	"github.com/godbus/dbus/v5/prop"
)

const (
	name    = "org.freedesktop.Notifications"
	objPath = "/org/freedesktop/Notifications"

	watcherName = "org.kde.StatusNotifierWatcher"
	watcherPath = "/StatusNotifierWatcher"
	itemIface   = "org.kde.StatusNotifierItem"
)

type server struct {
	conn *dbus.Conn
	mu   sync.Mutex
	next uint32
	last uint32
}

func (s *server) GetCapabilities() ([]string, *dbus.Error) {
	return []string{"body", "body-markup", "actions", "sound"}, nil
}

func (s *server) GetServerInformation() (string, string, string, string, *dbus.Error) {
	return "fakedesk", "nuncio", "1", "1.2", nil
}

func (s *server) Notify(app string, replaces uint32, icon, summary, body string, actions []string,
	hints map[string]dbus.Variant, timeout int32) (uint32, *dbus.Error) {
	s.mu.Lock()
	id := replaces
	if id == 0 {
		s.next++
		id = s.next
	}
	s.last = id
	s.mu.Unlock()
	_, sound := hints["sound-name"]
	fmt.Printf("уведомление id=%d replaces=%d app=%s title=%s body=%s sound=%v actions=%v\n",
		id, replaces, app, summary, body, sound, actions)
	return id, nil
}

func (s *server) CloseNotification(id uint32) *dbus.Error {
	if err := s.conn.Emit(objPath, name+".NotificationClosed", id, uint32(3)); err != nil {
		return dbus.MakeFailedError(err)
	}
	return nil
}

// watcher — наблюдатель значков: помнит последний зарегистрированный.
type watcher struct {
	conn *dbus.Conn
	mu   sync.Mutex
	item string // уникальное имя владельца значка
}

func (w *watcher) RegisterStatusNotifierItem(sender dbus.Sender, service string) *dbus.Error {
	w.mu.Lock()
	w.item = string(sender)
	w.mu.Unlock()
	fmt.Println("трей: зарегистрирован", strings.HasPrefix(service, "org.kde.StatusNotifierItem-"))
	go w.describe()
	return nil
}

func (w *watcher) RegisterStatusNotifierHost(service string) *dbus.Error { return nil }

func (w *watcher) obj() dbus.BusObject {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.conn.Object(w.item, "/StatusNotifierItem")
}

type pixmap struct {
	W, H int32
	Data []byte
}

type tooltip struct {
	Icon  string
	Pix   []pixmap
	Title string
	Body  string
}

type layout struct {
	ID       int32
	Props    map[string]dbus.Variant
	Children []dbus.Variant
}

// describe — прочитать значок, подсказку и меню, как это делает хост.
func (w *watcher) describe() {
	o := w.obj()
	var pix []pixmap
	if v, err := o.GetProperty(itemIface + ".IconPixmap"); err == nil {
		_ = v.Store(&pix)
	}
	var sizes []string
	for _, p := range pix {
		ok := len(p.Data) == int(p.W*p.H*4)
		sizes = append(sizes, fmt.Sprintf("%d:%v", p.W, ok))
	}
	var tip tooltip
	if v, err := o.GetProperty(itemIface + ".ToolTip"); err == nil {
		_ = v.Store(&tip)
	}
	var menu dbus.ObjectPath
	if v, err := o.GetProperty(itemIface + ".Menu"); err == nil {
		_ = v.Store(&menu)
	}
	var labels []string
	var rev uint32
	var root layout
	if err := w.conn.Object(o.Destination(), menu).Call("com.canonical.dbusmenu.GetLayout", 0, int32(0), int32(-1),
		[]string{}).Store(&rev, &root); err == nil {
		for _, c := range root.Children {
			var l layout
			if err := dbus.Store([]any{c.Value()}, &l); err == nil {
				if v, ok := l.Props["label"]; ok {
					labels = append(labels, v.Value().(string))
				}
			}
		}
	}
	fmt.Printf("трей: значок %s подсказка=%q меню=%s\n", strings.Join(sizes, ","), tip.Body, strings.Join(labels, "|"))
}

func main() {
	tray := flag.Bool("tray", false, "наблюдатель значков в трее")
	host := flag.Bool("host", true, "с -tray: хост значков есть")
	flag.Parse()
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	s := &server{conn: conn}
	if err := conn.Export(s, objPath, name); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if r, err := conn.RequestName(name, dbus.NameFlagDoNotQueue); err != nil || r != dbus.RequestNameReplyPrimaryOwner {
		fmt.Fprintln(os.Stderr, "имя занято", err)
		os.Exit(1)
	}
	w := &watcher{conn: conn}
	if *tray {
		if err := conn.Export(w, watcherPath, watcherName); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		if _, err := prop.Export(conn, watcherPath, prop.Map{watcherName: {
			"IsStatusNotifierHostRegistered": {Value: *host, Emit: prop.EmitConst},
			"ProtocolVersion":                {Value: int32(0), Emit: prop.EmitConst},
			"RegisteredStatusNotifierItems":  {Value: []string{}, Emit: prop.EmitFalse},
		}}); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		if r, err := conn.RequestName(watcherName, dbus.NameFlagDoNotQueue); err != nil || r != dbus.RequestNameReplyPrimaryOwner {
			fmt.Fprintln(os.Stderr, "имя наблюдателя занято", err)
			os.Exit(1)
		}
		// Смена значка или подсказки — перечитать.
		_ = conn.AddMatchSignal(dbus.WithMatchInterface(itemIface))
		sig := make(chan *dbus.Signal, 16)
		conn.Signal(sig)
		go func() {
			for range sig {
				w.describe()
			}
		}()
	}
	fmt.Println("готов")
	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGUSR1, syscall.SIGUSR2, syscall.SIGHUP, syscall.SIGTERM, syscall.SIGINT)
	for x := range sigs {
		switch x {
		case syscall.SIGUSR1:
			s.mu.Lock()
			id := s.last
			s.mu.Unlock()
			fmt.Println("щелчок", id)
			_ = conn.Emit(objPath, name+".ActionInvoked", id, "default")
		case syscall.SIGUSR2:
			err := w.obj().Call(itemIface+".Activate", 0, int32(0), int32(0)).Err
			fmt.Println("трей: щелчок по значку", err == nil)
		case syscall.SIGHUP:
			o := w.obj()
			var menu dbus.ObjectPath
			if v, err := o.GetProperty(itemIface + ".Menu"); err == nil {
				_ = v.Store(&menu)
			}
			err := conn.Object(o.Destination(), menu).Call("com.canonical.dbusmenu.Event", 0, int32(3), "clicked",
				dbus.MakeVariant(""), uint32(0)).Err
			fmt.Println("трей: меню «Выйти»", err == nil)
		default:
			return
		}
	}
}
