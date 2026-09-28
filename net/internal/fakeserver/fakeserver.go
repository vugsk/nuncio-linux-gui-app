// Package fakeserver — поддельный сервер MTProto для тестов сети без Telegram.
//
// Серверная сторона написана независимо от ядра (стандартная криптография Go,
// только в тестовом коде): расшифровка RSA_PAD своим закрытым ключом, DH,
// конверт в направлении сервер -> клиент (x = 8), обфускация obfuscated2.
// Поэтому тест «клиент nuncio против fakeserver» проверяет и совместимость
// двух реализаций, а не одну реализацию саму с собой.
//
// Управляемые «неприятности» (Options) позволяют проверить поведение сессии:
// смену соли, рассинхрон времени, FLOOD_WAIT, MIGRATE, контейнеры, gzip,
// new_session_created, dh_gen_retry, неизвестные обновления.
package fakeserver

import (
	"bytes"
	"compress/gzip"
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"sync"
	"time"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// dh_prime из документации (тот же, что встроен в ядро), g = 3.
var dhPrime, _ = new(big.Int).SetString("c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c1405822"+
	"9493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18"+
	"faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56"+
	"b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9a"+
	"e4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4f"+
	"adf034b10403119cd8e3b92fcc5b", 16)

// Options — поведение сервера.
type Options struct {
	DC               int
	DHRetryOnce      bool           // первый set_client_DH_params -> dh_gen_retry
	BadSaltFirst     bool           // первый содержательный запрос сессии -> bad_server_salt
	TimeSkewFirst    bool           // первый запрос -> bad_msg_notification 16 (msg_id слишком мал)
	GzipResults      bool           // ответы rpc_result в gzip_packed
	NewSession       bool           // перед первым ответом — new_session_created в контейнере
	FloodOnce        map[string]int // метод -> FLOOD_WAIT_n на первый вызов
	Migrate          map[string]int // метод -> *_MIGRATE_n (всегда)
	PushUnknown      bool           // после первого ответа прислать неизвестный конструктор (обновление)
	DropAfterReplies int            // закрыть соединение после стольких ответов (0 — не закрывать)
	Account          *Account       // аккаунт для методов входа (account.go); общий для нескольких DC
	Secret           []byte         // секрет MTProxy (16 байт): ключи обфускации с его участием
	ExtraPadding     bool           // padded intermediate: дописывать к ответам до 80 байт (как настоящий DC4)
}

// Server — поддельный сервер.
type Server struct {
	ln   net.Listener
	addr string
	opt  Options
	priv *rsa.PrivateKey
	fp   uint64

	mu       sync.Mutex
	keys     map[uint64][]byte // auth_key_id -> auth_key
	calls    map[string]int
	accepted int
	conns    map[*conn]bool
}

var (
	keyOnce sync.Once
	keyPriv *rsa.PrivateKey
	keyErr  error
)

// Start запускает сервер на 127.0.0.1. Ключ RSA-2048 один на процесс: ядро
// принимает не больше 16 дополнительных ключей, а тестов с серверами больше.
func Start(opt Options) (*Server, error) {
	keyOnce.Do(func() { keyPriv, keyErr = rsa.GenerateKey(rand.Reader, 2048) })
	priv, err := keyPriv, keyErr
	if err != nil {
		return nil, err
	}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	s := &Server{ln: ln, opt: opt, priv: priv, keys: map[uint64][]byte{}, calls: map[string]int{},
		conns: map[*conn]bool{}}
	s.fp = fingerprint(&priv.PublicKey)
	s.addr = ln.Addr().String()
	go s.accept(ln)
	return s, nil
}

// Addr — адрес для transport.Dial.
func (s *Server) Addr() string { return s.addr }

// PublicKey — модуль (256 байт) и экспонента для core.AddRSAKey.
func (s *Server) PublicKey() ([]byte, uint32) {
	return s.priv.N.FillBytes(make([]byte, 256)), uint32(s.priv.E)
}

// Fingerprint — отпечаток ключа, вычисленный сервером.
func (s *Server) Fingerprint() uint64 { return s.fp }

// Calls — сколько раз вызывался метод (после раскрытия обёрток).
func (s *Server) Calls(method string) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.calls[method]
}

