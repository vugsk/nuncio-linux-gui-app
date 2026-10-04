// Package tray — значок в системном трее (В9, Нс-4/Нс-5) по протоколу
// StatusNotifierItem: объект org.kde.StatusNotifierItem со счётчиком
// непрочитанных на картинке и меню com.canonical.dbusmenu («Открыть»,
// «Выйти»). Значок регистрируется у org.kde.StatusNotifierWatcher; есть ли
// хост, который его рисует, — свойство IsStatusNotifierHostRegistered.
// Без хоста закрытие окна завершает приложение (решает Rust по событию
// Unavailable).
//
// Шина — desk.Bus.
package tray

import (
	"context"
	"fmt"
	"os"
	"sync"

	"nuncio/net/desk"

	"github.com/godbus/dbus/v5"
	"github.com/godbus/dbus/v5/introspect"
	"github.com/godbus/dbus/v5/prop"
)

const (
	itemPath    = dbus.ObjectPath("/StatusNotifierItem")
	itemIface   = "org.kde.StatusNotifierItem"
	menuPath    = dbus.ObjectPath("/MenuBar")
	menuIface   = "com.canonical.dbusmenu"
	watcherName = "org.kde.StatusNotifierWatcher"
	watcherPath = dbus.ObjectPath("/StatusNotifierWatcher")
)

// Kind — что сообщить приложению.
type Kind int

const (
	Available   Kind = iota + 1 // хост значка есть: закрытие окна может прятать его
	Unavailable                 // хоста нет или трей выключен
	Toggle                      // щелчок по значку: показать или спрятать окно
	Show                        // «Открыть» в меню
	Quit                        // «Выйти» в меню
)

// State — желаемое состояние значка.
type State struct {
	Enabled   bool
	Unread    int32
	Tooltip   string
	ShowLabel string
	QuitLabel string
}

// Tray — значок; методы безопасны из разных горутин.
type Tray struct {
	report func(Kind)

	mu     sync.Mutex
	st     State
	conn   *dbus.Conn
	props  *prop.Properties
	rev    uint32
	avail  bool
	cancel context.CancelFunc
}

// New — report вызывается из горутин трея.
func New(report func(Kind)) *Tray { return &Tray{report: report} }

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

func icons(unread int32) []pixmap {
	var out []pixmap
	for _, s := range []int{22, 32, 48, 64} {
		out = append(out, pixmap{int32(s), int32(s), render(s, unread)})
	}
	return out
}

// Set — применить состояние: включить, обновить счётчик и подписи, выключить.
func (t *Tray) Set(ctx context.Context, st State) {
	t.mu.Lock()
	defer t.mu.Unlock()
	old := t.st
	t.st = st
	switch {
	case !st.Enabled:
		t.stopLocked()
	case t.conn == nil:
		if err := t.startLocked(ctx); err != nil {
			t.stopLocked()
		}
	default:
		if old.Unread != st.Unread {
			t.props.SetMust(itemIface, "IconPixmap", icons(st.Unread))
			_ = t.conn.Emit(itemPath, itemIface+".NewIcon")
		}
		if old.Tooltip != st.Tooltip {
			t.props.SetMust(itemIface, "ToolTip", tooltip{Title: "nuncio", Body: st.Tooltip})
			_ = t.conn.Emit(itemPath, itemIface+".NewToolTip")
		}
		if old.ShowLabel != st.ShowLabel || old.QuitLabel != st.QuitLabel {
			t.rev++
			_ = t.conn.Emit(menuPath, menuIface+".LayoutUpdated", t.rev, int32(0))
		}
	}
}

func (t *Tray) stopLocked() {
	if t.cancel != nil {
		t.cancel()
		t.cancel = nil
	}
	if t.conn != nil {
		_ = t.conn.Close()
		t.conn = nil
	}
	t.setAvailLocked(false)
}

