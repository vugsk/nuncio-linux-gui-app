// Package tlbin — двоичная сериализация TL (core.telegram.org/mtproto/serialize):
// примитивы кодирования и разбора, на которые опирается сгенерированный
// кодек net/tl. Пишется вручную и коммитится; net/tl генерирует tools/tlgen.
//
// Разбор недоверенного ввода: каждая длина проверяется до выделения памяти,
// ошибка возвращается значением, паники нет (Архитектура/nuncio - Сетевой слой (Go).md).
package tlbin

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math"
)

// Идентификаторы встроенных конструкторов.
const (
	IDVector    uint32 = 0x1cb5c415
	IDBoolTrue  uint32 = 0x997275b5
	IDBoolFalse uint32 = 0xbc799737
)

// Ошибки разбора.
var (
	ErrShort     = errors.New("tl: данные кончились")
	ErrTooLarge  = errors.New("tl: заявленная длина больше данных")
	ErrNotVector = errors.New("tl: ожидался вектор")
	ErrBadBool   = errors.New("tl: ожидался Bool")
	ErrTooDeep   = errors.New("tl: слишком глубокая вложенность объектов")
)

// MaxDepth — предел вложенности объектов при разборе (защита стека).
const MaxDepth = 64

// UnknownConstructorError — конструктор, которого нет в сгенерированном кодеке
// (новый слой API или ошибка). Ошибка кодека не роняет процесс (Р-4).
type UnknownConstructorError struct {
	ID   uint32
	Type string
}

func (e *UnknownConstructorError) Error() string {
	return fmt.Sprintf("tl: неизвестный конструктор %#08x для %s", e.ID, e.Type)
}

// Object — любой TL-объект (конструктор или функция).
type Object interface {
	TLID() uint32
	TLName() string
	TLEncodeBare(e *Encoder) error
	TLDecodeBare(d *Decoder) error
}

// Request — функция TL: умеет разобрать свой результат.
type Request interface {
	Object
	DecodeResult(d *Decoder) (any, error)
}

// EncodeBoxed пишет идентификатор и тело объекта.
func EncodeBoxed(e *Encoder, o Object) error {
	if o == nil {
		return errors.New("tl: пустое значение там, где объект обязателен")
	}
	e.PutID(o.TLID())
	return o.TLEncodeBare(e)
}

// Marshal кодирует объект с идентификатором.
func Marshal(o Object) ([]byte, error) {
	var e Encoder
	if err := EncodeBoxed(&e, o); err != nil {
		return nil, err
	}
	return e.Bytes(), nil
}

// ---------------------------------------------------------------- Encoder

// Encoder накапливает байты в порядке little-endian.
type Encoder struct {
	buf []byte
}

// Bytes возвращает накопленное (без копирования).
func (e *Encoder) Bytes() []byte { return e.buf }

// Len — число записанных байт.
func (e *Encoder) Len() int { return len(e.buf) }

func (e *Encoder) PutID(id uint32)    { e.buf = binary.LittleEndian.AppendUint32(e.buf, id) }
func (e *Encoder) PutUint32(v uint32) { e.buf = binary.LittleEndian.AppendUint32(e.buf, v) }
func (e *Encoder) PutInt32(v int32)   { e.PutUint32(uint32(v)) }
func (e *Encoder) PutInt64(v int64)   { e.buf = binary.LittleEndian.AppendUint64(e.buf, uint64(v)) }
func (e *Encoder) PutDouble(v float64) {
	e.buf = binary.LittleEndian.AppendUint64(e.buf, math.Float64bits(v))
}
func (e *Encoder) PutInt128(v [16]byte) { e.buf = append(e.buf, v[:]...) }
func (e *Encoder) PutInt256(v [32]byte) { e.buf = append(e.buf, v[:]...) }
func (e *Encoder) PutRaw(b []byte)      { e.buf = append(e.buf, b...) }
func (e *Encoder) PutString(s string)   { e.PutBytes([]byte(s)) }

func (e *Encoder) PutBool(v bool) {
	if v {
		e.PutID(IDBoolTrue)
	} else {
		e.PutID(IDBoolFalse)
	}
}

// PutBytes пишет TL bytes: длина (1 или 4 байта), данные, дополнение до 4.
func (e *Encoder) PutBytes(b []byte) {
	n := len(b)
	var head int
	if n < 254 {
		e.buf = append(e.buf, byte(n))
		head = 1
	} else {
		e.buf = append(e.buf, 254, byte(n), byte(n>>8), byte(n>>16))
		head = 4
	}
	e.buf = append(e.buf, b...)
	for i := (head + n) % 4; i != 0 && i < 4; i++ {
		e.buf = append(e.buf, 0)
	}
}