// Close останавливает сервер.
func (s *Server) Close() error {
	s.mu.Lock()
	ln := s.ln
	s.mu.Unlock()
	return ln.Close()
}

// Stop — «выключить сеть»: закрыть приём и все соединения. Restart — включить
// снова на том же адресе (ключи и аккаунт сохраняются).
func (s *Server) Stop() {
	s.mu.Lock()
	_ = s.ln.Close()
	for c := range s.conns {
		_ = c.c.Close()
	}
	s.mu.Unlock()
}

func (s *Server) Restart() error {
	ln, err := net.Listen("tcp", s.addr)
	if err != nil {
		return err
	}
	s.mu.Lock()
	s.ln = ln
	s.mu.Unlock()
	go s.accept(ln)
	return nil
}

func tlBytes(b []byte) []byte {
	var e tlbin.Encoder
	e.PutBytes(b)
	return e.Bytes()
}

func fingerprint(k *rsa.PublicKey) uint64 {
	n := tlBytes(k.N.FillBytes(make([]byte, 256)))
	e := tlBytes(big.NewInt(int64(k.E)).Bytes())
	h := sha1.Sum(append(n, e...))
	return binary.LittleEndian.Uint64(h[12:20])
}

func (s *Server) accept(ln net.Listener) {
	for {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		s.mu.Lock()
		s.accepted++
		s.mu.Unlock()
		cn := &conn{s: s, c: c}
		s.mu.Lock()
		s.conns[cn] = true
		s.mu.Unlock()
		go func() {
			defer c.Close()
			_ = cn.serve()
			s.mu.Lock()
			delete(s.conns, cn)
			s.mu.Unlock()
		}()
	}
}

// ---------------------------------------------------------------- транспорт

type conn struct {
	s        *Server
	c        net.Conn
	wmu      sync.Mutex // запись: ответы из цикла и обновления из теста
	in, out  cipher.Stream
	abridged bool
	padded   bool

	// состояние обмена ключами
	nonce, serverNonce [16]byte
	newNonce           [32]byte
	a                  *big.Int
	retried            bool
	prevAux            int64 // auth_key_aux_hash неудачной попытки (ожидаемый retry_id)

	// состояние сессии
	keyID     uint64
	key       []byte
	sessionID int64
	salt      int64
	lastID    int64
	seq       int32
	inited    bool
	answered  int
	firstDone bool
}

func (c *conn) readRaw(n int) ([]byte, error) {
	b := make([]byte, n)
	if _, err := io.ReadFull(c.c, b); err != nil {
		return nil, err
	}
	if c.in != nil {
		c.in.XORKeyStream(b, b)
	}
	return b, nil
}

func (c *conn) handshake() error {
	first := make([]byte, 1)
	if _, err := io.ReadFull(c.c, first); err != nil {
		return err
	}
	switch first[0] {
	case 0xef:
		c.abridged = true
		return nil
	case 0xee, 0xdd:
		rest := make([]byte, 3)
		if _, err := io.ReadFull(c.c, rest); err != nil {
			return err
		}
		if bytes.Equal(rest, []byte{first[0], first[0], first[0]}) {
			c.padded = first[0] == 0xdd
			return nil
		}
		return c.obfuscated(append(first, rest...))
	default:
		rest := make([]byte, 3)
		if _, err := io.ReadFull(c.c, rest); err != nil {
			return err
		}
		return c.obfuscated(append(first, rest...))
	}
}

