// Package core — cgo-обёртка над C ABI крипто-ядра (core/include/tgcrypto.h).
// Ядро вызывает только Go (О-1); остальные пакеты net обращаются к нему только
// через этот пакет, собственной криптографии в Go нет (принцип 2 архитектуры).
//
// Правила cgo (Архитектура/nuncio - Границы и C ABI.md): в C передаются только
// указатели на данные байтовых срезов на время вызова; память, выданная ядром
// (tg_buf), копируется в Go и сразу освобождается tg_buf_free; хэндлы
// освобождаются явно методом Free (и финализатором как страховкой).
//
// Сборка: сначала `just core` (libtgcrypto.a) и `just deps openssl` (libcrypto.a).
package core

/*
#cgo CFLAGS: -I${SRCDIR}/../../core/include
#cgo LDFLAGS: -L${SRCDIR}/../../target/native -ltgcrypto -lcrypto -lstdc++ -lpthread -ldl
#include <stdlib.h>
#include "tgcrypto.h"
*/
import "C"

import (
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// Ошибки ядра (tg_status).
var (
	ErrArg    = errors.New("tgcrypto: неверные аргументы или формат")
	ErrNoMem  = errors.New("tgcrypto: нет памяти")
	ErrCrypto = errors.New("tgcrypto: ошибка криптографии или целостности")
	ErrMsgKey = errors.New("tgcrypto: не тот auth_key_id или msg_key")
	ErrDH     = errors.New("tgcrypto: параметры DH/SRP не прошли проверки")
	ErrState  = errors.New("tgcrypto: неверное состояние")
)

func status(st C.tg_status) error {
	switch st {
	case C.TG_OK:
		return nil
	case C.TG_ERR_ARG:
		return ErrArg
	case C.TG_ERR_NOMEM:
		return ErrNoMem
	case C.TG_ERR_CRYPTO:
		return ErrCrypto
	case C.TG_ERR_MSG_KEY:
		return ErrMsgKey
	case C.TG_ERR_DH:
		return ErrDH
	case C.TG_ERR_STATE:
		return ErrState
	default:
		return fmt.Errorf("tgcrypto: неизвестный код %d", int(st))
	}
}

// ptr возвращает указатель на данные среза для передачи в C на время вызова.
func ptr(b []byte) *C.uint8_t {
	if len(b) == 0 {
		return nil
	}
	return (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(b)))
}

// take копирует буфер ядра в Go и освобождает его (со стиранием).
func take(b C.tg_buf) []byte {
	if b.ptr == nil {
		return nil
	}
	out := C.GoBytes(unsafe.Pointer(b.ptr), C.int(b.len))
	C.tg_buf_free(b)
	return out
}

// Version возвращает строку версии ядра, например "tgcrypto 0.2.0".
func Version() string { return C.GoString(C.tg_version()) }

// SHA256 — хэш через ядро.
func SHA256(data []byte) [32]byte {
	var out [32]byte
	C.tg_sha256(ptr(data), C.size_t(len(data)), (*C.uint8_t)(unsafe.Pointer(&out[0])))
	return out
}

// Random заполняет b криптостойкими случайными байтами.
func Random(b []byte) error {
	return status(C.tg_random(ptr(b), C.size_t(len(b))))
}

// ---------------------------------------------------------------- ключ

// Key — ключ авторизации; байты в Go не попадают. Неизменяем, безопасен для
// одновременного использования из нескольких горутин.
type Key struct {
	mu sync.Mutex
	k  *C.tg_key
}

func newKey(k *C.tg_key) *Key {
	key := &Key{k: k}
	runtime.SetFinalizer(key, (*Key).Free)
	return key
}

// Free освобождает ключ (со стиранием); повторный вызов безопасен.
func (k *Key) Free() {
	k.mu.Lock()
	defer k.mu.Unlock()
	if k.k != nil {
		C.tg_key_free(k.k)
		k.k = nil
	}
}

func (k *Key) handle() *C.tg_key {
	if k == nil {
		return nil
	}
	return k.k
}

// ID — auth_key_id.
func (k *Key) ID() uint64 { return uint64(C.tg_key_id(k.handle())) }

// Seal запечатывает ключ для файла сессии ключом обёртки из Secret Service.
func (k *Key) Seal(wrap *[32]byte) ([]byte, error) {
	var out C.tg_buf
	st := C.tg_key_seal(k.handle(), (*C.uint8_t)(unsafe.Pointer(&wrap[0])), &out)
	runtime.KeepAlive(k)
	if err := status(st); err != nil {
		return nil, err
	}
	return take(out), nil
}

