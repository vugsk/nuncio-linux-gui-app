// RSA_PAD (core.telegram.org/mtproto/auth_key, п. 4.1) и разложение pq.
//
//   data_with_padding := data + random_padding   (ровно 192 байта, data <= 144)
//   data_pad_reversed := BYTE_REVERSE(data_with_padding)
//   temp_key          := 32 случайных байта
//   data_with_hash    := data_pad_reversed + SHA256(temp_key + data_with_padding)  (224)
//   aes_encrypted     := AES256_IGE(data_with_hash, temp_key, iv = 0)
//   temp_key_xor      := temp_key XOR SHA256(aes_encrypted)
//   key_aes_encrypted := temp_key_xor + aes_encrypted                              (256)
//   если key_aes_encrypted >= n — заново с новым temp_key
//   encrypted_data    := key_aes_encrypted ^ e mod n  (256 байт big-endian)
#include "internal.h"

#include <memory>

#include <openssl/bn.h>

namespace tg {
namespace {

struct RsaKey {
    uint64_t fingerprint;
    unsigned long e;
    uint8_t n[256];
};

const RsaKey kKeys[] = {
#include "rsa_keys.inc"
};

const RsaKey *find_key(uint64_t fp) {
    for (const auto &k : kKeys)
        if (k.fingerprint == fp) return &k;
    return nullptr;
}

struct BnDel {
    void operator()(BIGNUM *b) const { BN_clear_free(b); }
};
struct CtxDel {
    void operator()(BN_CTX *c) const { BN_CTX_free(c); }
};
using Bn = std::unique_ptr<BIGNUM, BnDel>;
using Ctx = std::unique_ptr<BN_CTX, CtxDel>;

}  // namespace

const uint8_t *rsa_key_modulus(uint64_t fingerprint) {
    const RsaKey *k = find_key(fingerprint);
    return k == nullptr ? nullptr : k->n;
}

tg_status rsa_pad_once(const uint8_t *data, size_t len, uint64_t fingerprint, const uint8_t *padding,
                       const uint8_t temp_key[32], tg_buf *out) {
    if ((data == nullptr && len != 0) || out == nullptr || temp_key == nullptr) return TG_ERR_ARG;
    if (len > 144) return TG_ERR_ARG;
    if (padding == nullptr && len < 192) return TG_ERR_ARG;
    const RsaKey *key = find_key(fingerprint);
    if (key == nullptr) return TG_ERR_ARG;

    Secure dwp(192);
    if (len != 0) std::memcpy(dwp.data(), data, len);
    std::memcpy(dwp.data() + len, padding, 192 - len);

    Secure dwh(224);
    for (size_t i = 0; i < 192; ++i) dwh.data()[i] = dwp.data()[191 - i];
    sha256_parts({{temp_key, 32}, {dwp.data(), 192}}, dwh.data() + 192);

    Secure kae(256);  // key_aes_encrypted
    const uint8_t zero_iv[32] = {};
    if (!aes_ige_encrypt(temp_key, 32, zero_iv, dwh.data(), kae.data() + 32, 224)) return TG_ERR_CRYPTO;
    uint8_t h[32];
    sha256(kae.data() + 32, 224, h);
    for (int i = 0; i < 32; ++i) kae.data()[i] = temp_key[i] ^ h[i];

    // Сравнение с модулем как 256-байтных big-endian чисел.
    if (std::memcmp(kae.data(), key->n, 256) >= 0) return TG_ERR_STATE;

    Ctx ctx(BN_CTX_new());
    Bn m(BN_bin2bn(kae.data(), 256, nullptr));
    Bn n(BN_bin2bn(key->n, 256, nullptr));
    Bn e(BN_new());
    Bn r(BN_new());
    if (!ctx || !m || !n || !e || !r) return TG_ERR_NOMEM;
    if (BN_set_word(e.get(), key->e) != 1 || BN_mod_exp(r.get(), m.get(), e.get(), n.get(), ctx.get()) != 1)
        return TG_ERR_CRYPTO;
    uint8_t res[256];
    if (BN_bn2binpad(r.get(), res, 256) != 256) return TG_ERR_CRYPTO;
    return make_buf(res, 256, out);
}

}  // namespace tg