func (t *Tray) setAvailLocked(v bool) {
	if t.avail == v {
		return
	}
	t.avail = v
	if v {
		go t.report(Available)
	} else {
		go t.report(Unavailable)
	}
}

func (t *Tray) startLocked(ctx context.Context) error {
	conn, err := desk.Bus()
	if err != nil {
		return err
	}
	t.conn = conn
	name := fmt.Sprintf("org.kde.StatusNotifierItem-%d-1", os.Getpid())
	if _, err := conn.RequestName(name, dbus.NameFlagDoNotQueue); err != nil {
		return err
	}
	if err := conn.Export(item{t}, itemPath, itemIface); err != nil {
		return err
	}
	props, err := prop.Export(conn, itemPath, prop.Map{itemIface: {
		"Category":            {Value: "Communications", Emit: prop.EmitConst},
		"Id":                  {Value: "nuncio", Emit: prop.EmitConst},
		"Title":               {Value: "nuncio", Emit: prop.EmitConst},
		"Status":              {Value: "Active", Emit: prop.EmitFalse},
		"WindowId":            {Value: int32(0), Emit: prop.EmitConst},
		"IconName":            {Value: "", Emit: prop.EmitConst},
		"IconThemePath":       {Value: "", Emit: prop.EmitConst},
		"IconPixmap":          {Value: icons(t.st.Unread), Emit: prop.EmitFalse},
		"OverlayIconName":     {Value: "", Emit: prop.EmitConst},
		"OverlayIconPixmap":   {Value: []pixmap{}, Emit: prop.EmitConst},
		"AttentionIconName":   {Value: "", Emit: prop.EmitConst},
		"AttentionIconPixmap": {Value: []pixmap{}, Emit: prop.EmitConst},
		"AttentionMovieName":  {Value: "", Emit: prop.EmitConst},
		"ToolTip":             {Value: tooltip{Title: "nuncio", Body: t.st.Tooltip}, Emit: prop.EmitFalse},
		"ItemIsMenu":          {Value: false, Emit: prop.EmitConst},
		"Menu":                {Value: menuPath, Emit: prop.EmitConst},
	}})
	if err != nil {
		return err
	}
	t.props = props
	if err := conn.Export(introspect.Introspectable(itemXML), itemPath, "org.freedesktop.DBus.Introspectable"); err != nil {
		return err
	}
	if err := conn.Export(menu{t}, menuPath, menuIface); err != nil {
		return err
	}
	if _, err := prop.Export(conn, menuPath, prop.Map{menuIface: {
		"Version":       {Value: uint32(3), Emit: prop.EmitConst},
		"TextDirection": {Value: "ltr", Emit: prop.EmitConst},
		"Status":        {Value: "normal", Emit: prop.EmitConst},
		"IconThemePath": {Value: []string{}, Emit: prop.EmitConst},
	}}); err != nil {
		return err
	}
	if err := conn.Export(introspect.Introspectable(menuXML), menuPath, "org.freedesktop.DBus.Introspectable"); err != nil {
		return err
	}
	// Наблюдатель может появиться, пропасть, а хост — прийти позже.
	if err := conn.AddMatchSignal(dbus.WithMatchSender("org.freedesktop.DBus"), dbus.WithMatchMember("NameOwnerChanged"),
		dbus.WithMatchArg(0, watcherName)); err != nil {
		return err
	}
	if err := conn.AddMatchSignal(dbus.WithMatchInterface(watcherName)); err != nil {
		return err
	}
	sig := make(chan *dbus.Signal, 16)
	conn.Signal(sig)
	cctx, cancel := context.WithCancel(ctx)
	t.cancel = cancel
	go t.watch(cctx, conn, name, sig)
	return nil
}

// register — зарегистрироваться у наблюдателя и узнать, есть ли хост.
func (t *Tray) register(ctx context.Context, conn *dbus.Conn, name string) {
	w := conn.Object(watcherName, watcherPath)
	host := false
	if err := w.CallWithContext(ctx, watcherName+".RegisterStatusNotifierItem", 0, name).Err; err == nil {
		v, err := w.GetProperty(watcherName + ".IsStatusNotifierHostRegistered")
		host = err == nil && v.Value() == true
	}
	t.mu.Lock()
	if t.conn == conn {
		t.setAvailLocked(host)
	}
	t.mu.Unlock()
}