// PutVectorHeader пишет заголовок вектора (boxed — с идентификатором).
func (e *Encoder) PutVectorHeader(n int, boxed bool) {
	if boxed {
		e.PutID(IDVector)
	}
	e.PutInt32(int32(n))
}

// ---------------------------------------------------------------- Decoder

// Decoder читает из среза с проверкой границ; ошибки возвращаются значением.
type Decoder struct {
	buf   []byte
	off   int
	depth int
}

// Enter отмечает вход во вложенный объект; Leave — выход.
func (d *Decoder) Enter() error {
	d.depth++
	if d.depth > MaxDepth {
		return ErrTooDeep
	}
	return nil
}

func (d *Decoder) Leave() { d.depth-- }

// NewDecoder читает b (не копирует).
func NewDecoder(b []byte) *Decoder { return &Decoder{buf: b} }

// Remaining — сколько байт осталось.
func (d *Decoder) Remaining() int { return len(d.buf) - d.off }

// Raw возвращает следующие n байт (без копирования) и сдвигает позицию.
func (d *Decoder) Raw(n int) ([]byte, error) { return d.take(n) }

// Rest возвращает непрочитанный остаток и сдвигает позицию в конец.
func (d *Decoder) Rest() []byte {
	r := d.buf[d.off:]
	d.off = len(d.buf)
	return r
}

func (d *Decoder) take(n int) ([]byte, error) {
	if n < 0 || n > d.Remaining() {
		return nil, ErrShort
	}
	b := d.buf[d.off : d.off+n]
	d.off += n
	return b, nil
}

func (d *Decoder) Uint32() (uint32, error) {
	b, err := d.take(4)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint32(b), nil
}

func (d *Decoder) ID() (uint32, error) { return d.Uint32() }

// PeekID возвращает следующий идентификатор, не сдвигая позицию.
func (d *Decoder) PeekID() (uint32, error) {
	if d.Remaining() < 4 {
		return 0, ErrShort
	}
	return binary.LittleEndian.Uint32(d.buf[d.off:]), nil
}

func (d *Decoder) Int32() (int32, error) {
	v, err := d.Uint32()
	return int32(v), err
}

func (d *Decoder) Int64() (int64, error) {
	b, err := d.take(8)
	if err != nil {
		return 0, err
	}
	return int64(binary.LittleEndian.Uint64(b)), nil
}

func (d *Decoder) Double() (float64, error) {
	v, err := d.Int64()
	return math.Float64frombits(uint64(v)), err
}

func (d *Decoder) Int128() (v [16]byte, err error) {
	b, err := d.take(16)
	if err == nil {
		copy(v[:], b)
	}
	return v, err
}

func (d *Decoder) Int256() (v [32]byte, err error) {
	b, err := d.take(32)
	if err == nil {
		copy(v[:], b)
	}
	return v, err
}

func (d *Decoder) Bool() (bool, error) {
	id, err := d.ID()
	if err != nil {
		return false, err
	}
	switch id {
	case IDBoolTrue:
		return true, nil
	case IDBoolFalse:
		return false, nil
	}
	return false, ErrBadBool
}

// Bytes читает TL bytes (копию, чтобы результат не держал весь буфер).
func (d *Decoder) Bytes() ([]byte, error) {
	first, err := d.take(1)
	if err != nil {
		return nil, err
	}
	n, head := int(first[0]), 1
	if n == 254 {
		l, err := d.take(3)
		if err != nil {
			return nil, err
		}
		n, head = int(l[0])|int(l[1])<<8|int(l[2])<<16, 4
	} else if n == 255 {
		return nil, ErrTooLarge
	}
	if n > d.Remaining() {
		return nil, ErrTooLarge
	}
	data, _ := d.take(n)
	pad := (4 - (head+n)%4) % 4
	if _, err := d.take(pad); err != nil {
		return nil, err
	}
	out := make([]byte, n)
	copy(out, data)
	return out, nil
}

func (d *Decoder) String() (string, error) {
	b, err := d.Bytes()
	return string(b), err
}

// VectorHeader читает заголовок вектора и проверяет, что элементов не больше,
// чем может поместиться в остатке (каждый элемент — минимум minElem байт).
func (d *Decoder) VectorHeader(boxed bool, minElem int) (int, error) {
	if boxed {
		id, err := d.ID()
		if err != nil {
			return 0, err
		}
		if id != IDVector {
			return 0, ErrNotVector
		}
	}
	n, err := d.Int32()
	if err != nil {
		return 0, err
	}
	if minElem < 1 {
		minElem = 1
	}
	if n < 0 || int(n) > d.Remaining()/minElem {
		return 0, ErrTooLarge
	}
	return int(n), nil
}
