// Веха В0: проверяет публичный минимум ABI (tg_version, tg_buf_free).
// Настоящие векторные тесты и фаззеры — веха В1.
#include "tgcrypto.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>

static int failures = 0;

#define CHECK(cond)                                                         \
    do {                                                                    \
        if (!(cond)) {                                                      \
            std::fprintf(stderr, "%s:%d: CHECK(%s) не выполнено\n",         \
                         __FILE__, __LINE__, #cond);                        \
            ++failures;                                                     \
        }                                                                   \
    } while (0)

int main() {
    const char *v = tg_version();
    CHECK(v != nullptr);
    CHECK(std::strncmp(v, "tgcrypto ", 9) == 0);
    CHECK(std::strlen(v) > 9);

    // {NULL, 0} допустим.
    tg_buf_free(tg_buf{nullptr, 0});

    // Буфер, выделенный malloc (так же выдаёт ядро), освобождается без ошибок
    // (ошибку поймают ASan/valgrind на вехе В1).
    tg_buf b{static_cast<uint8_t *>(std::malloc(32)), 32};
    CHECK(b.ptr != nullptr);
    std::memset(b.ptr, 0xAB, b.len);
    tg_buf_free(b);

    if (failures == 0) {
        std::printf("ok: %s\n", v);
    }
    return failures == 0 ? 0 : 1;
}
