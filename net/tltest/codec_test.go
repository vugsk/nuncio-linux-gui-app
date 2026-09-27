// Package tltest — тесты сгенерированного кодека net/tl (сам net/tl генерируется
// и не коммитится, поэтому тесты живут здесь).
package tltest

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"math/big"
	"math/rand"
	"os"
	"reflect"
	"testing"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

type sample map[string]any

func loadSample(t *testing.T) sample {
	t.Helper()
	data, err := os.ReadFile("../testdata/auth_key_sample.json")
	if err != nil {
		t.Fatal(err)
	}
	var s sample
	if err := json.Unmarshal(data, &s); err != nil {
		t.Fatal(err)
	}
	return s
}

func (s sample) bytes(t *testing.T, k string) []byte {
	t.Helper()
	b, err := hex.DecodeString(s[k].(string))
	if err != nil {
		t.Fatalf("%s: %v", k, err)
	}
	return b
}

// body выделяет тело открытого сообщения: auth_key_id(8) msg_id(8) len(4) body.
func body(t *testing.T, msg []byte) []byte {
	t.Helper()
	if len(msg) < 20 || binary.LittleEndian.Uint64(msg) != 0 {
		t.Fatal("не открытое сообщение")
	}
	n := binary.LittleEndian.Uint32(msg[16:])
	if int(n) != len(msg)-20 {
		t.Fatalf("длина тела %d, а данных %d", n, len(msg)-20)
	}
	return msg[20:]
}

// roundtrip разбирает data как объект и проверяет, что кодирование обратно даёт те же байты.
func roundtrip(t *testing.T, name string, data []byte) tlbin.Object {
	t.Helper()
	d := tlbin.NewDecoder(data)
	o, err := tl.DecodeObject(d)
	if err != nil {
		t.Fatalf("%s: разбор: %v", name, err)
	}
	if d.Remaining() != 0 {
		t.Fatalf("%s: остались непрочитанные %d байт", name, d.Remaining())
	}
	back, err := tlbin.Marshal(o)
	if err != nil {
		t.Fatalf("%s: кодирование: %v", name, err)
	}
	if !bytes.Equal(back, data) {
		t.Fatalf("%s: кодирование не совпало с образцом\n  было  %x\n  стало %x", name, data, back)
	}
	return o
}

