// Файл сессии с диска: запечатанный ключ может быть испорчен.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    static const uint8_t wrap[32] = {7, 7, 7};
    tg_key *k = nullptr;
    if (tg_key_unseal(d, n, wrap, &k) == TG_OK) tg_key_free(k);
    return 0;
}