func (c *conn) obfuscated(head []byte) error {
	tail := make([]byte, 60)
	if _, err := io.ReadFull(c.c, tail); err != nil {
		return err
	}
	init := append(head, tail...)
	blk, _ := aes.NewCipher(c.obfsKey(init[8:40]))
	c.in = cipher.NewCTR(blk, init[40:56])
	dec := make([]byte, 64)
	c.in.XORKeyStream(dec, init)
	switch {
	case bytes.Equal(dec[56:60], []byte{0xef, 0xef, 0xef, 0xef}):
		c.abridged = true
	case bytes.Equal(dec[56:60], []byte{0xee, 0xee, 0xee, 0xee}):
	case bytes.Equal(dec[56:60], []byte{0xdd, 0xdd, 0xdd, 0xdd}):
		c.padded = true
	default:
		return errors.New("fakeserver: неизвестный тег транспорта")
	}
	full := append(append([]byte{}, init[:56]...), dec[56:]...)
	rev := make([]byte, 64)
	for i := range full {
		rev[i] = full[63-i]
	}
	blk2, _ := aes.NewCipher(c.obfsKey(rev[8:40]))
	c.out = cipher.NewCTR(blk2, rev[40:56])
	return nil
}

// obfsKey — ключ AES-CTR: для MTProxy sha256(ключ из init + секрет).
func (c *conn) obfsKey(k []byte) []byte {
	if len(c.s.opt.Secret) == 0 {
		return k
	}
	h := sha256.Sum256(append(append([]byte{}, k...), c.s.opt.Secret...))
	return h[:]
}

func (c *conn) recvPacket() ([]byte, error) {
	var n int
	if c.abridged {
		h, err := c.readRaw(1)
		if err != nil {
			return nil, err
		}
		n = int(h[0])
		if n == 0x7f {
			l, err := c.readRaw(3)
			if err != nil {
				return nil, err
			}
			n = int(l[0]) | int(l[1])<<8 | int(l[2])<<16
		}
		n *= 4
	} else {
		l, err := c.readRaw(4)
		if err != nil {
			return nil, err
		}
		n = int(binary.LittleEndian.Uint32(l))
	}
	if n > 16<<20 {
		return nil, errors.New("fakeserver: пакет слишком велик")
	}
	b, err := c.readRaw(n)
	if err != nil {
		return nil, err
	}
	if c.padded { // клиент дописывает 0–15 байт: у зашифрованного длина 24+16k, у открытого — из заголовка
		if binary.LittleEndian.Uint64(b) == 0 {
			b = b[:20+binary.LittleEndian.Uint32(b[16:])]
		} else {
			b = b[:24+(len(b)-24)/16*16]
		}
	}
	return b, nil
}

func (c *conn) sendPacket(p []byte) error {
	var frame []byte
	if c.abridged {
		n := len(p) / 4
		if n < 127 {
			frame = append([]byte{byte(n)}, p...)
		} else {
			frame = append([]byte{0x7f, byte(n), byte(n >> 8), byte(n >> 16)}, p...)
		}
	} else {
		pad := 0
		if c.padded && c.s.opt.ExtraPadding {
			// Открытые ответы настоящий сервер дополняет сильнее (до 72+ байт),
			// зашифрованные — на 0–15 (длина конверта 24+16k однозначна).
			var r [1]byte
			_, _ = rand.Read(r[:])
			pad = int(r[0]) % 16
			if len(p) >= 8 && binary.LittleEndian.Uint64(p) == 0 {
				pad = int(r[0]) % 81
			}
		}
		frame = binary.LittleEndian.AppendUint32(nil, uint32(len(p)+pad))
		frame = append(frame, p...)
		frame = append(frame, make([]byte, pad)...)
	}
	if c.out != nil {
		c.out.XORKeyStream(frame, frame)
	}
	_, err := c.c.Write(frame)
	return err
}

// ---------------------------------------------------------------- цикл

func (c *conn) serve() error {
	if err := c.handshake(); err != nil {
		return err
	}
	for {
		pkt, err := c.recvPacket()
		if err != nil {
			return err
		}
		if len(pkt) >= 8 && binary.LittleEndian.Uint64(pkt) == 0 {
			if err := c.plain(pkt); err != nil {
				return err
			}
			continue
		}
		if err := c.encrypted(pkt); err != nil {
			return err
		}
		if c.s.opt.DropAfterReplies > 0 && c.answered >= c.s.opt.DropAfterReplies {
			return nil // обрыв соединения
		}
	}
}

