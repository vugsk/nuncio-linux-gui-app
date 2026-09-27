// Package core — cgo-обёртка над C ABI крипто-ядра (core/include/tgcrypto.h).
// Ядро вызывает только Go (О-1); остальные пакеты net обращаются к нему
// только через этот пакет. Веха В0: только Version. Примитивы — веха В1.
//
// Сборка: сначала `just core` (кладёт libtgcrypto.a в target/native).
package core

/*
#cgo CFLAGS: -I${SRCDIR}/../../core/include
#cgo LDFLAGS: -L${SRCDIR}/../../target/native -ltgcrypto -lstdc++
#include "tgcrypto.h"
*/
import "C"

// Version возвращает строку версии ядра, например "tgcrypto 0.1.0".
func Version() string {
	return C.GoString(C.tg_version())
}
