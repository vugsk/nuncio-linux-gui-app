package core

import (
	"bytes"
	"crypto/sha1"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"math/big"
	"strings"
	"sync"
	"testing"
)

// dh_prime из core.telegram.org/mtproto/auth_key; g = 3 (p mod 3 = 2).
const dhPrimeHex = "c71caeb9c6b1c9048e6c522f70f13f73980d40238e3e21c14934d037563d930f48198a0aa7c1405822" +
	"9493d22530f4dbfa336f6e0ac925139543aed44cce7c3720fd51f69458705ac68cd4fe6b6b13abdc9746512969328454f18" +
	"faf8c595f642477fe96bb2a941d5bcd1d4ac8cc49880708fa9b378e3c4f3a9060bee67cf9a4a4a695811051907e162753b56" +
	"b0f6b410dba74d8a84b2a14b3144e0ef1284754fd17ed950d5965b4b9dd46582db1178d169c6bc465b0d6ff9ca3928fef5b9a" +
	"e4e418fc15e83ebea0f87fa9ff5eed70050ded2849f47bf959d956850ce929851f0d8115f635b105ee2e4e15d04b2454bf6f4f" +
	"adf034b10403119cd8e3b92fcc5b"

func dhPrime(t *testing.T) []byte {
	p, err := hex.DecodeString(dhPrimeHex)
	if err != nil || len(p) != 256 {
		t.Fatalf("dh_prime: %v, %d байт", err, len(p))
	}
	return p
}

func random(t *testing.T, n int) []byte {
	b := make([]byte, n)
	if err := Random(b); err != nil {
		t.Fatal(err)
	}
	return b
}

func pad256(x *big.Int) []byte { return x.FillBytes(make([]byte, 256)) }

func TestVersion(t *testing.T) {
	if v := Version(); !strings.HasPrefix(v, "tgcrypto ") {
		t.Fatalf("версия ядра: %q", v)
	}
}

func TestSHA256(t *testing.T) {
	got := SHA256([]byte("abc"))
	if hex.EncodeToString(got[:]) != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" {
		t.Fatal("SHA256(abc)")
	}
}

// Полный обмен ключами через Go-обёртки; сторона сервера — math/big (только в тесте).
func exchange(t *testing.T) (*Key, []byte) {
	t.Helper()
	p := dhPrime(t)
	var nn [32]byte
	var sn [16]byte
	copy(nn[:], random(t, 32))
	copy(sn[:], random(t, 16))
	ag, err := NewAuthGen(&nn, &sn)
	if err != nil {
		t.Fatal(err)
	}
	defer ag.Free()

	answer := random(t, 4*150)
	enc, err := ag.EncryptInner(answer)
	if err != nil {
		t.Fatal(err)
	}
	back, err := ag.DecryptAnswer(enc)
	if err != nil || !bytes.Equal(back, answer) {
		t.Fatalf("DecryptAnswer: %v", err)
	}

	P := new(big.Int).SetBytes(p)
	a := new(big.Int).SetBytes(random(t, 256))
	gA := new(big.Int).Exp(big.NewInt(3), a, P)
	gB, err := ag.DH(p, 3, pad256(gA))
	if err != nil {
		t.Fatal(err)
	}
	serverKey := pad256(new(big.Int).Exp(new(big.Int).SetBytes(gB), a, P))
	if _, err := ag.NonceHash(1); err != nil {
		t.Fatal(err)
	}
	key, err := ag.Finish()
	if err != nil {
		t.Fatal(err)
	}
	return key, serverKey
}

func TestAuthKeyExchange(t *testing.T) {
	key, serverKey := exchange(t)
	defer key.Free()
	// ID ключа клиента совпадает с ID ключа, вычисленного «сервером».
	sum := sha1Low64(serverKey)
	if key.ID() != sum {
		t.Fatalf("auth_key_id: клиент %x, сервер %x", key.ID(), sum)
	}
}

// sha1Low64 — auth_key_id, посчитанный независимо от ядра (эталон только в тесте;
// в рабочем коде сети криптографии Go нет).
func sha1Low64(b []byte) uint64 {
	h := sha1.Sum(b)
	return binary.LittleEndian.Uint64(h[12:20])
}

func TestDHRejectsBadParams(t *testing.T) {
	p := dhPrime(t)
	var nn [32]byte
	var sn [16]byte
	ag, err := NewAuthGen(&nn, &sn)
	if err != nil {
		t.Fatal(err)
	}
	defer ag.Free()
	one := make([]byte, 256)
	one[255] = 1
	if _, err := ag.DH(p, 3, one); !errors.Is(err, ErrDH) {
		t.Fatalf("g_a = 1: %v", err)
	}
	if _, err := ag.DH(p, 2, one); !errors.Is(err, ErrDH) { // g = 2 не подходит к этому p
		t.Fatalf("g = 2: %v", err)
	}
	if _, err := ag.Finish(); !errors.Is(err, ErrState) {
		t.Fatalf("Finish до DH: %v", err)
	}
}