func (c *conn) msgID(response bool) int64 {
	id := time.Now().Unix()<<32 | int64(time.Now().Nanosecond())&^3
	if id <= c.lastID {
		id = c.lastID + 4
	}
	c.lastID = id
	if response {
		return id | 1
	}
	return id | 3
}

// ---------------------------------------------------------------- обмен ключами

func (c *conn) sendPlain(o tlbin.Object) error {
	c.wmu.Lock()
	defer c.wmu.Unlock()
	body, err := tlbin.Marshal(o)
	if err != nil {
		return err
	}
	pkt := make([]byte, 20)
	binary.LittleEndian.PutUint64(pkt[8:], uint64(c.msgID(true)))
	binary.LittleEndian.PutUint32(pkt[16:], uint32(len(body)))
	return c.sendPacket(append(pkt, body...))
}

func ige(key, iv, data []byte, enc bool) []byte {
	blk, _ := aes.NewCipher(key)
	out := make([]byte, len(data))
	x := append([]byte{}, iv[:16]...)
	y := append([]byte{}, iv[16:32]...)
	t := make([]byte, 16)
	for i := 0; i < len(data); i += 16 {
		src := data[i : i+16]
		if enc {
			for j := range t {
				t[j] = src[j] ^ x[j]
			}
			blk.Encrypt(t, t)
			for j := range t {
				out[i+j] = t[j] ^ y[j]
			}
			x = append(x[:0], out[i:i+16]...)
			y = append(y[:0], src...)
		} else {
			for j := range t {
				t[j] = src[j] ^ y[j]
			}
			blk.Decrypt(t, t)
			for j := range t {
				out[i+j] = t[j] ^ x[j]
			}
			y = append(y[:0], out[i:i+16]...)
			x = append(x[:0], src...)
		}
	}
	return out
}

func tmpKey(newNonce []byte, serverNonce []byte) (key, iv []byte) {
	ns := sha1.Sum(append(append([]byte{}, newNonce...), serverNonce...))
	sn := sha1.Sum(append(append([]byte{}, serverNonce...), newNonce...))
	nn := sha1.Sum(append(append([]byte{}, newNonce...), newNonce...))
	key = append(append([]byte{}, ns[:]...), sn[:12]...)
	iv = append(append(append([]byte{}, sn[12:20]...), nn[:]...), newNonce[:4]...)
	return
}

func randomPrime() uint64 {
	for {
		p, _ := rand.Prime(rand.Reader, 31)
		if p.Uint64() > 1<<30 {
			return p.Uint64()
		}
	}
}

