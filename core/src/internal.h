// Внутренние интерфейсы ядра. Не входят в C ABI: используются реализацией,
// а также тестами, фаззерами и сверкой с эталоном (tests/refcheck.cpp).
// Детерминированные варианты принимают «случайные» значения явно.
#ifndef NUNCIO_TGCRYPTO_INTERNAL_H
#define NUNCIO_TGCRYPTO_INTERNAL_H

#include "tgcrypto.h"

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <initializer_list>
#include <new>
#include <utility>
#include <vector>

#include <openssl/crypto.h>

namespace tg {

// Вектор, стирающий содержимое при разрушении и перевыделении не допускающий
// (используется только через resize до нужного размера один раз).
struct Secure {
    std::vector<uint8_t> v;
    Secure() = default;
    explicit Secure(size_t n) : v(n) {}
    Secure(const uint8_t *p, size_t n) : v(p, p + n) {}
    Secure(const Secure &) = delete;
    Secure &operator=(const Secure &) = delete;
    Secure(Secure &&) = default;
    Secure &operator=(Secure &&) = default;
    ~Secure() { wipe(); }
    void wipe() {
        if (!v.empty()) OPENSSL_cleanse(v.data(), v.size());
    }
    uint8_t *data() { return v.data(); }
    const uint8_t *data() const { return v.data(); }
    size_t size() const { return v.size(); }
};

// ---- common.cpp ----
tg_status make_buf(const uint8_t *p, size_t n, tg_buf *out);
void sha1(const uint8_t *in, size_t len, uint8_t out[20]);
void sha256(const uint8_t *in, size_t len, uint8_t out[32]);
// SHA-256 от конкатенации нескольких кусков.
void sha256_parts(std::initializer_list<std::pair<const uint8_t *, size_t>> parts, uint8_t out[32]);
void sha1_parts(std::initializer_list<std::pair<const uint8_t *, size_t>> parts, uint8_t out[20]);
bool random_bytes(uint8_t *out, size_t len);

// ---- aes_ige.cpp ----
// AES-IGE по соглашению Telegram: iv = 32 байта (первые 16 — предыдущий шифроблок,
// вторые 16 — предыдущий открытый блок). key_len — 16 или 32. len кратна 16.
bool aes_ige_encrypt(const uint8_t *key, size_t key_len, const uint8_t iv[32],
                     const uint8_t *in, uint8_t *out, size_t len);
bool aes_ige_decrypt(const uint8_t *key, size_t key_len, const uint8_t iv[32],
                     const uint8_t *in, uint8_t *out, size_t len);
// true, если используется путь AES-NI (иначе запасной через EVP).
bool aes_ige_uses_aesni();
// Для тестов: принудительно отключить AES-NI, чтобы проверить запасной путь.
void aes_ige_force_fallback(bool on);

// ---- key.cpp ----
tg_key *key_from_bytes(const uint8_t auth_key[256]);
const uint8_t *key_bytes(const tg_key *k);
// Запечатывание с явным nonce (12 байт).
tg_status key_seal_with_nonce(const tg_key *k, const uint8_t wrap[32], const uint8_t nonce[12], tg_buf *out);

// ---- envelope.cpp ----
// x = 0 — клиент -> сервер, x = 8 — сервер -> клиент.
tg_status env_seal(const tg_key *k, int x, const tg_env_hdr *h, const uint8_t *body, size_t len,
                   const uint8_t *padding, size_t pad_len, tg_buf *out);
tg_status env_open(const tg_key *k, int x, const uint8_t *in, size_t len, tg_env_hdr *h, tg_buf *body);
// Разбор уже расшифрованного и проверенного открытого текста (цель фаззинга, см. П-4).
tg_status env_parse_plaintext(const uint8_t *pt, size_t len, tg_env_hdr *h, size_t *body_off, size_t *body_len);

// ---- rsa_pad.cpp ----
// Одна попытка RSA_PAD с заданными temp_key и дополнением (192 - len байт).
// TG_ERR_STATE — key_aes_encrypted >= модуля (нужна новая попытка).
tg_status rsa_pad_once(const uint8_t *data, size_t len, uint64_t fingerprint, const uint8_t *padding,
                       const uint8_t temp_key[32], tg_buf *out);
// Модуль встроенного ключа (256 байт) или nullptr — для сверки таблицы с эталоном.
const uint8_t *rsa_key_modulus(uint64_t fingerprint);

// ---- dh.cpp ----
// Проверки из спецификации: p — безопасное 2048-битное простое, g в 2..7 порождает
// подгруппу порядка (p-1)/2. Известные простые не перепроверяются.
tg_status dh_check_params(const uint8_t *p, size_t plen, uint32_t g);
// 1 < x < p-1 и (если strict) 2^{2048-64} <= x <= p - 2^{2048-64}.
bool dh_check_value(const uint8_t *x, size_t xlen, const uint8_t *p, size_t plen, bool strict);
// dh_prime из документации (256 байт) — для тестов.
const uint8_t *dh_known_prime();

// ---- authgen.cpp ----
void authgen_tmp_key(const uint8_t new_nonce[32], const uint8_t server_nonce[16], uint8_t key[32], uint8_t iv[32]);
// tg_authgen_dh с заданным b (256 байт) вместо случайного.
tg_status authgen_dh_with_b(tg_authgen *g, const uint8_t *dh_prime, size_t plen, uint32_t gen,
                            const uint8_t *g_a, size_t ga_len, const uint8_t b[256], tg_buf *g_b);
// tg_authgen_encrypt_dh с заданным дополнением (длина должна дать кратность 16).
tg_status authgen_encrypt_dh_with_pad(tg_authgen *g, const uint8_t *inner, size_t len,
                                      const uint8_t *pad, size_t pad_len, tg_buf *enc);

// ---- srp.cpp ----
// tg_srp_check с заданным a (256 байт) и числом итераций PBKDF2 (в протоколе 100000;
// другое значение — только для фаззинга).
tg_status srp_check_with_a(const uint8_t *pw, size_t pw_len, const uint8_t *salt1, size_t s1,
                           const uint8_t *salt2, size_t s2, uint32_t g, const uint8_t *p, size_t plen,
                           const uint8_t *srp_b, size_t blen, const uint8_t a[256], int iterations,
                           tg_buf *a_out, tg_buf *m1_out);

// ---- obfs.cpp ----
// tg_obfs_new с заданными 64 байтами init (байты 56..64 перезаписываются тегом и dc).
tg_status obfs_new_with_init(int transport, int dc_id, const uint8_t *secret, size_t slen,
                             const uint8_t init_random[64], tg_buf *init, tg_obfs **out);
// Проверка первых байт init на запрещённые значения (true — годится).
bool obfs_init_ok(const uint8_t init[64]);

}  // namespace tg

// Обёртка экспортируемых функций: исключения не пересекают C ABI.
#define TG_GUARD_BEGIN try {
#define TG_GUARD_END                          \
    }                                         \
    catch (const std::bad_alloc &) {          \
        return TG_ERR_NOMEM;                  \
    }                                         \
    catch (...) {                             \
        return TG_ERR_STATE;                  \
    }

#endif  // NUNCIO_TGCRYPTO_INTERNAL_H
