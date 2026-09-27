package session

import (
	"context"
	"crypto/subtle"
	"encoding/binary"
	"errors"
	"fmt"
	"math/big"
	"time"

	"nuncio/net/core"
	"nuncio/net/tl"
	"nuncio/net/tlbin"
	"nuncio/net/transport"
)

// AuthKey — результат обмена ключами.
type AuthKey struct {
	Key        *core.Key
	Salt       int64         // первая соль: new_nonce[0:8] XOR server_nonce[0:8]
	TimeOffset time.Duration // время сервера минус местное (для msg_id)
}

// ErrAuthKey — обмен ключами прерван: ответ сервера не прошёл проверку.
var ErrAuthKey = errors.New("session: обмен ключами: ответ сервера не прошёл проверку")

// plain — открытые сообщения (auth_key_id = 0) для обмена ключами.
type plain struct {
	conn   *transport.Conn
	lastID int64
	offset time.Duration
}

func (p *plain) send(o tlbin.Object) error {
	body, err := tlbin.Marshal(o)
	if err != nil {
		return err
	}
	id := nextMsgID(time.Now().Add(p.offset), p.lastID)
	p.lastID = id
	pkt := make([]byte, 20, 20+len(body))
	binary.LittleEndian.PutUint64(pkt[8:], uint64(id))
	binary.LittleEndian.PutUint32(pkt[16:], uint32(len(body)))
	return p.conn.Send(append(pkt, body...))
}

func (p *plain) recv(ctx context.Context) (tlbin.Object, error) {
	if dl, ok := ctx.Deadline(); ok {
		_ = p.conn.SetReadDeadline(dl)
		defer p.conn.SetReadDeadline(time.Time{})
	}
	pkt, err := p.conn.Recv()
	if err != nil {
		return nil, err
	}
	if len(pkt) < 20 || binary.LittleEndian.Uint64(pkt) != 0 {
		return nil, fmt.Errorf("%w: не открытое сообщение", ErrAuthKey)
	}
	n := binary.LittleEndian.Uint32(pkt[16:])
	if int(n) != len(pkt)-20 {
		return nil, fmt.Errorf("%w: длина тела", ErrAuthKey)
	}
	d := tlbin.NewDecoder(pkt[20:])
	o, err := tl.DecodeObject(d)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrAuthKey, err)
	}
	return o, nil
}

func random(n int) []byte {
	b := make([]byte, n)
	if err := core.Random(b); err != nil {
		panic(err) // отказ системного генератора — продолжать нельзя
	}
	return b
}