func (c *conn) plain(pkt []byte) error {
	if len(pkt) < 20 {
		return errors.New("fakeserver: короткое открытое сообщение")
	}
	o, err := tl.DecodeObject(tlbin.NewDecoder(pkt[20:]))
	if err != nil {
		return err
	}
	switch m := o.(type) {
	case *tl.ReqPqMultiRequest:
		c.nonce = m.Nonce
		_, _ = rand.Read(c.serverNonce[:])
		p, q := randomPrime(), randomPrime()
		pq := new(big.Int).SetUint64(p * q).Bytes()
		return c.sendPlain(&tl.ResPQ{Nonce: c.nonce, ServerNonce: c.serverNonce, Pq: string(pq),
			ServerPublicKeyFingerprints: []int64{0x1234, int64(c.s.fp)}})
	case *tl.ReqDHParamsRequest:
		if m.Nonce != c.nonce || m.ServerNonce != c.serverNonce || uint64(m.PublicKeyFingerprint) != c.s.fp {
			return errors.New("fakeserver: req_DH_params: nonce или ключ")
		}
		inner, err := c.rsaUnpad([]byte(m.EncryptedData))
		if err != nil {
			return err
		}
		io, err := tl.DecodeObject(tlbin.NewDecoder(inner))
		if err != nil {
			return err
		}
		pqi, ok := io.(*tl.PQInnerDataDc)
		if !ok || pqi.Nonce != c.nonce || pqi.ServerNonce != c.serverNonce || int(pqi.Dc) != c.s.opt.DC {
			return fmt.Errorf("fakeserver: p_q_inner_data_dc: %T dc=%v", io, pqi)
		}
		pv := new(big.Int).SetBytes([]byte(pqi.P))
		qv := new(big.Int).SetBytes([]byte(pqi.Q))
		if new(big.Int).Mul(pv, qv).Cmp(new(big.Int).SetBytes([]byte(pqi.Pq))) != 0 || pv.Cmp(qv) >= 0 {
			return errors.New("fakeserver: неверное разложение pq")
		}
		c.newNonce = pqi.NewNonce
		return c.sendDHParams()
	case *tl.SetClientDHParamsRequest:
		key, iv := tmpKey(c.newNonce[:], c.serverNonce[:])
		dec := ige(key, iv, []byte(m.EncryptedData), false)
		var cdi *tl.ClientDHInnerData
		for pad := 0; pad < 16 && cdi == nil; pad++ {
			data := dec[20 : len(dec)-pad]
			h := sha1.Sum(data)
			if bytes.Equal(h[:], dec[:20]) {
				o, err := tl.DecodeObject(tlbin.NewDecoder(data))
				if err == nil {
					cdi, _ = o.(*tl.ClientDHInnerData)
				}
			}
		}
		if cdi == nil || cdi.Nonce != c.nonce {
			return errors.New("fakeserver: client_DH_inner_data")
		}
		gb := new(big.Int).SetBytes([]byte(cdi.GB))
		authKey := new(big.Int).Exp(gb, c.a, dhPrime).FillBytes(make([]byte, 256))
		kh := sha1.Sum(authKey)
		nonceHash := func(n byte) (out [16]byte) {
			h := sha1.Sum(append(append(append([]byte{}, c.newNonce[:]...), n), kh[:8]...))
			copy(out[:], h[4:])
			return
		}
		if c.s.opt.DHRetryOnce && !c.retried {
			c.retried = true
			c.prevAux = int64(binary.LittleEndian.Uint64(kh[:8]))
			return c.sendPlain(&tl.DhGenRetry{Nonce: c.nonce, ServerNonce: c.serverNonce, NewNonceHash2: nonceHash(2)})
		}
		if c.retried && cdi.RetryId != c.prevAux {
			return fmt.Errorf("fakeserver: retry_id %x, ожидался auth_key_aux_hash %x", cdi.RetryId, c.prevAux)
		}
		c.s.mu.Lock()
		c.s.keys[binary.LittleEndian.Uint64(kh[12:20])] = authKey
		c.s.mu.Unlock()
		return c.sendPlain(&tl.DhGenOk{Nonce: c.nonce, ServerNonce: c.serverNonce, NewNonceHash1: nonceHash(1)})
	}
	return fmt.Errorf("fakeserver: неожиданное открытое сообщение %T", o)
}

func (c *conn) rsaUnpad(enc []byte) ([]byte, error) {
	m := new(big.Int).Exp(new(big.Int).SetBytes(enc), c.s.priv.D, c.s.priv.N).FillBytes(make([]byte, 256))
	tempKeyXor, aesEnc := m[:32], m[32:]
	h := sha256.Sum256(aesEnc)
	tempKey := make([]byte, 32)
	for i := range tempKey {
		tempKey[i] = tempKeyXor[i] ^ h[i]
	}
	dwh := ige(tempKey, make([]byte, 32), aesEnc, false)
	rev := dwh[:192]
	dwp := make([]byte, 192)
	for i := range rev {
		dwp[i] = rev[191-i]
	}
	want := sha256.Sum256(append(append([]byte{}, tempKey...), dwp...))
	if !bytes.Equal(want[:], dwh[192:]) {
		return nil, errors.New("fakeserver: RSA_PAD: хэш не сошёлся")
	}
	return dwp, nil
}