// Unseal восстанавливает ключ из запечатанного вида.
func Unseal(sealed []byte, wrap *[32]byte) (*Key, error) {
	var k *C.tg_key
	st := C.tg_key_unseal(ptr(sealed), C.size_t(len(sealed)), (*C.uint8_t)(unsafe.Pointer(&wrap[0])), &k)
	if err := status(st); err != nil {
		return nil, err
	}
	return newKey(k), nil
}

// ---------------------------------------------------------------- конверт

// EnvHeader — поля открытого текста конверта MTProto 2.0.
type EnvHeader struct {
	Salt, SessionID, MsgID uint64
	SeqNo                  uint32
}

// Encrypt запечатывает тело (TL, длина кратна 4) в конверт клиент -> сервер.
func (k *Key) Encrypt(h EnvHeader, body []byte) ([]byte, error) {
	ch := C.tg_env_hdr{salt: C.uint64_t(h.Salt), session_id: C.uint64_t(h.SessionID), msg_id: C.uint64_t(h.MsgID),
		seq_no: C.uint32_t(h.SeqNo)}
	var out C.tg_buf
	st := C.tg_env_encrypt(k.handle(), &ch, ptr(body), C.size_t(len(body)), &out)
	runtime.KeepAlive(k)
	if err := status(st); err != nil {
		return nil, err
	}
	return take(out), nil
}

// Decrypt открывает конверт сервер -> клиент. Проверки session_id и msg_id —
// дело вызывающего (пакет session).
func (k *Key) Decrypt(wire []byte) (EnvHeader, []byte, error) {
	var ch C.tg_env_hdr
	var out C.tg_buf
	st := C.tg_env_decrypt(k.handle(), ptr(wire), C.size_t(len(wire)), &ch, &out)
	runtime.KeepAlive(k)
	if err := status(st); err != nil {
		return EnvHeader{}, nil, err
	}
	return EnvHeader{uint64(ch.salt), uint64(ch.session_id), uint64(ch.msg_id), uint32(ch.seq_no)}, take(out), nil
}

// ---------------------------------------------------------------- обмен ключами

// FactorizePQ раскладывает pq из resPQ на p < q.
func FactorizePQ(pq uint64) (p, q uint32, err error) {
	var cp, cq C.uint32_t
	if err := status(C.tg_pq_factorize(C.uint64_t(pq), &cp, &cq)); err != nil {
		return 0, 0, err
	}
	return uint32(cp), uint32(cq), nil
}

// HasRSAKey — знает ли ядро открытый ключ сервера с таким отпечатком.
func HasRSAKey(fingerprint uint64) bool { return C.tg_rsa_has_key(C.uint64_t(fingerprint)) == 1 }

// RSAPad шифрует p_q_inner_data (не длиннее 144 байт) по RSA_PAD.
func RSAPad(data []byte, fingerprint uint64) ([]byte, error) {
	var out C.tg_buf
	if err := status(C.tg_rsa_pad_encrypt(ptr(data), C.size_t(len(data)), C.uint64_t(fingerprint), &out)); err != nil {
		return nil, err
	}
	return take(out), nil
}

// AuthGen — состояние обмена ключами (один владелец).
type AuthGen struct {
	g *C.tg_authgen
}

// NewAuthGen выводит временный ключ из new_nonce и server_nonce.
func NewAuthGen(newNonce *[32]byte, serverNonce *[16]byte) (*AuthGen, error) {
	var g *C.tg_authgen
	st := C.tg_authgen_new((*C.uint8_t)(unsafe.Pointer(&newNonce[0])), (*C.uint8_t)(unsafe.Pointer(&serverNonce[0])), &g)
	if err := status(st); err != nil {
		return nil, err
	}
	a := &AuthGen{g: g}
	runtime.SetFinalizer(a, (*AuthGen).Free)
	return a, nil
}

// Free освобождает состояние (со стиранием); повторный вызов безопасен.
func (a *AuthGen) Free() {
	if a.g != nil {
		C.tg_authgen_free(a.g)
		a.g = nil
	}
}

// DecryptAnswer расшифровывает encrypted_answer и возвращает server_DH_inner_data.
func (a *AuthGen) DecryptAnswer(enc []byte) ([]byte, error) {
	var out C.tg_buf
	if err := status(C.tg_authgen_decrypt_dh(a.g, ptr(enc), C.size_t(len(enc)), &out)); err != nil {
		return nil, err
	}
	return take(out), nil
}

