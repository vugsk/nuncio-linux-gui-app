package api

import (
	"testing"
	"time"

	"nuncio/net/dc"

	pb "nuncio/net/gen"
	"nuncio/net/internal/fakeserver"
)

func chatAccount() *fakeserver.Account {
	return &fakeserver.Account{Phone: "1", Code: "1", Registered: true, UserID: 5, FirstName: "Я",
		Contacts: []fakeserver.Contact{
			{ID: 100, Name: "Анна", AccessHash: 11, Messages: []string{"привет", "как дела?"}},
			{ID: 200, Name: "Борис", AccessHash: 22, Messages: []string{"встреча в 10"}},
		}}
}

// login — вход по QR и ожидание первой пачки обновлений (состояние).
func login(t *testing.T, h *harness) {
	h.step(pb.AuthState_LOGGED_OUT)
	h.send(&pb.Command{Body: &pb.Command_RequestQr{RequestQr: &pb.RequestQr{}}})
	h.step(pb.AuthState_QR)
	h.srv.ConfirmQR()
	h.step(pb.AuthState_READY)
	h.next(func(e *pb.Event) bool { return e.GetUpdates().GetState().GetPts() != 0 })
}

// newMessages — собрать n новых сообщений из пачек обновлений.
func newMessages(t *testing.T, h *harness, n int) ([]*pb.Message, *pb.UpdateState) {
	t.Helper()
	var got []*pb.Message
	var st *pb.UpdateState
	for len(got) < n {
		b := h.next(func(e *pb.Event) bool { return e.GetUpdates() != nil }).GetUpdates()
		for _, u := range b.GetUpdates() {
			if m := u.GetNewMessage(); m != nil {
				got = append(got, m)
			}
		}
		st = b.GetState()
	}
	return got, st
}

func TestVerticalOverProto(t *testing.T) {
	acc := chatAccount()
	h := newHarness(t, acc)
	login(t, h)

	// Диалоги с последними сообщениями и собеседниками.
	r := h.result(h.send(&pb.Command{Body: &pb.Command_LoadDialogs{LoadDialogs: &pb.LoadDialogs{Limit: 100}}}))
	d := r.GetDialogs()
	if r.GetError() != nil || len(d.GetDialogs()) != 2 || !d.GetComplete() || len(d.GetMessages()) != 2 {
		t.Fatalf("диалоги: %v", r)
	}
	var anna *pb.Peer
	for _, p := range d.GetPeers() {
		if p.GetId() == 100 {
			anna = p
		}
	}
	if anna == nil || anna.GetName() != "Анна" || anna.GetAccessHash() != 11 {
		t.Fatalf("собеседник: %v", anna)
	}
	ref := &pb.PeerRef{Id: 100, AccessHash: 11}

	// История: от новых к старым.
	r = h.result(h.send(&pb.Command{Body: &pb.Command_LoadHistory{LoadHistory: &pb.LoadHistory{Peer: ref, Limit: 50}}}))
	msgs := r.GetHistory().GetMessages()
	if len(msgs) != 2 || msgs[0].GetText() != "как дела?" || msgs[1].GetText() != "привет" || msgs[0].GetFromId() != 100 {
		t.Fatalf("история: %v", msgs)
	}

	// Отправка: результат с id, и то же сообщение в пачке обновлений с новым pts.
	r = h.result(h.send(&pb.Command{Body: &pb.Command_SendMessage{SendMessage: &pb.SendMessage{Peer: ref,
		Text: "ответ", RandomId: 42}}}))
	sent := r.GetSent().GetMessage()
	if r.GetError() != nil || sent.GetId() == 0 || !sent.GetOut() || sent.GetRandomId() != 42 {
		t.Fatalf("отправка: %v", r)
	}
	got, st := newMessages(t, h, 1)
	if got[0].GetId() != sent.GetId() || got[0].GetText() != "ответ" || st.GetPts() != int32(sent.GetId()) {
		t.Fatalf("отправленное в обновлениях: %v, pts %d", got[0], st.GetPts())
	}

	// 25 входящих подряд — без потерь и дублей, по порядку.
	var want []int32
	for i := 0; i < 25; i++ {
		want = append(want, h.srv.PushMessage(200, "серия", false))
	}
	got, _ = newMessages(t, h, 25)
	for i, m := range got {
		if m.GetId() != want[i] || m.GetPeerId() != 200 {
			t.Fatalf("серия: %d-е сообщение id %d, ожидался %d", i, m.GetId(), want[i])
		}
	}

	// Разрыв: одно сообщение «потерялось», следующее вызывает getDifference.
	lost := h.srv.PushMessage(100, "потерянное", true)
	after := h.srv.PushMessage(100, "после разрыва", false)
	got, st = newMessages(t, h, 2)
	ids := map[int32]bool{}
	for _, m := range got {
		ids[m.GetId()] = true
	}
	if !ids[lost] || !ids[after] || st.GetPts() != after {
		t.Fatalf("после разрыва: %v, pts %d", got, st.GetPts())
	}
}

