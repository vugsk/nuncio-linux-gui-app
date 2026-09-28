package updates

import (
	"context"
	"sync"
	"testing"
	"time"

	pb "nuncio/net/gen"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// fakeInv отвечает на getState/getDifference по сценарию.
type fakeInv struct {
	mu    sync.Mutex
	diffs []any
	calls []tlbin.Request
	state *tl.UpdatesState
}

func (f *fakeInv) Invoke(_ context.Context, req tlbin.Request) (any, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, req)
	switch req.(type) {
	case *tl.UpdatesGetStateRequest:
		return f.state, nil
	case *tl.UpdatesGetDifferenceRequest:
		if len(f.diffs) == 0 {
			return &tl.UpdatesDifferenceEmpty{Date: 1, Seq: 0}, nil
		}
		d := f.diffs[0]
		f.diffs = f.diffs[1:]
		return d, nil
	}
	return nil, nil
}

func (f *fakeInv) count(name string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	n := 0
	for _, c := range f.calls {
		if c.TLName() == name {
			n++
		}
	}
	return n
}

type sink struct {
	mu      sync.Mutex
	batches []*pb.UpdatesBatch
}

func (s *sink) emit(b *pb.UpdatesBatch) {
	s.mu.Lock()
	s.batches = append(s.batches, b)
	s.mu.Unlock()
}

// messages — id всех новых сообщений по порядку.
func (s *sink) messages() []int32 {
	s.mu.Lock()
	defer s.mu.Unlock()
	var ids []int32
	for _, b := range s.batches {
		for _, u := range b.Updates {
			if m := u.GetNewMessage(); m != nil {
				ids = append(ids, m.Id)
			}
		}
	}
	return ids
}

func (s *sink) last() *pb.UpdatesBatch {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.batches) == 0 {
		return nil
	}
	return s.batches[len(s.batches)-1]
}

func start(t *testing.T, inv *fakeInv, st *pb.UpdateState) (*Manager, *sink) {
	s := &sink{}
	m := New(Config{Inv: inv, Emit: s.emit, SelfID: func() int64 { return 1 }})
	m.Start(st)
	t.Cleanup(m.Stop)
	return m, s
}

// sync — дождаться обработки очереди.
func wait(m *Manager) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	m.Process(ctx, &tl.UpdateShort{Update: &tl.UpdateLoginToken{}}, nil)
}

func short(id, pts int32) *tl.UpdateShortMessage {
	return &tl.UpdateShortMessage{Id: id, UserId: 42, Message: "текст", Pts: pts, PtsCount: 1, Date: 100}
}

func TestInOrderAndDuplicates(t *testing.T) {
	m, s := start(t, &fakeInv{}, &pb.UpdateState{Pts: 10, Date: 1})
	for i := int32(1); i <= 25; i++ {
		m.Push(short(i, 10+i))
	}
	m.Push(short(5, 15)) // дубль
	m.Push(short(7, 17)) // дубль
	wait(m)
	ids := s.messages()
	if len(ids) != 25 {
		t.Fatalf("сообщений %d, ожидалось 25 (без дублей)", len(ids))
	}
	for i, id := range ids {
		if id != int32(i+1) {
			t.Fatalf("порядок нарушен: %v", ids)
		}
	}
	if st := m.State(); st.Pts != 35 || st.Date != 100 {
		t.Fatalf("состояние %v", st)
	}
	if s.last().State.GetPts() != 35 {
		t.Fatal("в пачке нет нового состояния")
	}
}

func TestGapTriggersDifference(t *testing.T) {
	inv := &fakeInv{diffs: []any{
		&tl.UpdatesDifferenceEmpty{Date: 1}, // стартовая разница
		&tl.UpdatesDifference{
			NewMessages: []tl.MessageClass{
				&tl.Message{Id: 2, PeerId: &tl.PeerUser{UserId: 42}, Date: 5, Message: "пропущенное"},
				&tl.Message{Id: 3, PeerId: &tl.PeerUser{UserId: 42}, Date: 6, Message: "пропущенное"},
			},
			OtherUpdates: []tl.UpdateClass{&tl.UpdateReadHistoryOutbox{Peer: &tl.PeerUser{UserId: 42}, MaxId: 3,
				Pts: 999, PtsCount: 1}},
			Users: []tl.UserClass{&tl.User{Id: 42}},
			State: &tl.UpdatesState{Pts: 14, Qts: 0, Date: 7, Seq: 0},
		},
	}}
	m, s := start(t, inv, &pb.UpdateState{Pts: 10, Date: 1})
	m.Push(short(1, 11))
	m.Push(short(4, 14)) // пропущены 12, 13
	wait(m)
	if inv.count("updates.getDifference") != 2 {
		t.Fatalf("getDifference вызван %d раз", inv.count("updates.getDifference"))
	}
	ids := s.messages()
	if len(ids) != 3 || ids[1] != 2 || ids[2] != 3 {
		t.Fatalf("после разрыва: %v", ids)
	}
	if m.State().Pts != 14 {
		t.Fatalf("pts после разницы %d", m.State().Pts)
	}
	// Сообщение 4 уже учтено разницей (pts 14): повтор — дубль.
	m.Push(short(4, 14))
	wait(m)
	if len(s.messages()) != 3 {
		t.Fatal("после разницы прошёл дубль")
	}
}

