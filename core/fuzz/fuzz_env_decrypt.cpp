// Полная расшифровка конверта (вход из сети); плюс круг «запечатать — открыть»
// для входа, трактуемого как тело.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    tg_env_hdr h{};
    tg_buf body{};
    if (tg_env_decrypt(fuzz_key(), d, n, &h, &body) == TG_OK) tg_buf_free(body);
    if (n % 4 == 0 && n <= 4096) {
        tg_env_hdr hh{1, 2, 3, 4};
        const uint8_t pad[28] = {};
        const size_t padlen = 12 + (16 - (32 + n + 12) % 16) % 16;
        tg_buf wire{};
        if (tg::env_seal(fuzz_key(), 8, &hh, d, n, pad, padlen, &wire) == TG_OK) {
            tg_buf out{};
            if (tg_env_decrypt(fuzz_key(), wire.ptr, wire.len, &h, &out) != TG_OK || out.len != n ||
                (n != 0 && std::memcmp(out.ptr, d, n) != 0))
                __builtin_trap();
            tg_buf_free(out);
            tg_buf_free(wire);
        }
    }
    return 0;
}