func (t *Tray) watch(ctx context.Context, conn *dbus.Conn, name string, sig chan *dbus.Signal) {
	t.register(ctx, conn, name)
	for {
		select {
		case <-ctx.Done():
			return
		case s, ok := <-sig:
			if !ok {
				t.mu.Lock()
				if t.conn == conn {
					t.setAvailLocked(false)
				}
				t.mu.Unlock()
				return
			}
			switch s.Name {
			case "org.freedesktop.DBus.NameOwnerChanged":
				if len(s.Body) == 3 {
					if owner, _ := s.Body[2].(string); owner != "" {
						t.register(ctx, conn, name)
						continue
					}
				}
				t.mu.Lock()
				if t.conn == conn {
					t.setAvailLocked(false)
				}
				t.mu.Unlock()
			case watcherName + ".StatusNotifierHostRegistered", watcherName + ".StatusNotifierHostUnregistered":
				t.register(ctx, conn, name)
			}
		}
	}
}

// item — методы org.kde.StatusNotifierItem.
type item struct{ t *Tray }

func (i item) Activate(x, y int32) *dbus.Error {
	go i.t.report(Toggle)
	return nil
}

func (i item) SecondaryActivate(x, y int32) *dbus.Error {
	go i.t.report(Show)
	return nil
}

func (i item) ContextMenu(x, y int32) *dbus.Error { return nil }

func (i item) Scroll(delta int32, orientation string) *dbus.Error { return nil }

// menu — com.canonical.dbusmenu: корень 0, «Открыть» 1, черта 2, «Выйти» 3.
type menu struct{ t *Tray }

type layout struct {
	ID       int32
	Props    map[string]dbus.Variant
	Children []dbus.Variant
}

type groupProps struct {
	ID    int32
	Props map[string]dbus.Variant
}

type menuEvent struct {
	ID        int32
	EventID   string
	Data      dbus.Variant
	Timestamp uint32
}

func (m menu) items() []groupProps {
	m.t.mu.Lock()
	st := m.t.st
	m.t.mu.Unlock()
	return []groupProps{
		{1, map[string]dbus.Variant{"label": dbus.MakeVariant(st.ShowLabel)}},
		{2, map[string]dbus.Variant{"type": dbus.MakeVariant("separator")}},
		{3, map[string]dbus.Variant{"label": dbus.MakeVariant(st.QuitLabel)}},
	}
}

func (m menu) GetLayout(parent, depth int32, names []string) (uint32, layout, *dbus.Error) {
	m.t.mu.Lock()
	rev := m.t.rev
	m.t.mu.Unlock()
	root := layout{ID: 0, Props: map[string]dbus.Variant{"children-display": dbus.MakeVariant("submenu")}}
	for _, it := range m.items() {
		l := layout{ID: it.ID, Props: it.Props, Children: []dbus.Variant{}}
		if parent == it.ID {
			return rev, l, nil
		}
		root.Children = append(root.Children, dbus.MakeVariant(l))
	}
	if depth == 0 {
		root.Children = []dbus.Variant{}
	}
	return rev, root, nil
}

func (m menu) GetGroupProperties(ids []int32, names []string) ([]groupProps, *dbus.Error) {
	var out []groupProps
	for _, it := range m.items() {
		for _, id := range ids {
			if id == it.ID {
				out = append(out, it)
			}
		}
	}
	return out, nil
}

func (m menu) GetProperty(id int32, name string) (dbus.Variant, *dbus.Error) {
	for _, it := range m.items() {
		if v, ok := it.Props[name]; ok && it.ID == id {
			return v, nil
		}
	}
	return dbus.MakeVariant(""), nil
}

