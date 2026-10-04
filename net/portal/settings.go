package portal

import (
	"context"

	"github.com/godbus/dbus/v5"
)

// Тёмная или светлая тема рабочего стола (org.freedesktop.appearance
// color-scheme: 0 — не задано, 1 — тёмная, 2 — светлая) для темы «Как в
// системе» (Дз-1). Без портала — молча ничего.

const settingsIface = "org.freedesktop.portal.Settings"

// WatchColorScheme сообщает текущее значение и каждую смену, пока жив ctx.
func WatchColorScheme(ctx context.Context, report func(int32)) {
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		return
	}
	defer conn.Close()
	var v dbus.Variant
	err = conn.Object(dest, objPath).CallWithContext(ctx, settingsIface+".ReadOne", 0,
		"org.freedesktop.appearance", "color-scheme").Store(&v)
	if err == nil {
		report(scheme(v))
	}
	if err := conn.AddMatchSignal(dbus.WithMatchInterface(settingsIface), dbus.WithMatchMember("SettingChanged")); err != nil {
		return
	}
	sig := make(chan *dbus.Signal, 8)
	conn.Signal(sig)
	for {
		select {
		case <-ctx.Done():
			return
		case s := <-sig:
			if s == nil || len(s.Body) < 3 {
				continue
			}
			ns, _ := s.Body[0].(string)
			key, _ := s.Body[1].(string)
			if ns == "org.freedesktop.appearance" && key == "color-scheme" {
				if v, ok := s.Body[2].(dbus.Variant); ok {
					report(scheme(v))
				}
			}
		}
	}
}

func scheme(v dbus.Variant) int32 {
	// ReadOne отдаёт uint32; старый Read — вариант в варианте.
	switch x := v.Value().(type) {
	case uint32:
		return int32(x)
	case dbus.Variant:
		return scheme(x)
	}
	return 0
}
