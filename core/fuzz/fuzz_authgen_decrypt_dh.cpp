// Расшифровка server_DH_params_ok.encrypted_answer.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    static const uint8_t nn[32] = {1, 2, 3}, sn[16] = {4, 5, 6};
    tg_authgen *g = nullptr;
    if (tg_authgen_new(nn, sn, &g) != TG_OK) return 0;
    tg_buf inner{};
    if (tg_authgen_decrypt_dh(g, d, n, &inner) == TG_OK) {
        if (inner.len + 20 > n) __builtin_trap();
        tg_buf_free(inner);
    }
    tg_authgen_free(g);
    return 0;
}