func (c *conn) sendDHParams() error {
	for {
		a, _ := rand.Int(rand.Reader, new(big.Int).Lsh(big.NewInt(1), 2048))
		ga := new(big.Int).Exp(big.NewInt(3), a, dhPrime)
		low := new(big.Int).Lsh(big.NewInt(1), 2048-64)
		if ga.Cmp(low) >= 0 && ga.Cmp(new(big.Int).Sub(dhPrime, low)) <= 0 {
			c.a = a
			answer, _ := tlbin.Marshal(&tl.ServerDHInnerData{Nonce: c.nonce, ServerNonce: c.serverNonce, G: 3,
				DhPrime: string(dhPrime.FillBytes(make([]byte, 256))), GA: string(ga.FillBytes(make([]byte, 256))),
				ServerTime: int32(time.Now().Unix())})
			h := sha1.Sum(answer)
			awh := append(h[:], answer...)
			pad := make([]byte, (16-len(awh)%16)%16)
			_, _ = rand.Read(pad)
			awh = append(awh, pad...)
			key, iv := tmpKey(c.newNonce[:], c.serverNonce[:])
			return c.sendPlain(&tl.ServerDHParamsOk{Nonce: c.nonce, ServerNonce: c.serverNonce,
				EncryptedAnswer: string(ige(key, iv, awh, true))})
		}
	}
}

// ---------------------------------------------------------------- зашифрованная часть

func envKeys(authKey, msgKey []byte, x int) (key, iv []byte) {
	a := sha256.Sum256(append(append([]byte{}, msgKey...), authKey[x:x+36]...))
	b := sha256.Sum256(append(append([]byte{}, authKey[40+x:40+x+36]...), msgKey...))
	key = append(append(append([]byte{}, a[:8]...), b[8:24]...), a[24:32]...)
	iv = append(append(append([]byte{}, b[:8]...), a[8:24]...), b[24:32]...)
	return
}

type msg struct {
	id   int64
	seq  int32
	body []byte
}

func (c *conn) encrypted(pkt []byte) error {
	if len(pkt) < 24+32 {
		return errors.New("fakeserver: короткий конверт")
	}
	keyID := binary.LittleEndian.Uint64(pkt)
	c.s.mu.Lock()
	authKey := c.s.keys[keyID]
	c.s.mu.Unlock()
	if authKey == nil {
		c.wmu.Lock()
		defer c.wmu.Unlock()
		return c.sendPacket(binary.LittleEndian.AppendUint32(nil, uint32(0xfffffe6c))) // -404
	}
	c.key = authKey
	c.keyID = keyID
	msgKey := pkt[8:24]
	k, iv := envKeys(authKey, msgKey, 0)
	pt := ige(k, iv, pkt[24:], false)
	want := sha256.Sum256(append(append([]byte{}, authKey[88:88+32]...), pt...))
	if !bytes.Equal(want[8:24], msgKey) {
		return errors.New("fakeserver: msg_key клиента не сошёлся")
	}
	salt := int64(binary.LittleEndian.Uint64(pt))
	c.sessionID = int64(binary.LittleEndian.Uint64(pt[8:]))
	id := int64(binary.LittleEndian.Uint64(pt[16:]))
	seq := int32(binary.LittleEndian.Uint32(pt[24:]))
	n := binary.LittleEndian.Uint32(pt[28:])
	if id%4 != 0 {
		return errors.New("fakeserver: msg_id клиента не кратен 4")
	}
	if c.salt == 0 {
		c.salt = salt // первая соль клиента (из обмена ключами)
	}
	return c.dispatch(salt, msg{id, seq, pt[32 : 32+n]})
}

