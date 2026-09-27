// AES-IGE (Infinite Garble Extension) в варианте Telegram.
// Основной путь — собственный цикл на AES-NI (замер П-3: в 2,4 раза быстрее Go,
// а готовая AES_ige_encrypt OpenSSL втрое медленнее Go). Расписание ключей тоже
// на AES-NI (схема из белой книги Intel), без приведения AES_KEY к __m128i
// (в П-3 это давало падение из-за выравнивания). Запасной путь — AES-ECB через
// EVP поблочно, если процессор без AES-NI.
//
//   c_i = E(p_i ^ c_{i-1}) ^ p_{i-1},   c_{-1} = iv[0..16], p_{-1} = iv[16..32]
//   p_i = D(c_i ^ p_{i-1}) ^ c_{i-1}
#include "internal.h"

#include <atomic>

#include <immintrin.h>
#include <openssl/evp.h>

namespace tg {
namespace {

std::atomic<bool> g_force_fallback{false};

bool cpu_has_aesni() {
    static const bool has = __builtin_cpu_supports("aes") && __builtin_cpu_supports("sse4.1");
    return has;
}

// ---------- AES-NI ----------

struct NiKeys {
    __m128i rk[15];  // раунды шифрования
    __m128i dk[15];  // раунды расшифровки (aesimc)
    int rounds = 0;
    ~NiKeys() { OPENSSL_cleanse(this, sizeof *this); }
};

__attribute__((target("aes,sse4.1"))) inline __m128i expand_step(__m128i k, __m128i t) {
    k = _mm_xor_si128(k, _mm_slli_si128(k, 4));
    k = _mm_xor_si128(k, _mm_slli_si128(k, 4));
    k = _mm_xor_si128(k, _mm_slli_si128(k, 4));
    return _mm_xor_si128(k, t);
}

#define TG_EXP128(i, rcon) \
    rk[i] = expand_step(rk[(i)-1], _mm_shuffle_epi32(_mm_aeskeygenassist_si128(rk[(i)-1], rcon), 0xff))

__attribute__((target("aes,sse4.1"))) void expand128(const uint8_t *key, NiKeys &k) {
    __m128i *rk = k.rk;
    rk[0] = _mm_loadu_si128(reinterpret_cast<const __m128i *>(key));
    TG_EXP128(1, 0x01); TG_EXP128(2, 0x02); TG_EXP128(3, 0x04); TG_EXP128(4, 0x08);
    TG_EXP128(5, 0x10); TG_EXP128(6, 0x20); TG_EXP128(7, 0x40); TG_EXP128(8, 0x80);
    TG_EXP128(9, 0x1b); TG_EXP128(10, 0x36);
    k.rounds = 10;
}

#define TG_EXP256_A(i, rcon) \
    rk[i] = expand_step(rk[(i)-2], _mm_shuffle_epi32(_mm_aeskeygenassist_si128(rk[(i)-1], rcon), 0xff))
#define TG_EXP256_B(i) \
    rk[i] = expand_step(rk[(i)-2], _mm_shuffle_epi32(_mm_aeskeygenassist_si128(rk[(i)-1], 0x00), 0xaa))

__attribute__((target("aes,sse4.1"))) void expand256(const uint8_t *key, NiKeys &k) {
    __m128i *rk = k.rk;
    rk[0] = _mm_loadu_si128(reinterpret_cast<const __m128i *>(key));
    rk[1] = _mm_loadu_si128(reinterpret_cast<const __m128i *>(key + 16));
    TG_EXP256_A(2, 0x01); TG_EXP256_B(3);
    TG_EXP256_A(4, 0x02); TG_EXP256_B(5);
    TG_EXP256_A(6, 0x04); TG_EXP256_B(7);
    TG_EXP256_A(8, 0x08); TG_EXP256_B(9);
    TG_EXP256_A(10, 0x10); TG_EXP256_B(11);
    TG_EXP256_A(12, 0x20); TG_EXP256_B(13);
    TG_EXP256_A(14, 0x40);
    k.rounds = 14;
}

__attribute__((target("aes,sse4.1"))) void make_dec_keys(NiKeys &k) {
    const int n = k.rounds;
    k.dk[0] = k.rk[n];
    for (int i = 1; i < n; ++i) k.dk[i] = _mm_aesimc_si128(k.rk[n - i]);
    k.dk[n] = k.rk[0];
}

__attribute__((target("aes,sse4.1"))) inline __m128i enc_block(const NiKeys &k, __m128i x) {
    x = _mm_xor_si128(x, k.rk[0]);
    for (int i = 1; i < k.rounds; ++i) x = _mm_aesenc_si128(x, k.rk[i]);
    return _mm_aesenclast_si128(x, k.rk[k.rounds]);
}

__attribute__((target("aes,sse4.1"))) inline __m128i dec_block(const NiKeys &k, __m128i x) {
    x = _mm_xor_si128(x, k.dk[0]);
    for (int i = 1; i < k.rounds; ++i) x = _mm_aesdec_si128(x, k.dk[i]);
    return _mm_aesdeclast_si128(x, k.dk[k.rounds]);
}

__attribute__((target("aes,sse4.1"))) void ige_ni(const uint8_t *key, size_t key_len, const uint8_t iv[32],
                                                   const uint8_t *in, uint8_t *out, size_t len, bool enc) {
    NiKeys k;
    if (key_len == 16) expand128(key, k); else expand256(key, k);
    __m128i prev_c = _mm_loadu_si128(reinterpret_cast<const __m128i *>(iv));
    __m128i prev_p = _mm_loadu_si128(reinterpret_cast<const __m128i *>(iv + 16));
    if (enc) {
        for (size_t off = 0; off < len; off += 16) {
            const __m128i p = _mm_loadu_si128(reinterpret_cast<const __m128i *>(in + off));
            const __m128i c = _mm_xor_si128(enc_block(k, _mm_xor_si128(p, prev_c)), prev_p);
            _mm_storeu_si128(reinterpret_cast<__m128i *>(out + off), c);
            prev_c = c;
            prev_p = p;
        }
    } else {
        make_dec_keys(k);
        for (size_t off = 0; off < len; off += 16) {
            const __m128i c = _mm_loadu_si128(reinterpret_cast<const __m128i *>(in + off));
            const __m128i p = _mm_xor_si128(dec_block(k, _mm_xor_si128(c, prev_p)), prev_c);
            _mm_storeu_si128(reinterpret_cast<__m128i *>(out + off), p);
            prev_c = c;
            prev_p = p;
        }
    }
}

// ---------- запасной путь: EVP AES-ECB поблочно ----------

bool ige_evp(const uint8_t *key, size_t key_len, const uint8_t iv[32], const uint8_t *in, uint8_t *out,
             size_t len, bool enc) {
    const EVP_CIPHER *cipher = key_len == 16 ? EVP_aes_128_ecb() : EVP_aes_256_ecb();
    EVP_CIPHER_CTX *ctx = EVP_CIPHER_CTX_new();
    if (ctx == nullptr) return false;
    bool ok = EVP_CipherInit_ex(ctx, cipher, nullptr, key, nullptr, enc ? 1 : 0) == 1 &&
              EVP_CIPHER_CTX_set_padding(ctx, 0) == 1;
    uint8_t prev_c[16], prev_p[16], tmp[16], blk[16];
    std::memcpy(prev_c, iv, 16);
    std::memcpy(prev_p, iv + 16, 16);
    for (size_t off = 0; ok && off < len; off += 16) {
        const uint8_t *src = in + off;
        int outl = 0;
        if (enc) {
            for (int i = 0; i < 16; ++i) tmp[i] = src[i] ^ prev_c[i];
            ok = EVP_CipherUpdate(ctx, blk, &outl, tmp, 16) == 1 && outl == 16;
            std::memcpy(tmp, src, 16);  // src и out могут совпадать
            for (int i = 0; i < 16; ++i) out[off + i] = blk[i] ^ prev_p[i];
            std::memcpy(prev_c, out + off, 16);
            std::memcpy(prev_p, tmp, 16);
        } else {
            for (int i = 0; i < 16; ++i) tmp[i] = src[i] ^ prev_p[i];
            ok = EVP_CipherUpdate(ctx, blk, &outl, tmp, 16) == 1 && outl == 16;
            std::memcpy(tmp, src, 16);
            for (int i = 0; i < 16; ++i) out[off + i] = blk[i] ^ prev_c[i];
            std::memcpy(prev_p, out + off, 16);
            std::memcpy(prev_c, tmp, 16);
        }
    }
    OPENSSL_cleanse(prev_c, 16);
    OPENSSL_cleanse(prev_p, 16);
    OPENSSL_cleanse(tmp, 16);
    OPENSSL_cleanse(blk, 16);
    EVP_CIPHER_CTX_free(ctx);
    return ok;
}

bool ige(const uint8_t *key, size_t key_len, const uint8_t iv[32], const uint8_t *in, uint8_t *out, size_t len,
         bool enc) {
    if (key == nullptr || iv == nullptr || (key_len != 16 && key_len != 32) || len % 16 != 0) return false;
    if (len == 0) return true;
    if (in == nullptr || out == nullptr) return false;
    if (aes_ige_uses_aesni()) {
        ige_ni(key, key_len, iv, in, out, len, enc);
        return true;
    }
    return ige_evp(key, key_len, iv, in, out, len, enc);
}

}  // namespace

bool aes_ige_uses_aesni() { return cpu_has_aesni() && !g_force_fallback.load(); }
void aes_ige_force_fallback(bool on) { g_force_fallback.store(on); }

bool aes_ige_encrypt(const uint8_t *key, size_t key_len, const uint8_t iv[32], const uint8_t *in, uint8_t *out,
                     size_t len) {
    return ige(key, key_len, iv, in, out, len, true);
}

bool aes_ige_decrypt(const uint8_t *key, size_t key_len, const uint8_t iv[32], const uint8_t *in, uint8_t *out,
                     size_t len) {
    return ige(key, key_len, iv, in, out, len, false);
}

}  // namespace tg