// CreateAuthKey выполняет обмен ключами (core.telegram.org/mtproto/auth_key)
// по открытому соединению. dc — номер DC для p_q_inner_data_dc (+10000 для
// тестовых, отрицательный для медиа). Криптография — в ядре, здесь только TL
// и проверки соответствия nonce.
func CreateAuthKey(ctx context.Context, conn *transport.Conn, dc int) (*AuthKey, error) {
	p := &plain{conn: conn}

	// 1–2. req_pq_multi -> resPQ.
	var nonce [16]byte
	copy(nonce[:], random(16))
	if err := p.send(&tl.ReqPqMultiRequest{Nonce: nonce}); err != nil {
		return nil, err
	}
	o, err := p.recv(ctx)
	if err != nil {
		return nil, err
	}
	res, ok := o.(*tl.ResPQ)
	if !ok || res.Nonce != nonce {
		return nil, fmt.Errorf("%w: resPQ", ErrAuthKey)
	}
	var fp uint64
	for _, f := range res.ServerPublicKeyFingerprints {
		if core.HasRSAKey(uint64(f)) {
			fp = uint64(f)
			break
		}
	}
	if fp == 0 {
		return nil, fmt.Errorf("%w: сервер не предложил известного ключа RSA", ErrAuthKey)
	}

	// 3. pq -> p, q.
	if len(res.Pq) > 8 {
		return nil, fmt.Errorf("%w: pq длиннее 64 бит", ErrAuthKey)
	}
	pqInt := new(big.Int).SetBytes([]byte(res.Pq)).Uint64()
	pp, qq, err := core.FactorizePQ(pqInt)
	if err != nil {
		return nil, fmt.Errorf("%w: pq: %v", ErrAuthKey, err)
	}

	// 4–5. p_q_inner_data_dc под RSA_PAD -> req_DH_params.
	var newNonce [32]byte
	copy(newNonce[:], random(32))
	inner, err := tlbin.Marshal(&tl.PQInnerDataDc{
		Pq: res.Pq, P: string(big.NewInt(int64(pp)).Bytes()), Q: string(big.NewInt(int64(qq)).Bytes()),
		Nonce: nonce, ServerNonce: res.ServerNonce, NewNonce: newNonce, Dc: int32(dc),
	})
	if err != nil {
		return nil, err
	}
	enc, err := core.RSAPad(inner, fp)
	if err != nil {
		return nil, err
	}
	if err := p.send(&tl.ReqDHParamsRequest{Nonce: nonce, ServerNonce: res.ServerNonce,
		P: string(big.NewInt(int64(pp)).Bytes()), Q: string(big.NewInt(int64(qq)).Bytes()),
		PublicKeyFingerprint: int64(fp), EncryptedData: string(enc)}); err != nil {
		return nil, err
	}

	// 6. server_DH_params_ok -> server_DH_inner_data.
	if o, err = p.recv(ctx); err != nil {
		return nil, err
	}
	dhOK, ok := o.(*tl.ServerDHParamsOk)
	if !ok || dhOK.Nonce != nonce || dhOK.ServerNonce != res.ServerNonce {
		return nil, fmt.Errorf("%w: server_DH_params (%T)", ErrAuthKey, o)
	}
	ag, err := core.NewAuthGen(&newNonce, &res.ServerNonce)
	if err != nil {
		return nil, err
	}
	defer ag.Free()
	answer, err := ag.DecryptAnswer([]byte(dhOK.EncryptedAnswer))
	if err != nil {
		return nil, fmt.Errorf("%w: encrypted_answer: %v", ErrAuthKey, err)
	}
	io, err := tl.DecodeObject(tlbin.NewDecoder(answer))
	if err != nil {
		return nil, fmt.Errorf("%w: server_DH_inner_data: %v", ErrAuthKey, err)
	}
	sdh, ok := io.(*tl.ServerDHInnerData)
	if !ok || sdh.Nonce != nonce || sdh.ServerNonce != res.ServerNonce {
		return nil, fmt.Errorf("%w: server_DH_inner_data", ErrAuthKey)
	}
	offset := time.Until(time.Unix(int64(sdh.ServerTime), 0))
	p.offset = offset

	// 7–9. g_b -> set_client_DH_params -> dh_gen_ok / retry / fail.
	var retryID int64
	for attempt := 0; attempt < 5; attempt++ {
		gB, err := ag.DH([]byte(sdh.DhPrime), uint32(sdh.G), []byte(sdh.GA))
		if err != nil {
			return nil, fmt.Errorf("%w: параметры DH: %v", ErrAuthKey, err)
		}
		cdh, err := tlbin.Marshal(&tl.ClientDHInnerData{Nonce: nonce, ServerNonce: res.ServerNonce,
			RetryId: retryID, GB: string(gB)})
		if err != nil {
			return nil, err
		}
		encInner, err := ag.EncryptInner(cdh)
		if err != nil {
			return nil, err
		}
		if err := p.send(&tl.SetClientDHParamsRequest{Nonce: nonce, ServerNonce: res.ServerNonce,
			EncryptedData: string(encInner)}); err != nil {
			return nil, err
		}
		if o, err = p.recv(ctx); err != nil {
			return nil, err
		}
		check := func(n int, got [16]byte, gotNonce, gotServer [16]byte) error {
			want, err := ag.NonceHash(n)
			if err != nil {
				return err
			}
			if gotNonce != nonce || gotServer != res.ServerNonce || subtle.ConstantTimeCompare(got[:], want[:]) != 1 {
				return fmt.Errorf("%w: new_nonce_hash%d", ErrAuthKey, n)
			}
			return nil
		}
		switch a := o.(type) {
		case *tl.DhGenOk:
			if err := check(1, a.NewNonceHash1, a.Nonce, a.ServerNonce); err != nil {
				return nil, err
			}
			key, err := ag.Finish()
			if err != nil {
				return nil, err
			}
			salt := int64(binary.LittleEndian.Uint64(newNonce[:8]) ^ binary.LittleEndian.Uint64(res.ServerNonce[:8]))
			return &AuthKey{Key: key, Salt: salt, TimeOffset: offset}, nil
		case *tl.DhGenRetry:
			if err := check(2, a.NewNonceHash2, a.Nonce, a.ServerNonce); err != nil {
				return nil, err
			}
			aux, err := ag.AuxHash()
			if err != nil {
				return nil, err
			}
			retryID = int64(aux)
		case *tl.DhGenFail:
			if err := check(3, a.NewNonceHash3, a.Nonce, a.ServerNonce); err != nil {
				return nil, err
			}
			return nil, fmt.Errorf("%w: сервер ответил dh_gen_fail", ErrAuthKey)
		default:
			return nil, fmt.Errorf("%w: set_client_DH_params: %T", ErrAuthKey, o)
		}
	}
	return nil, fmt.Errorf("%w: слишком много dh_gen_retry", ErrAuthKey)
}
