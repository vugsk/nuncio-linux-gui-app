// Проверки параметров Диффи-Хеллмана (auth_key, п. 6 и «IMPORTANT»; то же для SRP):
//  - p — безопасное 2048-битное простое: p и (p-1)/2 простые, 2^2047 < p < 2^2048;
//  - g порождает подгруппу порядка (p-1)/2: по закону взаимности условие на p mod 4g
//    (g=2: p mod 8 = 7; g=3: p mod 3 = 2; g=4: нет; g=5: p mod 5 in {1,4};
//     g=6: p mod 24 in {19,23}; g=7: p mod 7 in {3,5,6});
//  - 1 < g, g_a, g_b < p-1, рекомендуется 2^{2048-64} <= g_a, g_b <= p - 2^{2048-64}.
// Известное простое из документации не перепроверяется (как советует спецификация),
// прошедшие проверку простые кэшируются.
#include "internal.h"

#include <memory>
#include <mutex>
#include <set>
#include <string>

#include <openssl/bn.h>

namespace tg {
namespace {

// dh_prime из core.telegram.org/mtproto/auth_key (п. 6), big-endian.
const uint8_t kKnownPrime[256] = {
    0xC7, 0x1C, 0xAE, 0xB9, 0xC6, 0xB1, 0xC9, 0x04, 0x8E, 0x6C, 0x52, 0x2F, 0x70, 0xF1, 0x3F, 0x73,
    0x98, 0x0D, 0x40, 0x23, 0x8E, 0x3E, 0x21, 0xC1, 0x49, 0x34, 0xD0, 0x37, 0x56, 0x3D, 0x93, 0x0F,
    0x48, 0x19, 0x8A, 0x0A, 0xA7, 0xC1, 0x40, 0x58, 0x22, 0x94, 0x93, 0xD2, 0x25, 0x30, 0xF4, 0xDB,
    0xFA, 0x33, 0x6F, 0x6E, 0x0A, 0xC9, 0x25, 0x13, 0x95, 0x43, 0xAE, 0xD4, 0x4C, 0xCE, 0x7C, 0x37,
    0x20, 0xFD, 0x51, 0xF6, 0x94, 0x58, 0x70, 0x5A, 0xC6, 0x8C, 0xD4, 0xFE, 0x6B, 0x6B, 0x13, 0xAB,
    0xDC, 0x97, 0x46, 0x51, 0x29, 0x69, 0x32, 0x84, 0x54, 0xF1, 0x8F, 0xAF, 0x8C, 0x59, 0x5F, 0x64,
    0x24, 0x77, 0xFE, 0x96, 0xBB, 0x2A, 0x94, 0x1D, 0x5B, 0xCD, 0x1D, 0x4A, 0xC8, 0xCC, 0x49, 0x88,
    0x07, 0x08, 0xFA, 0x9B, 0x37, 0x8E, 0x3C, 0x4F, 0x3A, 0x90, 0x60, 0xBE, 0xE6, 0x7C, 0xF9, 0xA4,
    0xA4, 0xA6, 0x95, 0x81, 0x10, 0x51, 0x90, 0x7E, 0x16, 0x27, 0x53, 0xB5, 0x6B, 0x0F, 0x6B, 0x41,
    0x0D, 0xBA, 0x74, 0xD8, 0xA8, 0x4B, 0x2A, 0x14, 0xB3, 0x14, 0x4E, 0x0E, 0xF1, 0x28, 0x47, 0x54,
    0xFD, 0x17, 0xED, 0x95, 0x0D, 0x59, 0x65, 0xB4, 0xB9, 0xDD, 0x46, 0x58, 0x2D, 0xB1, 0x17, 0x8D,
    0x16, 0x9C, 0x6B, 0xC4, 0x65, 0xB0, 0xD6, 0xFF, 0x9C, 0xA3, 0x92, 0x8F, 0xEF, 0x5B, 0x9A, 0xE4,
    0xE4, 0x18, 0xFC, 0x15, 0xE8, 0x3E, 0xBE, 0xA0, 0xF8, 0x7F, 0xA9, 0xFF, 0x5E, 0xED, 0x70, 0x05,
    0x0D, 0xED, 0x28, 0x49, 0xF4, 0x7B, 0xF9, 0x59, 0xD9, 0x56, 0x85, 0x0C, 0xE9, 0x29, 0x85, 0x1F,
    0x0D, 0x81, 0x15, 0xF6, 0x35, 0xB1, 0x05, 0xEE, 0x2E, 0x4E, 0x15, 0xD0, 0x4B, 0x24, 0x54, 0xBF,
    0x6F, 0x4F, 0xAD, 0xF0, 0x34, 0xB1, 0x04, 0x03, 0x11, 0x9C, 0xD8, 0xE3, 0xB9, 0x2F, 0xCC, 0x5B,
};

struct BnDel {
    void operator()(BIGNUM *b) const { BN_clear_free(b); }
};
struct CtxDel {
    void operator()(BN_CTX *c) const { BN_CTX_free(c); }
};
using Bn = std::unique_ptr<BIGNUM, BnDel>;
using Ctx = std::unique_ptr<BN_CTX, CtxDel>;

std::mutex g_cache_mu;
std::set<std::string> &verified_primes() {
    static std::set<std::string> s{std::string(reinterpret_cast<const char *>(kKnownPrime), 256)};
    return s;
}

bool g_ok_for(const BIGNUM *p, uint32_t g) {
    auto mod = [p](BN_ULONG m) { return BN_mod_word(p, m); };
    switch (g) {
        case 2: return mod(8) == 7;
        case 3: return mod(3) == 2;
        case 4: return true;
        case 5: { const auto r = mod(5); return r == 1 || r == 4; }
        case 6: { const auto r = mod(24); return r == 19 || r == 23; }
        case 7: { const auto r = mod(7); return r == 3 || r == 5 || r == 6; }
        default: return false;
    }
}

}  // namespace

const uint8_t *dh_known_prime() { return kKnownPrime; }

tg_status dh_check_params(const uint8_t *p, size_t plen, uint32_t g) {
    if (p == nullptr || plen != 256) return TG_ERR_DH;
    if ((p[0] & 0x80) == 0) return TG_ERR_DH;  // 2^2047 < p < 2^2048
    Bn bp(BN_bin2bn(p, 256, nullptr));
    if (!bp) return TG_ERR_NOMEM;
    if (!g_ok_for(bp.get(), g)) return TG_ERR_DH;

    const std::string key(reinterpret_cast<const char *>(p), 256);
    {
        std::lock_guard<std::mutex> lock(g_cache_mu);
        if (verified_primes().count(key) != 0) return TG_OK;
    }
    Ctx ctx(BN_CTX_new());
    Bn q(BN_dup(bp.get()));
    if (!ctx || !q) return TG_ERR_NOMEM;
    if (BN_sub_word(q.get(), 1) != 1 || BN_rshift1(q.get(), q.get()) != 1) return TG_ERR_CRYPTO;
    if (BN_check_prime(bp.get(), ctx.get(), nullptr) != 1) return TG_ERR_DH;
    if (BN_check_prime(q.get(), ctx.get(), nullptr) != 1) return TG_ERR_DH;
    std::lock_guard<std::mutex> lock(g_cache_mu);
    if (verified_primes().size() < 16) verified_primes().insert(key);
    return TG_OK;
}

bool dh_check_value(const uint8_t *x, size_t xlen, const uint8_t *p, size_t plen, bool strict) {
    if (x == nullptr || p == nullptr || xlen == 0 || xlen > 256 || plen != 256) return false;
    Bn bx(BN_bin2bn(x, static_cast<int>(xlen), nullptr));
    Bn bp(BN_bin2bn(p, 256, nullptr));
    Bn lim(BN_new());
    if (!bx || !bp || !lim) return false;
    // 1 < x < p - 1
    if (BN_cmp(bx.get(), BN_value_one()) <= 0) return false;
    Bn pm1(BN_dup(bp.get()));
    if (!pm1 || BN_sub_word(pm1.get(), 1) != 1) return false;
    if (BN_cmp(bx.get(), pm1.get()) >= 0) return false;
    if (!strict) return true;
    // 2^{2048-64} <= x <= p - 2^{2048-64}
    if (BN_set_bit(lim.get(), 2048 - 64) != 1) return false;
    if (BN_cmp(bx.get(), lim.get()) < 0) return false;
    Bn upper(BN_new());
    if (!upper || BN_sub(upper.get(), bp.get(), lim.get()) != 1) return false;
    return BN_cmp(bx.get(), upper.get()) <= 0;
}

}  // namespace tg