extern "C" int tg_rsa_has_key(uint64_t fingerprint) { return tg::find_key(fingerprint) != nullptr ? 1 : 0; }

extern "C" tg_status tg_rsa_pad_encrypt(const uint8_t *data, size_t len, uint64_t fingerprint, tg_buf *out) {
    TG_GUARD_BEGIN
    if (out == nullptr || (data == nullptr && len != 0) || len > 144) return TG_ERR_ARG;
    if (tg::find_key(fingerprint) == nullptr) return TG_ERR_ARG;
    tg::Secure padding(192 - len);
    if (!tg::random_bytes(padding.data(), padding.size())) return TG_ERR_CRYPTO;
    // Вероятность «больше модуля» мала (модуль начинается с 0xe8/0xc8), но ограничим попытки.
    for (int attempt = 0; attempt < 64; ++attempt) {
        uint8_t temp_key[32];
        if (!tg::random_bytes(temp_key, sizeof temp_key)) return TG_ERR_CRYPTO;
        const tg_status st = tg::rsa_pad_once(data, len, fingerprint, padding.data(), temp_key, out);
        OPENSSL_cleanse(temp_key, sizeof temp_key);
        if (st != TG_ERR_STATE) return st;
    }
    return TG_ERR_CRYPTO;
    TG_GUARD_END
}

// ---- pq ----

namespace {

uint64_t mulmod(uint64_t a, uint64_t b, uint64_t m) {
    return static_cast<uint64_t>((static_cast<unsigned __int128>(a) * b) % m);
}

uint64_t gcd(uint64_t a, uint64_t b) {
    while (b != 0) {
        const uint64_t t = a % b;
        a = b;
        b = t;
    }
    return a;
}

// Ро-метод Полларда в варианте Брента; 0 — делитель не найден за отведённое число шагов.
uint64_t brent(uint64_t n, uint64_t c, uint64_t y0) {
    constexpr uint64_t kMaxSteps = 1u << 20;  // ожидаемо ~2^16 шагов для множителя < 2^32
    uint64_t y = y0, x = y0, ys = y0, q = 1, g = 1, r = 1, steps = 0;
    const uint64_t m = 128;
    while (g == 1) {
        x = y;
        for (uint64_t i = 0; i < r; ++i) y = (mulmod(y, y, n) + c) % n;
        uint64_t k = 0;
        while (k < r && g == 1) {
            ys = y;
            const uint64_t lim = (m < r - k) ? m : r - k;
            for (uint64_t i = 0; i < lim; ++i) {
                y = (mulmod(y, y, n) + c) % n;
                q = mulmod(q, x > y ? x - y : y - x, n);
            }
            g = gcd(q, n);
            k += m;
            steps += lim;
            if (steps > kMaxSteps) return 0;
        }
        r *= 2;
    }
    if (g == n) {
        do {
            ys = (mulmod(ys, ys, n) + c) % n;
            g = gcd(x > ys ? x - ys : ys - x, n);
            if (++steps > kMaxSteps) return 0;
        } while (g == 1);
    }
    return (g == n) ? 0 : g;
}

}  // namespace

extern "C" tg_status tg_pq_factorize(uint64_t pq, uint32_t *p, uint32_t *q) {
    if (p == nullptr || q == nullptr) return TG_ERR_ARG;
    *p = *q = 0;
    if (pq < 4) return TG_ERR_ARG;
    uint64_t d = 0;
    if (pq % 2 == 0) {
        d = 2;
    } else {
        for (uint64_t c = 1; c < 16 && d == 0; ++c) d = brent(pq, c, 2 + c);
    }
    if (d == 0) return TG_ERR_ARG;
    uint64_t a = d, b = pq / d;
    if (a > b) std::swap(a, b);
    if (a < 2 || b > 0xffffffffULL || a * b != pq) return TG_ERR_ARG;
    *p = static_cast<uint32_t>(a);
    *q = static_cast<uint32_t>(b);
    return TG_OK;
}