func (m menu) Event(id int32, eventID string, data dbus.Variant, ts uint32) *dbus.Error {
	if eventID != "clicked" {
		return nil
	}
	switch id {
	case 1:
		go m.t.report(Show)
	case 3:
		go m.t.report(Quit)
	}
	return nil
}

func (m menu) EventGroup(events []menuEvent) ([]int32, *dbus.Error) {
	for _, e := range events {
		_ = m.Event(e.ID, e.EventID, e.Data, e.Timestamp)
	}
	return []int32{}, nil
}

func (m menu) AboutToShow(id int32) (bool, *dbus.Error) { return false, nil }

func (m menu) AboutToShowGroup(ids []int32) ([]int32, []int32, *dbus.Error) {
	return []int32{}, []int32{}, nil
}

const itemXML = `<node>
 <interface name="org.kde.StatusNotifierItem">
  <property name="Category" type="s" access="read"/>
  <property name="Id" type="s" access="read"/>
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="WindowId" type="i" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="IconThemePath" type="s" access="read"/>
  <property name="IconPixmap" type="a(iiay)" access="read"/>
  <property name="OverlayIconName" type="s" access="read"/>
  <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
  <property name="AttentionIconName" type="s" access="read"/>
  <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
  <property name="AttentionMovieName" type="s" access="read"/>
  <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
  <property name="ItemIsMenu" type="b" access="read"/>
  <property name="Menu" type="o" access="read"/>
  <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
  <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
  <signal name="NewTitle"/>
  <signal name="NewIcon"/>
  <signal name="NewAttentionIcon"/>
  <signal name="NewOverlayIcon"/>
  <signal name="NewToolTip"/>
  <signal name="NewStatus"><arg name="status" type="s"/></signal>
 </interface>` + introspect.IntrospectDataString + prop.IntrospectDataString + `</node>`

const menuXML = `<node>
 <interface name="com.canonical.dbusmenu">
  <property name="Version" type="u" access="read"/>
  <property name="TextDirection" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconThemePath" type="as" access="read"/>
  <method name="GetLayout"><arg type="i" name="parentId" direction="in"/><arg type="i" name="recursionDepth" direction="in"/><arg type="as" name="propertyNames" direction="in"/><arg type="u" name="revision" direction="out"/><arg type="(ia{sv}av)" name="layout" direction="out"/></method>
  <method name="GetGroupProperties"><arg type="ai" name="ids" direction="in"/><arg type="as" name="propertyNames" direction="in"/><arg type="a(ia{sv})" name="properties" direction="out"/></method>
  <method name="GetProperty"><arg type="i" name="id" direction="in"/><arg type="s" name="name" direction="in"/><arg type="v" name="value" direction="out"/></method>
  <method name="Event"><arg type="i" name="id" direction="in"/><arg type="s" name="eventId" direction="in"/><arg type="v" name="data" direction="in"/><arg type="u" name="timestamp" direction="in"/></method>
  <method name="EventGroup"><arg type="a(isvu)" name="events" direction="in"/><arg type="ai" name="idErrors" direction="out"/></method>
  <method name="AboutToShow"><arg type="i" name="id" direction="in"/><arg type="b" name="needUpdate" direction="out"/></method>
  <method name="AboutToShowGroup"><arg type="ai" name="ids" direction="in"/><arg type="ai" name="updatesNeeded" direction="out"/><arg type="ai" name="idErrors" direction="out"/></method>
  <signal name="ItemsPropertiesUpdated"><arg type="a(ia{sv})" name="updatedProps"/><arg type="a(ias)" name="removedProps"/></signal>
  <signal name="LayoutUpdated"><arg type="u" name="revision"/><arg type="i" name="parent"/></signal>
  <signal name="ItemActivationRequested"><arg type="i" name="id"/><arg type="u" name="timestamp"/></signal>
 </interface>` + introspect.IntrospectDataString + prop.IntrospectDataString + `</node>`