func TestResumeWithStoredState(t *testing.T) {
	// Состояние из базы Rust передаётся в StartRequest: Go догоняет getDifference.
	acc := chatAccount()
	h := newHarness(t, acc)
	login(t, h)
	if h.s.upd.State().GetPts() != 3 {
		t.Fatalf("pts после входа %d", h.s.upd.State().GetPts())
	}
}

// «Выключили сеть на N секунд»: соединение восстанавливается само, пропущенные
// сообщения приходят через getDifference, без потерь и дублей.
func TestReconnectAfterOutage(t *testing.T) {
	dc.KeepMinDelay, dc.KeepMaxDelay = 50*time.Millisecond, 400*time.Millisecond
	acc := chatAccount()
	h := newHarness(t, acc)
	login(t, h)
	h.srv.Stop()
	h.next(func(e *pb.Event) bool {
		return e.GetConnection().GetState() == pb.ConnectionState_WAITING_NETWORK
	})
	var want []int32
	for i := 0; i < 3; i++ { // пока сети нет, сервер «копит» сообщения
		want = append(want, h.srv.PushMessage(100, "без сети", true))
	}
	time.Sleep(time.Second)
	if err := h.srv.Restart(); err != nil {
		t.Fatal(err)
	}
	h.next(func(e *pb.Event) bool { return e.GetConnection().GetState() == pb.ConnectionState_CONNECTED })
	got, st := newMessages(t, h, 3)
	for i, m := range got {
		if m.GetId() != want[i] {
			t.Fatalf("после восстановления: %v, ожидались %v", got, want)
		}
	}
	if st.GetPts() != want[2] {
		t.Fatalf("pts %d", st.GetPts())
	}
}

