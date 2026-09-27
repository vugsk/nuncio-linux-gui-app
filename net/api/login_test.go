package api

import (
	"context"
	"sync"
	"testing"
	"time"

	"google.golang.org/protobuf/proto"

	"nuncio/net/core"
	pb "nuncio/net/gen"
	"nuncio/net/internal/fakeserver"
)

type memKeyring struct {
	mu  sync.Mutex
	key []byte
}

func (k *memKeyring) WrapKey(_ context.Context, create bool) ([]byte, bool, error) {
	k.mu.Lock()
	defer k.mu.Unlock()
	if k.key == nil && create {
		k.key = make([]byte, 32)
		_ = core.Random(k.key)
	}
	return k.key, k.key != nil, nil
}

func (k *memKeyring) Delete(context.Context) error {
	k.mu.Lock()
	k.key = nil
	k.mu.Unlock()
	return nil
}

type harness struct {
	t       *testing.T
	s       *Service
	srv     *fakeserver.Server
	id      uint64
	pending []*pb.Event // пришли раньше, чем их ждали
}

func newHarness(t *testing.T, acc *fakeserver.Account) *harness {
	srv, err := fakeserver.Start(fakeserver.Options{DC: 2, Account: acc})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { srv.Close() })
	n, e := srv.PublicKey()
	if _, err := core.AddRSAKey(n, e); err != nil {
		t.Fatal(err)
	}
	s := New()
	s.Keyring = &memKeyring{}
	s.Addrs = map[int]string{2: srv.Addr()}
	req := &pb.StartRequest{ProtoVersion: ProtoVersion, ApiId: 1, ApiHash: "h", DataDir: t.TempDir(),
		LogDir: t.TempDir(), DeviceModel: "test", LangCode: "ru"}
	if err := s.Start(mustMarshal(t, req)); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Shutdown(2 * time.Second) })
	return &harness{t: t, s: s, srv: srv}
}

func (h *harness) send(c *pb.Command) uint64 {
	h.id++
	c.Id = h.id
	if err := h.s.Submit(h.id, mustMarshal(h.t, c)); err != nil {
		h.t.Fatal(err)
	}
	return h.id
}

// next ждёт событие, для которого match вернёт true (остальные пропускает).
func (h *harness) next(match func(*pb.Event) bool) *pb.Event {
	h.t.Helper()
	for i, ev := range h.pending {
		if match(ev) {
			h.pending = append(h.pending[:i], h.pending[i+1:]...)
			return ev
		}
	}
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		data, res := h.s.NextEvent(time.Second)
		if res != GotEvent {
			continue
		}
		var ev pb.Event
		if err := proto.Unmarshal(data, &ev); err != nil {
			h.t.Fatal(err)
		}
		if match(&ev) {
			return &ev
		}
		h.pending = append(h.pending, &ev)
	}
	h.t.Fatal("событие не пришло")
	return nil
}

func (h *harness) step(want pb.AuthState_Step) *pb.AuthState {
	h.t.Helper()
	return h.next(func(e *pb.Event) bool { return e.GetAuth() != nil && e.GetAuth().GetStep() == want }).GetAuth()
}

func (h *harness) result(id uint64) *pb.CommandResult {
	h.t.Helper()
	return h.next(func(e *pb.Event) bool { return e.GetResult() != nil && e.GetResult().GetCmdId() == id }).GetResult()
}

func TestLoginOverProto(t *testing.T) {
	acc := &fakeserver.Account{Phone: "79990001122", Code: "12345", Password: "pw", Registered: true, UserID: 5,
		FirstName: "Имя"}
	h := newHarness(t, acc)
	h.step(pb.AuthState_LOGGED_OUT)

	// Список стран на языке интерфейса.
	r := h.result(h.send(&pb.Command{Body: &pb.Command_GetCountries{GetCountries: &pb.GetCountries{LangCode: "ru"}}}))
	if r.GetError() != nil || len(r.GetCountries().GetCountries()) != 1 ||
		r.GetCountries().GetCountries()[0].GetName() != "Россия" {
		t.Fatalf("страны: %v", r)
	}

	// QR первым (Вх-1), затем переход к номеру.
	h.send(&pb.Command{Body: &pb.Command_RequestQr{RequestQr: &pb.RequestQr{}}})
	if st := h.step(pb.AuthState_QR); st.GetQrUrl() == "" || st.GetQrExpiresUnix() == 0 {
		t.Fatalf("QR: %v", st)
	}
	r = h.result(h.send(&pb.Command{Body: &pb.Command_SendCode{SendCode: &pb.SendCode{Phone: "+79990001122"}}}))
	if r.GetError() != nil {
		t.Fatal(r.GetError())
	}
	if st := h.step(pb.AuthState_CODE); st.GetCode().GetLength() != 5 {
		t.Fatalf("код: %v", st)
	}
	r = h.result(h.send(&pb.Command{Body: &pb.Command_SignIn{SignIn: &pb.SignIn{Code: "99999"}}}))
	if r.GetError().GetKind() != pb.Error_RPC || r.GetError().GetText() != "PHONE_CODE_INVALID" {
		t.Fatalf("неверный код: %v", r.GetError())
	}
	h.send(&pb.Command{Body: &pb.Command_SignIn{SignIn: &pb.SignIn{Code: "12345"}}})
	h.step(pb.AuthState_PASSWORD)
	h.send(&pb.Command{Body: &pb.Command_CheckPassword{CheckPassword: &pb.CheckPassword{Password: "pw"}}})
	if st := h.step(pb.AuthState_READY); st.GetUser().GetFirstName() != "Имя" {
		t.Fatalf("готово: %v", st)
	}

	// Выход.
	r = h.result(h.send(&pb.Command{Body: &pb.Command_LogOut{LogOut: &pb.LogOut{}}}))
	if r.GetError() != nil {
		t.Fatal(r.GetError())
	}
	if st := h.step(pb.AuthState_LOGGED_OUT); st.GetReason() != "logged_out" {
		t.Fatalf("выход: %v", st)
	}
	if acc.LoggedOut() != 1 {
		t.Fatal("сервер не получил auth.logOut")
	}
}

func TestQROverProto(t *testing.T) {
	acc := &fakeserver.Account{Phone: "1", Code: "1", Registered: true, UserID: 5, FirstName: "Имя"}
	h := newHarness(t, acc)
	h.step(pb.AuthState_LOGGED_OUT)
	h.send(&pb.Command{Body: &pb.Command_RequestQr{RequestQr: &pb.RequestQr{}}})
	h.step(pb.AuthState_QR)
	h.srv.ConfirmQR()
	h.step(pb.AuthState_READY)
}

func TestNoAPIIDStillAnswersPing(t *testing.T) {
	s := New()
	if err := s.Start(mustMarshal(t, &pb.StartRequest{ProtoVersion: ProtoVersion})); err != nil {
		t.Fatal(err)
	}
	defer func() { _ = s.Shutdown(time.Second) }()
	h := &harness{t: t, s: s}
	r := h.result(h.send(&pb.Command{Body: &pb.Command_SendCode{SendCode: &pb.SendCode{Phone: "1"}}}))
	if r.GetError().GetKind() != pb.Error_INTERNAL {
		t.Fatalf("без api_id: %v", r)
	}
}