func TestSealUnseal(t *testing.T) {
	key, _ := exchange(t)
	defer key.Free()
	var wrap, other [32]byte
	copy(wrap[:], random(t, 32))
	copy(other[:], random(t, 32))
	sealed, err := key.Seal(&wrap)
	if err != nil || len(sealed) != 288 {
		t.Fatalf("Seal: %v, %d", err, len(sealed))
	}
	back, err := Unseal(sealed, &wrap)
	if err != nil || back.ID() != key.ID() {
		t.Fatalf("Unseal: %v", err)
	}
	back.Free()
	back.Free() // повторный Free безопасен
	if _, err := Unseal(sealed, &other); !errors.Is(err, ErrCrypto) {
		t.Fatalf("чужой ключ обёртки: %v", err)
	}
}

func TestEnvelope(t *testing.T) {
	key, _ := exchange(t)
	defer key.Free()
	h := EnvHeader{Salt: 1, SessionID: 2, MsgID: 3 << 2, SeqNo: 1}
	wire, err := key.Encrypt(h, random(t, 64))
	if err != nil {
		t.Fatal(err)
	}
	if (len(wire)-24)%16 != 0 || len(wire) < 24+32+64+12 {
		t.Fatalf("длина конверта %d", len(wire))
	}
	// Своё исходящее сообщение не открывается как входящее (x = 0 против x = 8).
	if _, _, err := key.Decrypt(wire); !errors.Is(err, ErrMsgKey) {
		t.Fatalf("Decrypt своего: %v", err)
	}
	if _, err := key.Encrypt(h, []byte{1, 2, 3}); !errors.Is(err, ErrArg) {
		t.Fatalf("тело не кратно 4: %v", err)
	}
}

func TestConcurrentKeyUse(t *testing.T) {
	key, _ := exchange(t)
	defer key.Free()
	var wg sync.WaitGroup
	for i := 0; i < 16; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < 50; j++ {
				if _, err := key.Encrypt(EnvHeader{MsgID: uint64(j)}, make([]byte, 128)); err != nil {
					t.Error(err)
					return
				}
			}
		}()
	}
	wg.Wait()
}

func TestPQAndRSA(t *testing.T) {
	p, q, err := FactorizePQ(0x17ED48941A08F981)
	if err != nil || p != 0x494C553B || q != 0x53911073 {
		t.Fatalf("pq: %x %x %v", p, q, err)
	}
	if !HasRSAKey(0xd09d1d85de64fd85) || HasRSAKey(1) {
		t.Fatal("HasRSAKey")
	}
	enc, err := RSAPad(random(t, 100), 0xd09d1d85de64fd85)
	if err != nil || len(enc) != 256 {
		t.Fatalf("RSAPad: %v %d", err, len(enc))
	}
	if _, err := RSAPad(random(t, 145), 0xd09d1d85de64fd85); !errors.Is(err, ErrArg) {
		t.Fatalf("RSAPad 145: %v", err)
	}
}

func TestSRP(t *testing.T) {
	p := dhPrime(t)
	P := new(big.Int).SetBytes(p)
	gb := pad256(new(big.Int).Exp(big.NewInt(3), new(big.Int).SetBytes(random(t, 256)), P))
	A, M1, err := SRPCheck([]byte("пароль"), SRPParams{Salt1: random(t, 40), Salt2: random(t, 16), G: 3, P: p, SRPB: gb})
	if err != nil || len(A) != 256 || len(M1) != 32 {
		t.Fatalf("SRPCheck: %v %d %d", err, len(A), len(M1))
	}
}

func TestObfs(t *testing.T) {
	for _, tr := range []int{TransportAbridged, TransportIntermediate, TransportPadded} {
		o, init, err := NewObfs(tr, 2, nil)
		if err != nil || len(init) != 64 || init[0] == 0xef {
			t.Fatalf("NewObfs: %v", err)
		}
		msg := random(t, 1000)
		b := append([]byte(nil), msg...)
		o.Encrypt(b)
		if bytes.Equal(b, msg) {
			t.Fatal("Encrypt не изменил данные")
		}
		o.Free()
		o.Free()
	}
	if _, _, err := NewObfs(9, 0, nil); !errors.Is(err, ErrArg) {
		t.Fatalf("неверный транспорт: %v", err)
	}
}