func TestOfficialAuthKeySample(t *testing.T) {
	s := loadSample(t)

	// 1. req_pq_multi: собираем сами и сравниваем с образцом.
	reqBody := body(t, s.bytes(t, "req_pq_multi_msg"))
	var nonce [16]byte
	copy(nonce[:], reqBody[4:])
	got, err := tlbin.Marshal(&tl.ReqPqMultiRequest{Nonce: nonce})
	if err != nil || !bytes.Equal(got, reqBody) {
		t.Fatalf("req_pq_multi: %x, ожидалось %x (%v)", got, reqBody, err)
	}

	// 2. resPQ.
	res := roundtrip(t, "resPQ", body(t, s.bytes(t, "res_pq_msg"))).(*tl.ResPQ)
	if res.Nonce != nonce {
		t.Fatal("resPQ: nonce не совпал")
	}
	pq := new(big.Int).SetBytes([]byte(res.Pq))
	if pq.Uint64() != 3358800871349344843 { // из примера; в JSON число теряет точность float64
		t.Fatalf("pq = %v", pq)
	}
	// В примере сервер прислал три отпечатка; первый — боевой ключ, его и выбрал клиент.
	if len(res.ServerPublicKeyFingerprints) != 3 || uint64(res.ServerPublicKeyFingerprints[0]) != 0xd09d1d85de64fd85 {
		t.Fatalf("отпечатки: %x", res.ServerPublicKeyFingerprints)
	}

	// 4. p_q_inner_data_dc: собираем из значений и сравниваем с образцом.
	pqi := roundtrip(t, "p_q_inner_data_dc", s.bytes(t, "p_q_inner_data_dc")).(*tl.PQInnerDataDc)
	if pqi.Dc != 2 || pqi.ServerNonce != res.ServerNonce {
		t.Fatalf("p_q_inner_data_dc: dc %d", pqi.Dc)
	}
	p := big.NewInt(int64(s["p"].(float64)))
	q := big.NewInt(int64(s["q"].(float64)))
	built, _ := tlbin.Marshal(&tl.PQInnerDataDc{Pq: res.Pq, P: string(p.Bytes()), Q: string(q.Bytes()), Nonce: res.Nonce,
		ServerNonce: res.ServerNonce, NewNonce: pqi.NewNonce, Dc: 2})
	if !bytes.Equal(built, s.bytes(t, "p_q_inner_data_dc")) {
		t.Fatal("p_q_inner_data_dc: собранное не совпало с образцом")
	}

	// 5–9. Остальные сообщения — разбор и обратное кодирование.
	req := roundtrip(t, "req_DH_params", body(t, s.bytes(t, "req_dh_params_msg"))).(*tl.ReqDHParamsRequest)
	if req.PublicKeyFingerprint != int64(-0x2f62e27a219b027b) || len(req.EncryptedData) != 256 {
		t.Fatalf("req_DH_params: отпечаток %x, данных %d", uint64(req.PublicKeyFingerprint), len(req.EncryptedData))
	}
	ok := roundtrip(t, "server_DH_params_ok", body(t, s.bytes(t, "server_dh_params_ok_msg"))).(*tl.ServerDHParamsOk)
	if !bytes.Equal([]byte(ok.EncryptedAnswer), s.bytes(t, "encrypted_answer")) {
		t.Fatal("server_DH_params_ok: encrypted_answer")
	}
	inner := roundtrip(t, "server_DH_inner_data", s.bytes(t, "answer")[:564]).(*tl.ServerDHInnerData)
	if inner.G != 3 || len(inner.DhPrime) != 256 || len(inner.GA) != 256 || inner.ServerTime != 1783001185 {
		t.Fatalf("server_DH_inner_data: g=%d time=%d", inner.G, inner.ServerTime)
	}
	cdi := roundtrip(t, "client_DH_inner_data", s.bytes(t, "client_dh_inner_data")).(*tl.ClientDHInnerData)
	if !bytes.Equal([]byte(cdi.GB), s.bytes(t, "g_b")) {
		t.Fatal("client_DH_inner_data: g_b")
	}
	roundtrip(t, "set_client_DH_params", body(t, s.bytes(t, "set_client_dh_params_msg")))
	gen := roundtrip(t, "dh_gen_ok", body(t, s.bytes(t, "dh_gen_ok_msg"))).(*tl.DhGenOk)
	if hex.EncodeToString(gen.NewNonceHash1[:]) != "aa404b58df404d8f363772b14ce5a56f" {
		t.Fatal("dh_gen_ok: new_nonce_hash1")
	}
}

// ---------------------------------------------------------------- случайные объекты

type filler struct {
	r     *rand.Rand
	depth int
}

func (f *filler) value(v reflect.Value) {
	switch v.Kind() {
	case reflect.Bool:
		v.SetBool(f.r.Intn(2) == 0)
	case reflect.Int32:
		v.SetInt(int64(f.r.Int31()) - 1<<30)
	case reflect.Int64:
		v.SetInt(f.r.Int63() - 1<<62)
	case reflect.Float64:
		v.SetFloat(f.r.NormFloat64())
	case reflect.String:
		b := make([]byte, f.r.Intn(300))
		f.r.Read(b)
		v.SetString(string(b))
	case reflect.Array:
		for i := 0; i < v.Len(); i++ {
			v.Index(i).SetUint(uint64(f.r.Intn(256)))
		}
	case reflect.Slice:
		if v.Type().Elem().Kind() == reflect.Uint8 {
			b := make([]byte, f.r.Intn(600))
			f.r.Read(b)
			v.SetBytes(b)
			return
		}
		n := 0
		if f.depth < 4 {
			n = f.r.Intn(3)
		}
		s := reflect.MakeSlice(v.Type(), n, n)
		for i := 0; i < n; i++ {
			f.value(s.Index(i))
		}
		v.Set(s)
	case reflect.Ptr:
		if f.r.Intn(2) == 0 {
			return // необязательное поле отсутствует
		}
		p := reflect.New(v.Type().Elem())
		f.value(p.Elem())
		v.Set(p)
	case reflect.Interface:
		ids := tl.ClassConstructors[v.Type().Name()]
		if len(ids) == 0 || f.depth > 5 {
			return
		}
		o := tl.NewObject(ids[f.r.Intn(len(ids))])
		f.depth++
		f.value(reflect.ValueOf(o).Elem())
		f.depth--
		v.Set(reflect.ValueOf(o))
	case reflect.Struct:
		for i := 0; i < v.NumField(); i++ {
			f.value(v.Field(i))
		}
	}
}