func (c *conn) dispatch(salt int64, m msg) error {
	d := tlbin.NewDecoder(m.body)
	id, _ := d.PeekID()
	if id == 0x73f1f8dc { // контейнер
		_, _ = d.ID()
		n, err := d.VectorHeader(false, 16)
		if err != nil {
			return err
		}
		var inner []msg
		for i := 0; i < n; i++ {
			mid, _ := d.Int64()
			mseq, _ := d.Int32()
			ln, _ := d.Int32()
			b, err := d.Raw(int(ln))
			if err != nil {
				return err
			}
			inner = append(inner, msg{mid, mseq, b})
		}
		for _, im := range inner {
			if err := c.dispatch(salt, im); err != nil {
				return err
			}
		}
		return nil
	}
	o, err := tl.DecodeObject(tlbin.NewDecoder(m.body))
	if err != nil {
		return err
	}
	if _, ok := o.(*tl.MsgsAck); ok {
		return nil
	}
	if m.seq%2 == 0 {
		return errors.New("fakeserver: запрос с чётным seq_no")
	}
	// Неприятности первого запроса.
	if !c.firstDone {
		c.firstDone = true
		if c.s.opt.TimeSkewFirst {
			return c.send(false, &tl.BadMsgNotification{BadMsgId: m.id, BadMsgSeqno: m.seq, ErrorCode: 16})
		}
		if c.s.opt.BadSaltFirst {
			c.salt ^= 0x5555
			return c.send(false, &tl.BadServerSalt{BadMsgId: m.id, BadMsgSeqno: m.seq, ErrorCode: 48,
				NewServerSalt: c.salt})
		}
	}
	if salt != c.salt {
		return c.send(false, &tl.BadServerSalt{BadMsgId: m.id, BadMsgSeqno: m.seq, ErrorCode: 48, NewServerSalt: c.salt})
	}
	return c.answer(m.id, o)
}

// answer отвечает на запрос (обёртки раскрываются).
func (c *conn) answer(reqID int64, o tlbin.Object) error {
	for {
		switch w := o.(type) {
		case *tl.InvokeWithLayerRequest:
			o = w.Query
			continue
		case *tl.InitConnectionRequest:
			c.inited = true
			o = w.Query
			continue
		}
		break
	}
	name := o.TLName()
	c.s.mu.Lock()
	c.s.calls[name]++
	calls := c.s.calls[name]
	c.s.mu.Unlock()

	if dc, ok := c.s.opt.Migrate[name]; ok {
		return c.rpcError(reqID, 303, fmt.Sprintf("USER_MIGRATE_%d", dc))
	}
	if sec, ok := c.s.opt.FloodOnce[name]; ok && calls == 1 {
		return c.rpcError(reqID, 420, fmt.Sprintf("FLOOD_WAIT_%d", sec))
	}
	var res tlbin.Object
	switch r := o.(type) {
	case *tl.PingRequest:
		c.answered++
		return c.send(true, &tl.Pong{MsgId: reqID, PingId: r.PingId})
	case *tl.PingDelayDisconnectRequest:
		c.answered++
		return c.send(true, &tl.Pong{MsgId: reqID, PingId: r.PingId})
	case *tl.HelpGetConfigRequest:
		if !c.inited {
			return c.rpcError(reqID, 400, "CONNECTION_NOT_INITED")
		}
		res = &tl.Config{Date: int32(time.Now().Unix()), Expires: int32(time.Now().Unix() + 3600), ThisDc: int32(c.s.opt.DC),
			DcOptions:   []tl.DcOptionClass{&tl.DcOption{Id: int32(c.s.opt.DC), IpAddress: "127.0.0.1", Port: 443}},
			MeUrlPrefix: "https://t.me/", ChatSizeMax: 200, MegagroupSizeMax: 200000}
	case *tl.HelpGetNearestDcRequest:
		res = &tl.NearestDc{Country: "RU", ThisDc: int32(c.s.opt.DC), NearestDc: int32(c.s.opt.DC)}
	default:
		if c.s.opt.Account != nil {
			r, err := c.accountAnswer(o)
			if err != nil {
				var re *rpcErr
				if errors.As(err, &re) {
					return c.rpcError(reqID, re.code, re.msg)
				}
				return err
			}
			return c.rpcResult(reqID, r)
		}
		return c.rpcError(reqID, 400, "METHOD_NOT_IMPLEMENTED_IN_FAKESERVER")
	}
	return c.rpcResult(reqID, res)
}

