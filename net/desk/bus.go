// Package desk — шина рабочего стола для уведомлений и трея: сессионная, или
// NUNCIO_DESKTOP_BUS (сквозные проверки поднимают свою шину с поддельными
// серверами), "off" — ни уведомлений, ни трея.
package desk

import (
	"errors"
	"os"

	"github.com/godbus/dbus/v5"
)

// ErrOff — выключено переменной окружения.
var ErrOff = errors.New("desk: выключено NUNCIO_DESKTOP_BUS=off")

// Bus — новое соединение с шиной рабочего стола.
func Bus() (*dbus.Conn, error) {
	switch addr := os.Getenv("NUNCIO_DESKTOP_BUS"); addr {
	case "":
		return dbus.ConnectSessionBus()
	case "off":
		return nil, ErrOff
	default:
		return dbus.Connect(addr)
	}
}
