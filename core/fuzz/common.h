// Общие помощники фаззеров.
#ifndef NUNCIO_TGCRYPTO_FUZZ_COMMON_H
#define NUNCIO_TGCRYPTO_FUZZ_COMMON_H
#include "internal.h"

#include <cstddef>
#include <cstdint>
#include <vector>

// Фиксированный auth_key для целей, где нужен ключ (детерминированность запусков).
inline tg_key *fuzz_key() {
    static tg_key *k = [] {
        uint8_t ak[256];
        for (int i = 0; i < 256; ++i) ak[i] = static_cast<uint8_t>(i * 7 + 3);
        return tg::key_from_bytes(ak);
    }();
    return k;
}

#endif  // NUNCIO_TGCRYPTO_FUZZ_COMMON_H
