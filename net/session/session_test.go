package session

import (
	"bytes"
	"compress/gzip"
	"testing"
	"time"
)

func TestNextMsgID(t *testing.T) {
	now := time.Unix(1_800_000_000, 123)
	var last int64
	for i := 0; i < 10000; i++ {
		id := nextMsgID(now, last)
		if id%4 != 0 {
			t.Fatalf("msg_id %d не кратен 4", id)
		}
		if id <= last {
			t.Fatalf("msg_id не возрастает: %d после %d", id, last)
		}
		if id>>32 != now.Unix() && id>>32 != now.Unix()+1 {
			t.Fatalf("старшие 32 бита msg_id — не время: %d", id>>32)
		}
		last = id
	}
}

func TestAcceptID(t *testing.T) {
	s := &Session{seen: map[int64]struct{}{}}
	now := time.Now()
	base := now.Unix() << 32
	if s.acceptIDLocked(base | 4) {
		t.Fatal("принят чётный msg_id (от клиента)")
	}
	if !s.acceptIDLocked(base | 1) {
		t.Fatal("не принят нормальный msg_id сервера")
	}
	if s.acceptIDLocked(base | 1) {
		t.Fatal("принят повтор")
	}
	old := now.Add(-400*time.Second).Unix()<<32 | 1
	if s.acceptIDLocked(old) {
		t.Fatal("принят msg_id старше 300 с")
	}
	future := now.Add(60*time.Second).Unix()<<32 | 1
	if s.acceptIDLocked(future) {
		t.Fatal("принят msg_id из будущего (> 30 с)")
	}
	// Поправка на время сервера сдвигает окно.
	s.offset = 60 * time.Second
	if !s.acceptIDLocked(future) {
		t.Fatal("с поправкой времени msg_id не принят")
	}
	// Память повторов ограничена.
	for i := 0; i < seenLimit*2; i++ {
		s.acceptIDLocked(base | int64(i*4+3))
	}
	if len(s.seen) > seenLimit || len(s.seenOrder) > seenLimit {
		t.Fatalf("память повторов не ограничена: %d", len(s.seen))
	}
}

func TestGunzipLimit(t *testing.T) {
	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	_, _ = zw.Write(make([]byte, maxGunzip+1024)) // сжимается в килобайты
	_ = zw.Close()
	if buf.Len() > 1<<20 {
		t.Fatalf("бомба не сжалась: %d", buf.Len())
	}
	if _, err := gunzip(buf.Bytes()); err == nil {
		t.Fatal("распаковка сверх предела не отвергнута")
	}
	buf.Reset()
	zw = gzip.NewWriter(&buf)
	_, _ = zw.Write([]byte("привет"))
	_ = zw.Close()
	if out, err := gunzip(buf.Bytes()); err != nil || string(out) != "привет" {
		t.Fatalf("обычная распаковка: %v", err)
	}
	if _, err := gunzip([]byte{1, 2, 3}); err == nil {
		t.Fatal("мусор распакован")
	}
}
