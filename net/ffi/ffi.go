// Command ffi — экспорт сетевого слоя для Rust: собирается с
// -buildmode=c-archive в target/native/libnuncio_net.a (+ .h).
// Пять функций nn_* по эскизу из Архитектура/nuncio - Границы и C ABI.md.
// Обратных вызовов нет: события Rust забирает блокирующим nn_next_event.
// В каждой экспортируемой функции стоит recover: паника Go не должна
// пересечь границу.
package main

/*
#include <stdint.h>
#include <stdlib.h>

typedef struct { uint8_t *ptr; size_t len; } nn_buf;

// Коды возврата (зеркалируются в bridge/src/ffi.rs).
#define NN_OK            0
#define NN_TIMEOUT       1
#define NN_STOPPED       2
#define NN_ERR_ARG      -1
#define NN_ERR_STATE    -2
#define NN_ERR_VERSION  -3
#define NN_ERR_INTERNAL -4
#define NN_ERR_FULL     -5
*/
import "C"

import (
	"errors"
	"math"
	"time"
	"unsafe"

	"nuncio/net/api"
)

var service = api.New()

func goBytes(p *C.uint8_t, n C.size_t) ([]byte, bool) {
	if n == 0 {
		return nil, true
	}
	if p == nil || uint64(n) > math.MaxInt32 {
		return nil, false
	}
	return C.GoBytes(unsafe.Pointer(p), C.int(n)), true
}

//export nn_start
func nn_start(req *C.uint8_t, length C.size_t) (rc C.int32_t) {
	defer func() {
		if recover() != nil {
			rc = C.NN_ERR_INTERNAL
		}
	}()
	data, ok := goBytes(req, length)
	if !ok {
		return C.NN_ERR_ARG
	}
	switch err := service.Start(data); {
	case err == nil:
		return C.NN_OK
	case errors.Is(err, api.ErrAlreadyStarted):
		return C.NN_ERR_STATE
	case errors.Is(err, api.ErrBadVersion):
		return C.NN_ERR_VERSION
	default:
		return C.NN_ERR_ARG
	}
}

//export nn_submit
func nn_submit(cmdID C.uint64_t, cmd *C.uint8_t, length C.size_t) (rc C.int32_t) {
	defer func() {
		if recover() != nil {
			rc = C.NN_ERR_INTERNAL
		}
	}()
	data, ok := goBytes(cmd, length)
	if !ok {
		return C.NN_ERR_ARG
	}
	switch err := service.Submit(uint64(cmdID), data); {
	case err == nil:
		return C.NN_OK
	case errors.Is(err, api.ErrQueueFull):
		return C.NN_ERR_FULL
	case errors.Is(err, api.ErrNotStarted), errors.Is(err, api.ErrStopped):
		return C.NN_ERR_STATE
	default:
		return C.NN_ERR_ARG
	}
}

//export nn_next_event
func nn_next_event(timeoutMs C.uint32_t, out *C.nn_buf) (rc C.int32_t) {
	defer func() {
		if recover() != nil {
			rc = C.NN_ERR_INTERNAL
		}
	}()
	if out == nil {
		return C.NN_ERR_ARG
	}
	out.ptr, out.len = nil, 0
	data, res := service.NextEvent(time.Duration(timeoutMs) * time.Millisecond)
	switch res {
	case api.Timeout:
		return C.NN_TIMEOUT
	case api.Stopped:
		return C.NN_STOPPED
	}
	if len(data) > 0 {
		p := (*C.uint8_t)(C.malloc(C.size_t(len(data))))
		if p == nil {
			return C.NN_ERR_INTERNAL
		}
		copy(unsafe.Slice((*byte)(unsafe.Pointer(p)), len(data)), data)
		out.ptr, out.len = p, C.size_t(len(data))
	}
	return C.NN_OK
}

//export nn_free
func nn_free(buf C.nn_buf) {
	if buf.ptr != nil {
		C.free(unsafe.Pointer(buf.ptr))
	}
}

//export nn_shutdown
func nn_shutdown(timeoutMs C.uint32_t) (rc C.int32_t) {
	defer func() {
		if recover() != nil {
			rc = C.NN_ERR_INTERNAL
		}
	}()
	if err := service.Shutdown(time.Duration(timeoutMs) * time.Millisecond); err != nil {
		return C.NN_ERR_INTERNAL
	}
	return C.NN_OK
}

func main() {}