func TestMessageActionsOverProto(t *testing.T) {
	acc := chatAccount()
	h := newHarness(t, acc)
	login(t, h)
	anna := &pb.PeerRef{Id: 100, AccessHash: 11}
	boris := &pb.PeerRef{Id: 200, AccessHash: 22}
	res := func(c *pb.Command) *pb.CommandResult {
		t.Helper()
		r := h.result(h.send(c))
		if r.GetError() != nil {
			t.Fatalf("%T: %v", c.GetBody(), r.GetError())
		}
		return r
	}
	// Ответ: reply_to уходит на сервер и возвращается в сообщении.
	sent := res(&pb.Command{Body: &pb.Command_SendMessage{SendMessage: &pb.SendMessage{Peer: anna, Text: "ответ",
		RandomId: 1, ReplyTo: 1}}}).GetSent().GetMessage()
	if sent.GetReplyTo() != 1 {
		t.Fatalf("ответ: %v", sent)
	}
	newMessages(t, h, 1)
	// Правка — обновлением с новым pts.
	res(&pb.Command{Body: &pb.Command_EditMessage{EditMessage: &pb.EditMessage{Peer: anna, Id: sent.GetId(), Text: "исправлено"}}})
	b := h.next(func(e *pb.Event) bool { return len(e.GetUpdates().GetUpdates()) > 0 }).GetUpdates()
	if em := b.GetUpdates()[0].GetEditMessage(); em.GetText() != "исправлено" || em.GetEditDate() == 0 {
		t.Fatalf("правка: %v", b)
	}
	// Пересылка двум адресатам.
	res(&pb.Command{Body: &pb.Command_Forward{Forward: &pb.ForwardMessages{FromPeer: anna, Ids: []int32{1},
		Targets: []*pb.ForwardTarget{{Peer: boris, RandomIds: []int64{7}}, {Peer: &pb.PeerRef{Id: 5}, RandomIds: []int64{8}}}}}})
	got, _ := newMessages(t, h, 2)
	if got[0].GetFwdFromId() != 100 || got[0].GetPeerId() != 200 || got[1].GetPeerId() != 5 {
		t.Fatalf("пересылка: %v", got)
	}
	// Повтор пересылки с теми же random_id — дубль отвергнут.
	r := h.result(h.send(&pb.Command{Body: &pb.Command_Forward{Forward: &pb.ForwardMessages{FromPeer: anna, Ids: []int32{1},
		Targets: []*pb.ForwardTarget{{Peer: boris, RandomIds: []int64{7}}}}}}))
	if r.GetError().GetText() != "RANDOM_ID_DUPLICATE" {
		t.Fatalf("повтор пересылки: %v", r)
	}
	// Закрепление и список закреплённых.
	res(&pb.Command{Body: &pb.Command_PinMessage{PinMessage: &pb.PinMessage{Peer: anna, Id: 2}}})
	b = h.next(func(e *pb.Event) bool { return e.GetUpdates().GetUpdates() != nil && len(e.GetUpdates().GetUpdates()) > 0 }).GetUpdates()
	if p := b.GetUpdates()[0].GetPinned(); p.GetPeerId() != 100 || !p.GetPinned() || p.GetIds()[0] != 2 {
		t.Fatalf("закрепление: %v", b)
	}
	pins := res(&pb.Command{Body: &pb.Command_LoadPinned{LoadPinned: &pb.LoadPinned{Peer: anna}}}).GetHistory().GetMessages()
	if len(pins) != 1 || pins[0].GetId() != 2 || !pins[0].GetPinned() {
		t.Fatalf("закреплённые: %v", pins)
	}
	// Отдельные сообщения по id (цитата ответа).
	one := res(&pb.Command{Body: &pb.Command_LoadMessages{LoadMessages: &pb.LoadMessages{Peer: anna, Ids: []int32{1}}}}).GetHistory()
	if len(one.GetMessages()) != 1 || one.GetMessages()[0].GetText() != "привет" {
		t.Fatalf("по id: %v", one)
	}
	// Удаление — обновлением с pts.
	res(&pb.Command{Body: &pb.Command_DeleteMessages{DeleteMessages: &pb.DeleteMessagesCmd{Peer: anna, Ids: []int32{sent.GetId()}, Revoke: true}}})
	b = h.next(func(e *pb.Event) bool { return len(e.GetUpdates().GetUpdates()) > 0 }).GetUpdates()
	if d := b.GetUpdates()[0].GetDeleteMessages(); len(d.GetIds()) != 1 || d.GetIds()[0] != sent.GetId() {
		t.Fatalf("удаление: %v", b)
	}
	// Контакты и поиск людей.
	if n := len(res(&pb.Command{Body: &pb.Command_GetContacts{GetContacts: &pb.GetContacts{}}}).GetPeers().GetPeers()); n != 2 {
		t.Fatalf("контактов %d", n)
	}
	found := res(&pb.Command{Body: &pb.Command_SearchPeers{SearchPeers: &pb.SearchPeers{Query: "бор"}}}).GetPeers().GetPeers()
	if len(found) != 1 || found[0].GetName() != "Борис" {
		t.Fatalf("поиск: %v", found)
	}
}
