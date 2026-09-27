// pq из resPQ: не должно быть зависаний и неверных множителей.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    if (n != 8) return 0;
    uint64_t pq = 0;
    for (int i = 0; i < 8; ++i) pq = (pq << 8) | d[i];
    uint32_t p = 0, q = 0;
    if (tg_pq_factorize(pq, &p, &q) == TG_OK) {
        if (static_cast<uint64_t>(p) * q != pq || p > q || p < 2) __builtin_trap();
    }
    return 0;
}