func TestDifferenceSliceAndTooLong(t *testing.T) {
	msg := func(id int32) tl.MessageClass {
		return &tl.Message{Id: id, PeerId: &tl.PeerUser{UserId: 42}, Date: 5}
	}
	inv := &fakeInv{diffs: []any{
		&tl.UpdatesDifferenceSlice{NewMessages: []tl.MessageClass{msg(1), msg(2)},
			IntermediateState: &tl.UpdatesState{Pts: 12, Date: 5}},
		&tl.UpdatesDifference{NewMessages: []tl.MessageClass{msg(3)}, State: &tl.UpdatesState{Pts: 13, Date: 6}},
	}}
	m, s := start(t, inv, &pb.UpdateState{Pts: 10, Date: 1})
	wait(m)
	if ids := s.messages(); len(ids) != 3 || m.State().Pts != 13 {
		t.Fatalf("срез разницы: %v, pts %d", ids, m.State().Pts)
	}

	inv2 := &fakeInv{diffs: []any{&tl.UpdatesDifferenceTooLong{Pts: 500}}}
	m2, s2 := start(t, inv2, &pb.UpdateState{Pts: 10, Date: 1})
	wait(m2)
	if s2.last() == nil || s2.last().Updates[0].GetReset_() == nil || m2.State().Pts != 500 {
		t.Fatal("differenceTooLong не сбросил диалоги")
	}
}

func TestEmptyStateUsesGetState(t *testing.T) {
	inv := &fakeInv{state: &tl.UpdatesState{Pts: 77, Qts: 1, Seq: 3, Date: 9}}
	m, s := start(t, inv, nil)
	wait(m)
	if st := m.State(); st.Pts != 77 || st.Seq != 3 || inv.count("updates.getState") != 1 {
		t.Fatalf("getState: %v", st)
	}
	if s.last().GetState().GetPts() != 77 {
		t.Fatal("начальное состояние не отдано Rust для записи")
	}
}

func TestSeqGapAndCombined(t *testing.T) {
	inv := &fakeInv{}
	m, s := start(t, inv, &pb.UpdateState{Pts: 10, Seq: 5, Date: 1})
	wait(m)
	newMsg := func(id, pts int32) tl.UpdateClass {
		return &tl.UpdateNewMessage{Message: &tl.Message{Id: id, PeerId: &tl.PeerChat{ChatId: 9}, Date: 3},
			Pts: pts, PtsCount: 1}
	}
	m.Push(&tl.Updates{Updates: []tl.UpdateClass{newMsg(1, 11)}, Users: []tl.UserClass{}, Chats: []tl.ChatClass{
		&tl.Chat{Id: 9, Title: "группа", Photo: &tl.ChatPhotoEmpty{}}}, Date: 3, Seq: 6})
	wait(m)
	if ids := s.messages(); len(ids) != 1 || m.State().Seq != 6 {
		t.Fatalf("updates: %v seq %d", ids, m.State().Seq)
	}
	if s.last().Peers[0].GetId() != -9 {
		t.Fatal("чаты из updates не переданы")
	}
	before := inv.count("updates.getDifference")
	m.Push(&tl.Updates{Updates: []tl.UpdateClass{newMsg(3, 13)}, Date: 4, Seq: 9}) // seq пропущен
	wait(m)
	if inv.count("updates.getDifference") != before+1 {
		t.Fatal("разрыв seq не вызвал getDifference")
	}
}

func TestSentMessageInSameBatch(t *testing.T) {
	m, s := start(t, &fakeInv{}, &pb.UpdateState{Pts: 10, Date: 1})
	wait(m)
	sent := &pb.Message{PeerId: 1, Text: "в Избранное", RandomId: 555, FromId: 1}
	m.Process(context.Background(), &tl.UpdateShortSentMessage{Id: 99, Pts: 11, PtsCount: 1, Date: 50}, sent)
	b := s.last()
	if len(b.Updates) != 1 || b.Updates[0].GetNewMessage().GetId() != 99 || b.State.GetPts() != 11 {
		t.Fatalf("отправленное: %v", b)
	}
	// Ответ в виде updates с updateMessageID.
	m.Process(context.Background(), &tl.Updates{Updates: []tl.UpdateClass{
		&tl.UpdateMessageID{Id: 100, RandomId: 777},
		&tl.UpdateNewMessage{Message: &tl.Message{Id: 100, Out: true, PeerId: &tl.PeerUser{UserId: 1}, Date: 51},
			Pts: 12, PtsCount: 1},
	}, Date: 51}, nil)
	if m.SentID(777) != 100 || m.SentID(777) != 0 {
		t.Fatal("updateMessageID не сопоставлен")
	}
}
