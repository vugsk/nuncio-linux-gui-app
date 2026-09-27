// Замер скорости AES-256-IGE (сравнение с П-3: C++ AES-NI 1254 МБ/с, Go 529 МБ/с).
// Не входит в ctest: `just core-bench`.
#include "internal.h"

#include <chrono>
#include <cstdio>
#include <vector>

static double run(size_t size, int reps) {
    std::vector<uint8_t> key(32, 1), iv(32, 2), buf(size, 3);
    const auto t0 = std::chrono::steady_clock::now();
    for (int i = 0; i < reps; ++i) tg::aes_ige_encrypt(key.data(), 32, iv.data(), buf.data(), buf.data(), size);
    const double s = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    return static_cast<double>(size) * reps / s / 1e6;
}

int main() {
    for (bool fb : {false, true}) {
        tg::aes_ige_force_fallback(fb);
        std::printf("%s: 1 КБ %.0f МБ/с, 64 КБ %.0f МБ/с, 16 МБ %.0f МБ/с\n", fb ? "EVP поблочно" : "AES-NI",
                    run(1024, 200000), run(65536, 4000), run(16 << 20, 16));
    }
}
