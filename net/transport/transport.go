// Package transport — транспорт MTProto поверх TCP
// (core.telegram.org/mtproto/mtproto-transports): кадрирование abridged,
// intermediate и padded intermediate, обфускация obfuscated2 через ядро,
// транспортные ошибки. Сессию и шифрование сообщений делает пакет session.
package transport

import (
	"crypto/rand" // только для случайного дополнения padded intermediate, не криптография протокола
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"sync"
	"time"

	"nuncio/net/core"
)

// Framing — вид кадрирования.
type Framing int

const (
	Abridged Framing = iota
	Intermediate
	PaddedIntermediate
)

func (f Framing) String() string {
	return [...]string{"abridged", "intermediate", "padded intermediate"}[f]
}

// MaxPacket — предел размера входящего пакета (защита от огромных выделений).
const MaxPacket = 16 << 20

// Error — транспортная ошибка: пакет из 4 байт с отрицательным кодом
// (-404 нет ключа, -429 слишком много соединений, -444 неверный DC).
type Error struct{ Code int32 }

func (e *Error) Error() string {
	return fmt.Sprintf("transport: ошибка сервера %d", e.Code)
}

// ErrTooLarge — входящий пакет больше MaxPacket или с неверной длиной.
var ErrTooLarge = errors.New("transport: неверная длина пакета")

// Options — параметры соединения.
type Options struct {
	Framing Framing
	// Obfuscate — обфускация obfuscated2 (рекомендуется: защищает от блокировок по сигнатуре).
	Obfuscate bool
	// DC — номер DC для заголовка обфускации (0 — не указывать; для MTProxy обязателен).
	DC int
	// Secret — секрет MTProxy (16 или 17 байт); пусто — прямое соединение.
	Secret []byte
	// Timeout — предел на установку соединения и на запись.
	Timeout time.Duration
	// Proxy — SOCKS5 или MTProxy (для MTProxy Secret и кадрирование берутся из него).
	Proxy Proxy
}

// Conn — соединение с DC: целые пакеты MTProto на вход и на выход.
// Send и Recv можно вызывать одновременно из разных горутин (по одной на направление).
type Conn struct {
	c    net.Conn
	opt  Options
	obfs *core.Obfs
	wmu  sync.Mutex
	rmu  sync.Mutex
}

// Dial устанавливает соединение и отправляет заголовок транспорта.
func Dial(addr string, opt Options) (*Conn, error) {
	if opt.Timeout == 0 {
		opt.Timeout = 15 * time.Second
	}
	nc, err := dialVia(addr, &opt)
	if err != nil {
		return nil, err
	}
	c, err := New(nc, opt)
	if err != nil {
		nc.Close()
		return nil, err
	}
	return c, nil
}

// New оборачивает уже открытое соединение (используется и в тестах с net.Pipe).
func New(nc net.Conn, opt Options) (*Conn, error) {
	c := &Conn{c: nc, opt: opt}
	if opt.Timeout == 0 {
		c.opt.Timeout = 15 * time.Second
	}
	var header []byte
	if opt.Obfuscate || len(opt.Secret) != 0 {
		var tr int
		switch opt.Framing {
		case Abridged:
			tr = core.TransportAbridged
		case Intermediate:
			tr = core.TransportIntermediate
		case PaddedIntermediate:
			tr = core.TransportPadded
		}
		o, init, err := core.NewObfs(tr, opt.DC, opt.Secret)
		if err != nil {
			return nil, fmt.Errorf("transport: обфускация: %w", err)
		}
		c.obfs = o
		header = init
	} else {
		switch opt.Framing {
		case Abridged:
			header = []byte{0xef}
		case Intermediate:
			header = []byte{0xee, 0xee, 0xee, 0xee}
		case PaddedIntermediate:
			header = []byte{0xdd, 0xdd, 0xdd, 0xdd}
		}
	}
	if err := c.writeRaw(header, false); err != nil {
		return nil, err
	}
	return c, nil
}

// writeRaw пишет байты (через обфускацию, кроме заголовка init).
func (c *Conn) writeRaw(b []byte, encrypt bool) error {
	if encrypt && c.obfs != nil {
		c.obfs.Encrypt(b)
	}
	_ = c.c.SetWriteDeadline(time.Now().Add(c.opt.Timeout))
	_, err := c.c.Write(b)
	return err
}

