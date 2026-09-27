package transport

import (
	"encoding/base64"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net"
	"strconv"
	"strings"
	"time"
)

// ProxyKind — вид прокси (Дизайн - Вход, шаг 7; Настройки, раздел «Соединение»).
type ProxyKind int

const (
	NoProxy ProxyKind = iota
	SOCKS5
	MTProxy
)

// Proxy — параметры прокси.
type Proxy struct {
	Kind     ProxyKind
	Addr     string // host:port
	User     string // SOCKS5
	Password string // SOCKS5
	Secret   string // MTProxy: hex (32 или dd+32 знака) или base64url
}

// ErrFakeTLS — секрет MTProxy с маскировкой под TLS (ee…) пока не поддерживается.
var ErrFakeTLS = errors.New("transport: секреты MTProxy вида ee… (маскировка под TLS) не поддерживаются")

// ErrBadSecret — секрет MTProxy не разобран.
var ErrBadSecret = errors.New("transport: неверный секрет MTProxy")

// ParseSecret разбирает секрет MTProxy: 16 байт (обычный) или dd + 16 байт
// (со случайным дополнением — только padded intermediate). Возвращает байты для
// ядра (16 или 17) и признак dd.
func ParseSecret(s string) ([]byte, bool, error) {
	s = strings.TrimSpace(s)
	b, err := hex.DecodeString(s)
	if err != nil {
		if b, err = base64.RawURLEncoding.DecodeString(strings.TrimRight(s, "=")); err != nil {
			return nil, false, ErrBadSecret
		}
	}
	switch {
	case len(b) == 16:
		return b, false, nil
	case len(b) == 17 && b[0] == 0xdd:
		return b, true, nil
	case len(b) > 17 && b[0] == 0xee:
		return nil, false, ErrFakeTLS
	}
	return nil, false, ErrBadSecret
}

// dialVia открывает TCP-соединение с адресом DC напрямую или через прокси.
// Для MTProxy соединение идёт к прокси, а DC указывается в заголовке обфускации.
func dialVia(addr string, opt *Options) (net.Conn, error) {
	switch opt.Proxy.Kind {
	case NoProxy:
		return net.DialTimeout("tcp", addr, opt.Timeout)
	case MTProxy:
		sec, dd, err := ParseSecret(opt.Proxy.Secret)
		if err != nil {
			return nil, err
		}
		opt.Secret, opt.Obfuscate = sec, true
		if dd {
			opt.Framing = PaddedIntermediate
		}
		return net.DialTimeout("tcp", opt.Proxy.Addr, opt.Timeout)
	case SOCKS5:
		c, err := net.DialTimeout("tcp", opt.Proxy.Addr, opt.Timeout)
		if err != nil {
			return nil, err
		}
		_ = c.SetDeadline(time.Now().Add(opt.Timeout))
		if err := socks5Connect(c, addr, opt.Proxy.User, opt.Proxy.Password); err != nil {
			c.Close()
			return nil, err
		}
		_ = c.SetDeadline(time.Time{})
		return c, nil
	}
	return nil, fmt.Errorf("transport: неизвестный вид прокси %d", opt.Proxy.Kind)
}

// socks5Connect — RFC 1928 (CONNECT) и RFC 1929 (логин и пароль).
func socks5Connect(c net.Conn, addr, user, pass string) error {
	methods := []byte{0x00}
	if user != "" {
		methods = []byte{0x02, 0x00}
	}
	if _, err := c.Write(append([]byte{5, byte(len(methods))}, methods...)); err != nil {
		return err
	}
	var rep [2]byte
	if _, err := io.ReadFull(c, rep[:]); err != nil {
		return err
	}
	if rep[0] != 5 {
		return errors.New("transport: SOCKS5: сервер ответил не по протоколу SOCKS5")
	}
	switch rep[1] {
	case 0x00:
	case 0x02:
		if len(user) > 255 || len(pass) > 255 {
			return errors.New("transport: SOCKS5: логин или пароль длиннее 255 байт")
		}
		req := append([]byte{1, byte(len(user))}, user...)
		req = append(append(req, byte(len(pass))), pass...)
		if _, err := c.Write(req); err != nil {
			return err
		}
		if _, err := io.ReadFull(c, rep[:]); err != nil {
			return err
		}
		if rep[1] != 0 {
			return errors.New("transport: SOCKS5: неверный логин или пароль")
		}
	default:
		return errors.New("transport: SOCKS5: сервер не принял ни один способ входа")
	}
	host, portS, err := net.SplitHostPort(addr)
	if err != nil {
		return err
	}
	port, err := strconv.Atoi(portS)
	if err != nil {
		return err
	}
	req := []byte{5, 1, 0}
	if ip := net.ParseIP(host); ip != nil && ip.To4() != nil {
		req = append(append(req, 1), ip.To4()...)
	} else if ip != nil {
		req = append(append(req, 4), ip.To16()...)
	} else {
		req = append(append(req, 3, byte(len(host))), host...)
	}
	req = binary.BigEndian.AppendUint16(req, uint16(port))
	if _, err := c.Write(req); err != nil {
		return err
	}
	var head [4]byte
	if _, err := io.ReadFull(c, head[:]); err != nil {
		return err
	}
	if head[1] != 0 {
		return fmt.Errorf("transport: SOCKS5: прокси отказал в соединении (код %d)", head[1])
	}
	var skip int
	switch head[3] {
	case 1:
		skip = 4
	case 4:
		skip = 16
	case 3:
		var l [1]byte
		if _, err := io.ReadFull(c, l[:]); err != nil {
			return err
		}
		skip = int(l[0])
	default:
		return errors.New("transport: SOCKS5: неверный адрес в ответе")
	}
	_, err = io.ReadFull(c, make([]byte, skip+2))
	return err
}
