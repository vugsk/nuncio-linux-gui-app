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
