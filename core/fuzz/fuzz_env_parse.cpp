// Разбор открытого текста конверта после проверки msg_key.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    tg_env_hdr h{};
    size_t off = 0, len = 0;
    if (tg::env_parse_plaintext(d, n, &h, &off, &len) == TG_OK) {
        if (off + len > n) __builtin_trap();  // тело обязано лежать внутри буфера
    }
    return 0;
}
