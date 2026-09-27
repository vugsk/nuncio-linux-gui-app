// SRP: параметры из account.password (соли, g, g_b) — вход из сети. Одна итерация
// PBKDF2, чтобы фаззер успевал; сама PBKDF2 — код OpenSSL.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    if (n < 4) return 0;
    const uint32_t gen = d[0] & 7;
    const size_t s1 = d[1] % 64, s2 = d[2] % 64;
    if (n < 3 + s1 + s2) return 0;
    const uint8_t *salt1 = d + 3, *salt2 = d + 3 + s1, *gb = d + 3 + s1 + s2;
    const size_t blen = n - 3 - s1 - s2;
    static const uint8_t a[256] = {0x13, 0x37};
    static const uint8_t pw[] = {'p', 'w'};
    tg_buf A{}, M{};
    if (tg::srp_check_with_a(pw, 2, salt1, s1, salt2, s2, gen, tg::dh_known_prime(), 256, gb, blen, a, 1, &A, &M) ==
        TG_OK) {
        if (A.len != 256 || M.len != 32) __builtin_trap();
        tg_buf_free(A);
        tg_buf_free(M);
    }
    return 0;
}
