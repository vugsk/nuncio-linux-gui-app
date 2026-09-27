package transport

import (
	"bytes"
	"crypto/aes"
	"crypto/cipher"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"io"
	"math/rand"
	"net"
	"testing"
	"time"
)

// server — сторона сервера в тесте: снимает заголовок, расшифровывает
// обфускацию и разбирает кадры независимо от кода клиента.
type server struct {
	c       net.Conn
	framing Framing
	in, out cipher.Stream
}

func newServer(t *testing.T, c net.Conn, f Framing, obfs bool, secret []byte) *server {
	t.Helper()
	s := &server{c: c, framing: f}
	if !obfs {
		n := map[Framing]int{Abridged: 1, Intermediate: 4, PaddedIntermediate: 4}[f]
		h := make([]byte, n)
		if _, err := io.ReadFull(c, h); err != nil {
			t.Fatal(err)
		}
		want := map[Framing][]byte{Abridged: {0xef}, Intermediate: {0xee, 0xee, 0xee, 0xee},
			PaddedIntermediate: {0xdd, 0xdd, 0xdd, 0xdd}}[f]
		if !bytes.Equal(h, want) {
			t.Fatalf("заголовок %x, ожидался %x", h, want)
		}
		return s
	}
	init := make([]byte, 64)
	if _, err := io.ReadFull(c, init); err != nil {
		t.Fatal(err)
	}
	key := func(k []byte) []byte {
		if len(secret) == 0 {
			return k
		}
		h := sha256.Sum256(append(append([]byte{}, k...), secret...))
		return h[:]
	}
	blk, _ := aes.NewCipher(key(init[8:40]))
	s.in = cipher.NewCTR(blk, init[40:56])
	dec := make([]byte, 64)
	s.in.XORKeyStream(dec, init)
	tag := map[Framing]byte{Abridged: 0xef, Intermediate: 0xee, PaddedIntermediate: 0xdd}[f]
	if !bytes.Equal(dec[56:60], bytes.Repeat([]byte{tag}, 4)) {
		t.Fatalf("тег транспорта в init: %x", dec[56:60])
	}
	full := append(append([]byte{}, init[:56]...), dec[56:]...)
	rev := make([]byte, 64)
	for i := range full {
		rev[i] = full[63-i]
	}
	blk2, _ := aes.NewCipher(key(rev[8:40]))
	s.out = cipher.NewCTR(blk2, rev[40:56])
	return s
}

func (s *server) read(t *testing.T, n int) []byte {
	b := make([]byte, n)
	if _, err := io.ReadFull(s.c, b); err != nil {
		t.Fatal(err)
	}
	if s.in != nil {
		s.in.XORKeyStream(b, b)
	}
	return b
}

func (s *server) recv(t *testing.T) []byte {
	var n int
	if s.framing == Abridged {
		h := s.read(t, 1)
		n = int(h[0])
		if n == 0x7f {
			l := s.read(t, 3)
			n = int(l[0]) | int(l[1])<<8 | int(l[2])<<16
		}
		n *= 4
	} else {
		n = int(binary.LittleEndian.Uint32(s.read(t, 4)))
	}
	return s.read(t, n)
}

func (s *server) send(t *testing.T, payload []byte) {
	var frame []byte
	switch s.framing {
	case Abridged:
		n := len(payload) / 4
		if n < 127 {
			frame = append([]byte{byte(n)}, payload...)
		} else {
			frame = append([]byte{0x7f, byte(n), byte(n >> 8), byte(n >> 16)}, payload...)
		}
	default:
		frame = binary.LittleEndian.AppendUint32(nil, uint32(len(payload)))
		frame = append(frame, payload...)
	}
	if s.out != nil {
		s.out.XORKeyStream(frame, frame)
	}
	if _, err := s.c.Write(frame); err != nil {
		t.Error(err) // вызывается из горутины: Fatal здесь нельзя
	}
}

// encryptedLike — пакет, похожий на зашифрованное сообщение: 24 + 16k байт.
func encryptedLike(r *rand.Rand, k int) []byte {
	b := make([]byte, 24+16*k)
	r.Read(b)
	b[0] |= 1 // не нулевой auth_key_id
	return b
}

