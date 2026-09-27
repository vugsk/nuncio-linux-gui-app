package tlbin

import (
	"bytes"
	"errors"
	"testing"
)

func TestBytesPaddingAndLongForm(t *testing.T) {
	for n := 0; n < 700; n++ {
		src := bytes.Repeat([]byte{byte(n)}, n)
		var e Encoder
		e.PutBytes(src)
		if e.Len()%4 != 0 {
			t.Fatalf("n=%d: длина %d не кратна 4", n, e.Len())
		}
		wantHead := 1
		if n >= 254 {
			wantHead = 4
		}
		if e.Len() < wantHead+n || e.Len() > wantHead+n+3 {
			t.Fatalf("n=%d: неверная длина %d", n, e.Len())
		}
		d := NewDecoder(e.Bytes())
		got, err := d.Bytes()
		if err != nil || !bytes.Equal(got, src) || d.Remaining() != 0 {
			t.Fatalf("n=%d: %v", n, err)
		}
	}
}

func TestPrimitives(t *testing.T) {
	var e Encoder
	e.PutInt32(-5)
	e.PutInt64(-1 << 40)
	e.PutDouble(3.5)
	e.PutBool(true)
	e.PutBool(false)
	e.PutString("привет")
	e.PutInt128([16]byte{1, 2})
	e.PutInt256([32]byte{3})
	d := NewDecoder(e.Bytes())
	i32, _ := d.Int32()
	i64, _ := d.Int64()
	f, _ := d.Double()
	b1, _ := d.Bool()
	b2, _ := d.Bool()
	s, _ := d.String()
	i128, _ := d.Int128()
	i256, err := d.Int256()
	if err != nil || i32 != -5 || i64 != -1<<40 || f != 3.5 || !b1 || b2 || s != "привет" || i128[1] != 2 || i256[0] != 3 {
		t.Fatalf("примитивы разобраны неверно: %v", err)
	}
	if d.Remaining() != 0 {
		t.Fatal("остались байты")
	}
}

func TestDecoderBounds(t *testing.T) {
	// Заявленная длина строки больше данных.
	if _, err := NewDecoder([]byte{200, 1, 2, 3}).Bytes(); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("длинная строка: %v", err)
	}
	if _, err := NewDecoder([]byte{254, 0xff, 0xff, 0x7f}).Bytes(); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("длинная строка (4 байта): %v", err)
	}
	if _, err := NewDecoder([]byte{255, 0, 0, 0}).Bytes(); err == nil {
		t.Fatal("префикс 255 принят")
	}
	// Вектор на миллиард элементов при 4 байтах данных.
	var e Encoder
	e.PutVectorHeader(1_000_000_000, true)
	e.PutInt32(1)
	if _, err := NewDecoder(e.Bytes()).VectorHeader(true, 4); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("огромный вектор: %v", err)
	}
	var neg Encoder
	neg.PutVectorHeader(-1, false)
	if _, err := NewDecoder(neg.Bytes()).VectorHeader(false, 4); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("отрицательный вектор: %v", err)
	}
	if _, err := NewDecoder([]byte{1, 2, 3, 4}).VectorHeader(true, 4); !errors.Is(err, ErrNotVector) {
		t.Fatalf("не вектор: %v", err)
	}
	if _, err := NewDecoder([]byte{1, 2, 3, 4}).Bool(); !errors.Is(err, ErrBadBool) {
		t.Fatalf("не Bool: %v", err)
	}
	if _, err := NewDecoder([]byte{1, 2}).Int32(); !errors.Is(err, ErrShort) {
		t.Fatalf("короткие данные: %v", err)
	}
}

func TestDepthLimit(t *testing.T) {
	d := NewDecoder(nil)
	for i := 0; i < MaxDepth; i++ {
		if err := d.Enter(); err != nil {
			t.Fatalf("глубина %d: %v", i, err)
		}
	}
	if err := d.Enter(); !errors.Is(err, ErrTooDeep) {
		t.Fatalf("предел глубины не сработал: %v", err)
	}
}