func (c *conn) rpcError(reqID int64, code int32, text string) error {
	return c.rpcResult(reqID, &tl.RpcError{ErrorCode: code, ErrorMessage: text})
}

func (c *conn) rpcResult(reqID int64, res tlbin.Object) error {
	body, err := tlbin.Marshal(res)
	if err != nil {
		return err
	}
	if c.s.opt.GzipResults {
		var buf bytes.Buffer
		zw := gzip.NewWriter(&buf)
		_, _ = zw.Write(body)
		_ = zw.Close()
		var e tlbin.Encoder
		e.PutID(0x3072cfa1)
		e.PutBytes(buf.Bytes())
		body = e.Bytes()
	}
	var e tlbin.Encoder
	e.PutID(0xf35c6d01)
	e.PutInt64(reqID)
	e.PutRaw(body)
	c.answered++
	if c.s.opt.NewSession && c.answered == 1 {
		ns, _ := tlbin.Marshal(&tl.NewSessionCreated{FirstMsgId: reqID, UniqueId: 1, ServerSalt: c.salt})
		if err := c.sendMsgs(true, [][]byte{ns, e.Bytes()}); err != nil {
			return err
		}
	} else if err := c.sendMsgs(true, [][]byte{e.Bytes()}); err != nil {
		return err
	}
	if c.s.opt.PushUnknown && c.answered == 1 {
		var u tlbin.Encoder
		u.PutID(0xe317af7e) // updatesTooLong — нет в кодеке В2, должно уйти в OnUpdate
		return c.sendMsgs(true, [][]byte{u.Bytes()})
	}
	return nil
}

func (c *conn) send(content bool, o tlbin.Object) error {
	body, err := tlbin.Marshal(o)
	if err != nil {
		return err
	}
	return c.sendMsgs(content, [][]byte{body})
}

// sendMsgs отправляет одно сообщение или контейнер (x = 8).
func (c *conn) sendMsgs(content bool, bodies [][]byte) error {
	c.wmu.Lock()
	defer c.wmu.Unlock()
	nextSeq := func(content bool) int32 {
		s := c.seq * 2
		if content {
			s++
			c.seq++
		}
		return s
	}
	var body []byte
	var id int64
	var seq int32
	if len(bodies) == 1 {
		body = bodies[0]
		id, seq = c.msgID(false), nextSeq(content)
	} else {
		var e tlbin.Encoder
		e.PutID(0x73f1f8dc)
		e.PutInt32(int32(len(bodies)))
		for _, b := range bodies {
			e.PutInt64(c.msgID(false))
			e.PutInt32(nextSeq(true))
			e.PutInt32(int32(len(b)))
			e.PutRaw(b)
		}
		body = e.Bytes()
		id, seq = c.msgID(false), nextSeq(false)
	}
	var pt bytes.Buffer
	_ = binary.Write(&pt, binary.LittleEndian, c.salt)
	_ = binary.Write(&pt, binary.LittleEndian, c.sessionID)
	_ = binary.Write(&pt, binary.LittleEndian, id)
	_ = binary.Write(&pt, binary.LittleEndian, seq)
	_ = binary.Write(&pt, binary.LittleEndian, int32(len(body)))
	pt.Write(body)
	padLen := 12 + (16-(pt.Len()+12)%16)%16
	pad := make([]byte, padLen)
	_, _ = rand.Read(pad)
	pt.Write(pad)
	plain := pt.Bytes()
	large := sha256.Sum256(append(append([]byte{}, c.key[88+8:88+8+32]...), plain...))
	msgKey := large[8:24]
	k, iv := envKeys(c.key, msgKey, 8)
	kh := sha1.Sum(c.key)
	out := append(append(append([]byte{}, kh[12:20]...), msgKey...), ige(k, iv, plain, true)...)
	return c.sendPacket(out)
}