func TestFramingsBothDirections(t *testing.T) {
	r := rand.New(rand.NewSource(1))
	for _, f := range []Framing{Abridged, Intermediate, PaddedIntermediate} {
		for _, obfs := range []bool{false, true} {
			for _, secret := range [][]byte{nil, bytes.Repeat([]byte{7}, 16)} {
				if secret != nil && !obfs {
					continue
				}
				cli, srv := net.Pipe()
				done := make(chan *Conn, 1)
				go func() {
					c, err := New(cli, Options{Framing: f, Obfuscate: obfs, Secret: secret, DC: 2})
					if err != nil {
						t.Error(err)
					}
					done <- c
				}()
				s := newServer(t, srv, f, obfs, secret)
				c := <-done
				for i := 0; i < 20; i++ {
					// Размеры: маленькие и больше 127*4 (длинная форма abridged).
					msg := encryptedLike(r, []int{1, 3, 40, 200}[i%4])
					go func() {
						if err := c.Send(msg); err != nil {
							t.Error(err)
						}
					}()
					got := s.recv(t)
					if f == PaddedIntermediate {
						m, err := stripPadding(got)
						if err != nil {
							t.Fatal(err)
						}
						got = got[:m]
					}
					if !bytes.Equal(got, msg) {
						t.Fatalf("%v obfs=%v: клиент -> сервер не совпало", f, obfs)
					}
					reply := encryptedLike(r, []int{2, 130, 5, 1}[i%4])
					go s.send(t, reply)
					back, err := c.Recv()
					if err != nil || !bytes.Equal(back, reply) {
						t.Fatalf("%v obfs=%v: сервер -> клиент: %v", f, obfs, err)
					}
				}
				c.Close()
				srv.Close()
			}
		}
	}
}

func TestTransportErrorAndLimits(t *testing.T) {
	cli, srv := net.Pipe()
	defer srv.Close()
	done := make(chan *Conn, 1)
	go func() {
		c, _ := New(cli, Options{Framing: Intermediate})
		done <- c
	}()
	s := newServer(t, srv, Intermediate, false, nil)
	c := <-done
	defer c.Close()

	// Код ошибки -404 (нет ключа авторизации).
	go s.send(t, binary.LittleEndian.AppendUint32(nil, uint32(0xfffffe6c)))
	_, err := c.Recv()
	var te *Error
	if !errors.As(err, &te) || te.Code != -404 {
		t.Fatalf("транспортная ошибка: %v", err)
	}
	// Заявлен пакет больше предела.
	go func() { _, _ = srv.Write(binary.LittleEndian.AppendUint32(nil, MaxPacket+4)) }()
	if _, err := c.Recv(); !errors.Is(err, ErrTooLarge) {
		t.Fatalf("огромный пакет: %v", err)
	}
	if err := c.Send([]byte{1, 2, 3}); err == nil {
		t.Fatal("пакет не кратный 4 принят")
	}
}

func TestStripPadding(t *testing.T) {
	// Открытое сообщение: длина из заголовка.
	plain := make([]byte, 20+12)
	binary.LittleEndian.PutUint32(plain[16:], 12)
	// Настоящий DC4 дополнил открытый ответ на 72 байта — хвост любой длины.
	for _, pad := range []int{0, 1, 15, 16, 72, 1000} {
		b := append(append([]byte{}, plain...), make([]byte, pad)...)
		if m, err := stripPadding(b); err != nil || m != 32 {
			t.Fatalf("открытое, дополнение %d: %d %v", pad, m, err)
		}
	}
	// Зашифрованное: 24 + 16k.
	enc := make([]byte, 24+48)
	enc[0] = 1
	for pad := 0; pad < 16; pad++ {
		b := append(append([]byte{}, enc...), make([]byte, pad)...)
		if m, err := stripPadding(b); err != nil || m != 72 {
			t.Fatalf("зашифрованное, дополнение %d: %d %v", pad, m, err)
		}
	}
	bad := make([]byte, 20+4)
	binary.LittleEndian.PutUint32(bad[16:], 1000)
	if _, err := stripPadding(bad); err == nil {
		t.Fatal("длина из заголовка больше пакета принята")
	}
}

func TestCloseDuringRecv(t *testing.T) {
	cli, srv := net.Pipe()
	defer srv.Close()
	done := make(chan *Conn, 1)
	go func() {
		c, _ := New(cli, Options{Framing: Intermediate, Obfuscate: true})
		done <- c
	}()
	_ = newServer(t, srv, Intermediate, true, nil)
	c := <-done
	errc := make(chan error, 1)
	go func() {
		_, err := c.Recv()
		errc <- err
	}()
	time.Sleep(20 * time.Millisecond)
	c.Close()
	if err := <-errc; err == nil {
		t.Fatal("Recv после Close без ошибки")
	}
}
