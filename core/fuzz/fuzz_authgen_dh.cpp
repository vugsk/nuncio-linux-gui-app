// Проверки параметров DH и вычисление g_b: первый байт — g и выбор простого
// (встроенное или из входа), затем p (если из входа) и g_a.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    if (n < 2) return 0;
    const uint32_t gen = d[0] & 7;
    const bool own_p = (d[0] & 0x80) != 0;
    const uint8_t *p = tg::dh_known_prime();
    size_t off = 1;
    if (own_p) {
        if (n < 1 + 256 + 1) return 0;
        p = d + 1;
        off += 256;
    }
    static const uint8_t nn[32] = {9}, sn[16] = {8};
    tg_authgen *g = nullptr;
    if (tg_authgen_new(nn, sn, &g) != TG_OK) return 0;
    static const uint8_t b[256] = {0x42, 0x42, 0x42};
    tg_buf gb{};
    if (tg::authgen_dh_with_b(g, p, 256, gen, d + off, n - off, b, &gb) == TG_OK) {
        if (gb.len != 256) __builtin_trap();
        tg_buf_free(gb);
        uint8_t h[16];
        if (tg_authgen_nonce_hash(g, 1, h) != TG_OK) __builtin_trap();
    }
    tg_authgen_free(g);
    return 0;
}
