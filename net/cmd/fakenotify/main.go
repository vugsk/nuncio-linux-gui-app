// Поддельный сервер уведомлений рабочего стола (org.freedesktop.Notifications)
// для сквозных проверок: занимает имя на шине из DBUS_SESSION_BUS_ADDRESS,
// печатает каждый вызов Notify; по SIGUSR1 «щёлкает» по последнему
// уведомлению (сигнал ActionInvoked "default").
//
//	dbus-run-session -- go run ./cmd/fakenotify
package main

import (
	"fmt"
	"os"
	"os/signal"
	"sync"
	"syscall"

	"github.com/godbus/dbus/v5"
)

const (
	name    = "org.freedesktop.Notifications"
	objPath = "/org/freedesktop/Notifications"
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
	return "fakenotify", "nuncio", "1", "1.2", nil
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

func main() {
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
	fmt.Println("готов")
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGUSR1, syscall.SIGTERM, syscall.SIGINT)
	for x := range sig {
		if x != syscall.SIGUSR1 {
			return
		}
		s.mu.Lock()
		id := s.last
		s.mu.Unlock()
		fmt.Println("щелчок", id)
		_ = conn.Emit(objPath, name+".ActionInvoked", id, "default")
	}
}