// EncryptInner шифрует client_DH_inner_data.
func (a *AuthGen) EncryptInner(inner []byte) ([]byte, error) {
	var out C.tg_buf
	if err := status(C.tg_authgen_encrypt_dh(a.g, ptr(inner), C.size_t(len(inner)), &out)); err != nil {
		return nil, err
	}
	return take(out), nil
}

// DH проверяет dh_prime, g, g_a и возвращает g_b (256 байт); auth_key остаётся в ядре.
func (a *AuthGen) DH(dhPrime []byte, g uint32, gA []byte) ([]byte, error) {
	var out C.tg_buf
	st := C.tg_authgen_dh(a.g, ptr(dhPrime), C.size_t(len(dhPrime)), C.uint32_t(g), ptr(gA), C.size_t(len(gA)), &out)
	if err := status(st); err != nil {
		return nil, err
	}
	return take(out), nil
}

// NonceHash — new_nonce_hash{n}, n = 1 (ok), 2 (retry), 3 (fail).
func (a *AuthGen) NonceHash(n int) ([16]byte, error) {
	var out [16]byte
	err := status(C.tg_authgen_nonce_hash(a.g, C.int(n), (*C.uint8_t)(unsafe.Pointer(&out[0]))))
	return out, err
}

// AuxHash — auth_key_aux_hash, используется как retry_id.
func (a *AuthGen) AuxHash() (uint64, error) {
	var v C.uint64_t
	err := status(C.tg_authgen_aux_hash(a.g, &v))
	return uint64(v), err
}

// Finish отдаёт готовый ключ; после этого AuthGen годится только для Free.
func (a *AuthGen) Finish() (*Key, error) {
	var k *C.tg_key
	if err := status(C.tg_authgen_finish(a.g, &k)); err != nil {
		return nil, err
	}
	return newKey(k), nil
}

// ---------------------------------------------------------------- SRP

// SRPParams — поля account.password (алгоритм ...SHA256ModPow).
type SRPParams struct {
	Salt1, Salt2 []byte
	G            uint32
	P            []byte
	SRPB         []byte
}

// SRPCheck вычисляет A и M1 для inputCheckPasswordSRP.
func SRPCheck(password []byte, p SRPParams) (A, M1 []byte, err error) {
	var a, m C.tg_buf
	st := C.tg_srp_check(ptr(password), C.size_t(len(password)), ptr(p.Salt1), C.size_t(len(p.Salt1)),
		ptr(p.Salt2), C.size_t(len(p.Salt2)), C.uint32_t(p.G), ptr(p.P), C.size_t(len(p.P)),
		ptr(p.SRPB), C.size_t(len(p.SRPB)), &a, &m)
	if err := status(st); err != nil {
		return nil, nil, err
	}
	return take(a), take(m), nil
}

// ---------------------------------------------------------------- обфускация

// Транспорты для obfuscated2.
const (
	TransportAbridged     = C.TG_TRANSPORT_ABRIDGED
	TransportIntermediate = C.TG_TRANSPORT_INTERMEDIATE
	TransportPadded       = C.TG_TRANSPORT_PADDED
)

// Obfs — состояние обфускации соединения. Encrypt вызывает только писатель,
// Decrypt — только читатель; одновременно они не мешают друг другу.
type Obfs struct {
	o *C.tg_obfs
}

// NewObfs возвращает состояние и 64 байта init для отправки первыми.
func NewObfs(transport, dcID int, secret []byte) (*Obfs, []byte, error) {
	var o *C.tg_obfs
	var init C.tg_buf
	st := C.tg_obfs_new(C.int(transport), C.int(dcID), ptr(secret), C.size_t(len(secret)), &init, &o)
	if err := status(st); err != nil {
		return nil, nil, err
	}
	ob := &Obfs{o: o}
	runtime.SetFinalizer(ob, (*Obfs).Free)
	return ob, take(init), nil
}

// Encrypt шифрует исходящие байты на месте.
func (o *Obfs) Encrypt(b []byte) {
	C.tg_obfs_encrypt(o.o, ptr(b), C.size_t(len(b)))
	runtime.KeepAlive(b)
}

// Decrypt расшифровывает входящие байты на месте.
func (o *Obfs) Decrypt(b []byte) {
	C.tg_obfs_decrypt(o.o, ptr(b), C.size_t(len(b)))
	runtime.KeepAlive(b)
}

// Free освобождает состояние; повторный вызов безопасен.
func (o *Obfs) Free() {
	if o.o != nil {
		C.tg_obfs_free(o.o)
		o.o = nil
	}
}
