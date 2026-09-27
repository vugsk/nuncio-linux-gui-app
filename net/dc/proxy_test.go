package dc

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"io"
	"net"
	"sync/atomic"
	"testing"

	"nuncio/net/internal/fakeserver"
	"nuncio/net/transport"
)

// socks5Server — минимальный SOCKS5 с логином и паролем для теста.
func socks5Server(t *testing.T, user, pass string) (string, *atomic.Int32) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	var used atomic.Int32
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				h := make([]byte, 2)
				if _, err := io.ReadFull(c, h); err != nil {
					return
				}
				m := make([]byte, h[1])
				_, _ = io.ReadFull(c, m)
				_, _ = c.Write([]byte{5, 2})
				v := make([]byte, 2)
				_, _ = io.ReadFull(c, v)
				u := make([]byte, v[1])
				_, _ = io.ReadFull(c, u)
				pl := make([]byte, 1)
				_, _ = io.ReadFull(c, pl)
				p := make([]byte, pl[0])
				_, _ = io.ReadFull(c, p)
				if string(u) != user || string(p) != pass {
					_, _ = c.Write([]byte{1, 1})
					return
				}
				_, _ = c.Write([]byte{1, 0})
				req := make([]byte, 4)
				_, _ = io.ReadFull(c, req)
				if req[3] != 1 {
					return
				}
				ip := make([]byte, 6)
				_, _ = io.ReadFull(c, ip)
				target := net.JoinHostPort(net.IP(ip[:4]).String(), itoa(int(binary.BigEndian.Uint16(ip[4:]))))
				up, err := net.Dial("tcp", target)
				if err != nil {
					_, _ = c.Write([]byte{5, 5, 0, 1, 0, 0, 0, 0, 0, 0})
					return
				}
				defer up.Close()
				used.Add(1)
				_, _ = c.Write([]byte{5, 0, 0, 1, 127, 0, 0, 1, 0, 0})
				go func() { _, _ = io.Copy(up, c) }()
				_, _ = io.Copy(c, up)
			}()
		}
	}()
	return ln.Addr().String(), &used
}

func itoa(n int) string {
	b := []byte{}
	for n > 0 {
		b = append([]byte{byte('0' + n%10)}, b...)
		n /= 10
	}
	return string(b)
}

func TestSOCKS5(t *testing.T) {
	s := startServer(t, fakeserver.Options{})
	addr, used := socks5Server(t, "user", "пароль")
	m := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate, Obfuscate: true,
		Proxy: transport.Proxy{Kind: transport.SOCKS5, Addr: addr, User: "user", Password: "пароль"}}, nil)
	defer m.Close()
	getConfig(t, m)
	if used.Load() != 1 {
		t.Fatal("соединение шло не через прокси")
	}
	bad := newManager(map[int]string{2: s.Addr()}, transport.Options{Framing: transport.Intermediate,
		Proxy: transport.Proxy{Kind: transport.SOCKS5, Addr: addr, User: "user", Password: "не тот"}}, nil)
	defer bad.Close()
	if _, err := bad.Ping(context.Background(), 2); err == nil {
		t.Fatal("неверный пароль прокси принят")
	}
}

func TestMTProxy(t *testing.T) {
	secret := []byte("0123456789abcdef")
	for _, tc := range []struct{ name, secret string }{
		{"обычный секрет", hex.EncodeToString(secret)},
		{"секрет dd", "dd" + hex.EncodeToString(secret)},
	} {
		t.Run(tc.name, func(t *testing.T) {
			s := startServer(t, fakeserver.Options{Secret: secret, ExtraPadding: true})
			// Адрес DC намеренно неверный: соединение должно идти к прокси.
			m := newManager(map[int]string{2: "127.0.0.1:1"}, transport.Options{Framing: transport.Abridged,
				Proxy: transport.Proxy{Kind: transport.MTProxy, Addr: s.Addr(), Secret: tc.secret}}, nil)
			defer m.Close()
			getConfig(t, m)
		})
	}
	wrong := startServer(t, fakeserver.Options{Secret: []byte("другой секрет!!!")})
	m := newManager(map[int]string{2: "127.0.0.1:1"}, transport.Options{
		Proxy: transport.Proxy{Kind: transport.MTProxy, Addr: wrong.Addr(), Secret: hex.EncodeToString(secret)}}, nil)
	defer m.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 3e9)
	defer cancel()
	if _, err := m.Ping(ctx, 2); err == nil {
		t.Fatal("неверный секрет MTProxy принят")
	}
}
