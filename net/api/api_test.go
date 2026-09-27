package api

import (
	"errors"
	"strings"
	"testing"
	"time"

	"google.golang.org/protobuf/proto"

	pb "nuncio/net/gen"
)

func mustMarshal(t *testing.T, m proto.Message) []byte {
	t.Helper()
	b, err := proto.Marshal(m)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func startService(t *testing.T) *Service {
	t.Helper()
	s := New()
	if err := s.Start(mustMarshal(t, &pb.StartRequest{ProtoVersion: ProtoVersion})); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Shutdown(time.Second) })
	return s
}

func TestPingReturnsCoreVersion(t *testing.T) {
	s := startService(t)
	cmd := &pb.Command{Id: 7, AccountId: 1, Body: &pb.Command_Ping{Ping: &pb.Ping{Message: "привет"}}}
	if err := s.Submit(7, mustMarshal(t, cmd)); err != nil {
		t.Fatal(err)
	}
	data, res := s.NextEvent(2 * time.Second)
	if res != GotEvent {
		t.Fatalf("ожидалось событие, получено %v", res)
	}
	var ev pb.Event
	if err := proto.Unmarshal(data, &ev); err != nil {
		t.Fatal(err)
	}
	r := ev.GetResult()
	if r.GetCmdId() != 7 || r.GetError() != nil {
		t.Fatalf("неожиданный результат: %v", r)
	}
	if r.GetPong().GetMessage() != "привет" || !strings.HasPrefix(r.GetPong().GetCoreVersion(), "tgcrypto ") {
		t.Fatalf("неожиданный Pong: %v", r.GetPong())
	}
}

func TestStartRejectsWrongVersion(t *testing.T) {
	s := New()
	err := s.Start(mustMarshal(t, &pb.StartRequest{ProtoVersion: ProtoVersion + 1}))
	if !errors.Is(err, ErrBadVersion) {
		t.Fatalf("ожидалась ErrBadVersion, получено %v", err)
	}
}

func TestStartTwiceFails(t *testing.T) {
	s := startService(t)
	if err := s.Start(mustMarshal(t, &pb.StartRequest{ProtoVersion: ProtoVersion})); !errors.Is(err, ErrAlreadyStarted) {
		t.Fatalf("ожидалась ErrAlreadyStarted, получено %v", err)
	}
}

func TestNextEventTimeoutAndStop(t *testing.T) {
	s := startService(t)
	if _, res := s.NextEvent(10 * time.Millisecond); res != Timeout {
		t.Fatalf("ожидался таймаут, получено %v", res)
	}
	if err := s.Shutdown(time.Second); err != nil {
		t.Fatal(err)
	}
	if _, res := s.NextEvent(time.Second); res != Stopped {
		t.Fatalf("ожидалась остановка, получено %v", res)
	}
	if err := s.Submit(1, nil); !errors.Is(err, ErrStopped) {
		t.Fatalf("ожидалась ErrStopped, получено %v", err)
	}
}

func TestUnknownCommandIsError(t *testing.T) {
	s := startService(t)
	if err := s.Submit(3, mustMarshal(t, &pb.Command{Id: 3})); err != nil {
		t.Fatal(err)
	}
	data, res := s.NextEvent(2 * time.Second)
	if res != GotEvent {
		t.Fatalf("ожидалось событие, получено %v", res)
	}
	var ev pb.Event
	if err := proto.Unmarshal(data, &ev); err != nil {
		t.Fatal(err)
	}
	if ev.GetResult().GetError().GetKind() != pb.Error_INTERNAL {
		t.Fatalf("ожидалась ошибка INTERNAL, получено %v", ev.GetResult())
	}
}

func TestSubmitRejectsGarbage(t *testing.T) {
	s := startService(t)
	if err := s.Submit(1, []byte{0xff, 0xff, 0xff}); err == nil {
		t.Fatal("ожидалась ошибка разбора")
	}
}