// Все сгенерированные конструкторы: случайное значение -> байты -> разбор -> те же байты.
func TestRandomRoundtripAllConstructors(t *testing.T) {
	r := rand.New(rand.NewSource(1))
	checked := 0
	for class, ids := range tl.ClassConstructors {
		for _, id := range ids {
			for i := 0; i < 50; i++ {
				o := tl.NewObject(id)
				f := &filler{r: r}
				f.value(reflect.ValueOf(o).Elem())
				data, err := tlbin.Marshal(o)
				if err != nil {
					// Обязательный интерфейс мог остаться пустым на глубине — пропускаем.
					continue
				}
				d := tlbin.NewDecoder(data)
				back, err := tl.DecodeObject(d)
				if err != nil || d.Remaining() != 0 {
					t.Fatalf("%s %s: разбор: %v, остаток %d", class, o.TLName(), err, d.Remaining())
				}
				again, _ := tlbin.Marshal(back)
				if !bytes.Equal(again, data) {
					t.Fatalf("%s %s: повторное кодирование не совпало", class, o.TLName())
				}
				checked++
			}
		}
	}
	if checked < 1000 {
		t.Fatalf("проверено всего %d объектов", checked)
	}
	t.Logf("проверено %d случайных объектов", checked)
}

func TestDecodeRejectsGarbage(t *testing.T) {
	var e tlbin.Encoder
	e.PutID(0xdeadbeef)
	_, err := tl.DecodeObject(tlbin.NewDecoder(e.Bytes()))
	var unk *tlbin.UnknownConstructorError
	if !errors.As(err, &unk) || unk.ID != 0xdeadbeef {
		t.Fatalf("неизвестный конструктор: %v", err)
	}
	// Усечённый resPQ.
	full, _ := tlbin.Marshal(&tl.ResPQ{Pq: "\x01\x02\x03", ServerPublicKeyFingerprints: []int64{1, 2}})
	for n := 0; n < len(full); n++ {
		if _, err := tl.DecodeObject(tlbin.NewDecoder(full[:n])); err == nil {
			t.Fatalf("усечённые до %d байт данные разобраны без ошибки", n)
		}
	}
}

// Разбор произвольных байт: никаких паник, зависаний и огромных выделений.
func FuzzDecodeObject(f *testing.F) {
	s := map[string]string{}
	data, _ := os.ReadFile("../testdata/auth_key_sample.json")
	var raw map[string]any
	_ = json.Unmarshal(data, &raw)
	for k, v := range raw {
		if str, ok := v.(string); ok {
			s[k] = str
		}
	}
	for _, k := range []string{"res_pq_msg", "server_dh_params_ok_msg", "dh_gen_ok_msg", "req_dh_params_msg"} {
		b, _ := hex.DecodeString(s[k])
		if len(b) > 20 {
			f.Add(b[20:])
		}
	}
	cfg, _ := tlbin.Marshal(&tl.Config{DcOptions: []tl.DcOptionClass{&tl.DcOption{Id: 2, IpAddress: "1.2.3.4", Port: 443}}})
	f.Add(cfg)
	f.Fuzz(func(t *testing.T, b []byte) {
		d := tlbin.NewDecoder(b)
		o, err := tl.DecodeObject(d)
		if err != nil {
			return
		}
		// Разобранное должно кодироваться обратно без ошибок.
		if _, err := tlbin.Marshal(o); err != nil {
			t.Fatalf("разобранный %s не кодируется: %v", o.TLName(), err)
		}
	})
}
