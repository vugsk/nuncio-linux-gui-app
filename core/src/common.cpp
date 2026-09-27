// Общие функции ядра: версия и освобождение буферов.
// См. Архитектура/nuncio - Границы и C ABI.md (стык 2).

#include "tgcrypto.h"

#include <cstdlib>
#include <cstring>

#ifndef TGCRYPTO_VERSION
#error "TGCRYPTO_VERSION задаётся в CMakeLists.txt"
#endif

extern "C" const char *tg_version(void) {
    return "tgcrypto " TGCRYPTO_VERSION;
}

extern "C" void tg_buf_free(tg_buf b) {
    if (b.ptr == nullptr) {
        return;
    }
    // Буфер может содержать ключевой материал: стираем перед освобождением.
    explicit_bzero(b.ptr, b.len);
    std::free(b.ptr);
}