// Send отправляет один пакет MTProto (длина кратна 4).
func (c *Conn) Send(payload []byte) error {
	if len(payload)%4 != 0 || len(payload) == 0 {
		return fmt.Errorf("transport: длина пакета %d не кратна 4", len(payload))
	}
	var frame []byte
	switch c.opt.Framing {
	case Abridged:
		n := len(payload) / 4
		if n < 127 {
			frame = append([]byte{byte(n)}, payload...)
		} else {
			frame = append([]byte{0x7f, byte(n), byte(n >> 8), byte(n >> 16)}, payload...)
		}
	case Intermediate:
		frame = binary.LittleEndian.AppendUint32(nil, uint32(len(payload)))
		frame = append(frame, payload...)
	case PaddedIntermediate:
		var pad [1]byte
		_, _ = rand.Read(pad[:])
		padding := make([]byte, int(pad[0])%16)
		_, _ = rand.Read(padding)
		frame = binary.LittleEndian.AppendUint32(nil, uint32(len(payload)+len(padding)))
		frame = append(frame, payload...)
		frame = append(frame, padding...)
	}
	c.wmu.Lock()
	defer c.wmu.Unlock()
	return c.writeRaw(frame, true)
}

func (c *Conn) readFull(b []byte) error {
	if _, err := io.ReadFull(c.c, b); err != nil {
		return err
	}
	if c.obfs != nil {
		c.obfs.Decrypt(b)
	}
	return nil
}

// Recv читает один пакет. Блокирует до прихода данных или SetReadDeadline.
func (c *Conn) Recv() ([]byte, error) {
	c.rmu.Lock()
	defer c.rmu.Unlock()
	var n int
	switch c.opt.Framing {
	case Abridged:
		var h [1]byte
		if err := c.readFull(h[:]); err != nil {
			return nil, err
		}
		v := int(h[0] & 0x7f) // старший бит — quick ack, не используется
		if v == 0x7f {
			var l [3]byte
			if err := c.readFull(l[:]); err != nil {
				return nil, err
			}
			v = int(l[0]) | int(l[1])<<8 | int(l[2])<<16
		}
		n = v * 4
	default:
		var l [4]byte
		if err := c.readFull(l[:]); err != nil {
			return nil, err
		}
		n = int(binary.LittleEndian.Uint32(l[:]))
	}
	if n < 4 || n > MaxPacket {
		return nil, ErrTooLarge
	}
	buf := make([]byte, n)
	if err := c.readFull(buf); err != nil {
		return nil, err
	}
	if n == 4 {
		return nil, &Error{Code: int32(binary.LittleEndian.Uint32(buf))}
	}
	if c.opt.Framing == PaddedIntermediate {
		m, err := stripPadding(buf)
		if err != nil {
			return nil, err
		}
		buf = buf[:m]
	}
	return buf, nil
}

// stripPadding находит длину сообщения MTProto в пакете с дополнением.
// У открытого сообщения длина тела записана в заголовке, хвост любой длины
// отбрасывается: настоящий сервер дополняет открытые ответы больше, чем на
// 0–15 байт из документации (DC4 прислал 72 байта, проверка В2). У
// зашифрованного длина равна 24 + 16k — берётся наибольшая, как в Telegram
// Desktop (session_private.cpp); неверную длину отвергнет проверка msg_key.
func stripPadding(b []byte) (int, error) {
	if len(b) >= 20 && binary.LittleEndian.Uint64(b) == 0 {
		m := 20 + int(binary.LittleEndian.Uint32(b[16:]))
		if m <= len(b) {
			return m, nil
		}
		return 0, ErrTooLarge
	}
	if len(b) < 24+16 {
		return 0, ErrTooLarge
	}
	return 24 + (len(b)-24)/16*16, nil
}

// SetReadDeadline ограничивает ожидание Recv.
func (c *Conn) SetReadDeadline(t time.Time) error { return c.c.SetReadDeadline(t) }

// Close закрывает соединение. Сначала закрывается сокет (это будит Recv и Send),
// затем, дождавшись обоих направлений, освобождается состояние обфускации —
// иначе параллельный Recv мог бы расшифровывать уже освобождённым хэндлом ядра.
func (c *Conn) Close() error {
	err := c.c.Close()
	c.rmu.Lock()
	c.wmu.Lock()
	if c.obfs != nil {
		c.obfs.Free()
		c.obfs = nil
	}
	c.wmu.Unlock()
	c.rmu.Unlock()
	return err
}
