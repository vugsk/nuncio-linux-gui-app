// Ключ авторизации (auth_key, 256 байт). Байты наружу не выдаются; для файла
// сессии ключ запечатывается AES-256-GCM ключом обёртки из Secret Service (О-6).
//
// Формат запечатанного вида (288 байт):
//   "NKS1" (4) | nonce (12) | шифротекст auth_key (256) | тег GCM (16)
// Заголовок "NKS1" входит в AAD, так что подмена версии ломает тег.
#include "internal.h"

#include <openssl/evp.h>

struct tg_key {
    uint8_t bytes[256];
    uint64_t id;
    ~tg_key() { OPENSSL_cleanse(bytes, sizeof bytes); }
};

namespace tg {
namespace {

constexpr uint8_t kMagic[4] = {'N', 'K', 'S', '1'};
constexpr size_t kSealedLen = 4 + 12 + 256 + 16;

uint64_t compute_id(const uint8_t auth_key[256]) {
    uint8_t h[20];
    sha1(auth_key, 256, h);
    uint64_t id = 0;
    for (int i = 0; i < 8; ++i) id |= static_cast<uint64_t>(h[12 + i]) << (8 * i);  // 64 младших бита, LE
    return id;
}

}  // namespace

tg_key *key_from_bytes(const uint8_t auth_key[256]) {
    auto *k = new (std::nothrow) tg_key;
    if (k == nullptr) return nullptr;
    std::memcpy(k->bytes, auth_key, 256);
    k->id = compute_id(auth_key);
    return k;
}

const uint8_t *key_bytes(const tg_key *k) { return k->bytes; }

tg_status key_seal_with_nonce(const tg_key *k, const uint8_t wrap[32], const uint8_t nonce[12], tg_buf *out) {
    if (k == nullptr || wrap == nullptr || nonce == nullptr || out == nullptr) return TG_ERR_ARG;
    Secure res(kSealedLen);
    std::memcpy(res.data(), kMagic, 4);
    std::memcpy(res.data() + 4, nonce, 12);
    EVP_CIPHER_CTX *c = EVP_CIPHER_CTX_new();
    if (c == nullptr) return TG_ERR_NOMEM;
    int outl = 0, fin = 0;
    bool ok = EVP_EncryptInit_ex(c, EVP_aes_256_gcm(), nullptr, nullptr, nullptr) == 1 &&
              EVP_CIPHER_CTX_ctrl(c, EVP_CTRL_GCM_SET_IVLEN, 12, nullptr) == 1 &&
              EVP_EncryptInit_ex(c, nullptr, nullptr, wrap, nonce) == 1 &&
              EVP_EncryptUpdate(c, nullptr, &outl, kMagic, 4) == 1 &&
              EVP_EncryptUpdate(c, res.data() + 16, &outl, k->bytes, 256) == 1 && outl == 256 &&
              EVP_EncryptFinal_ex(c, res.data() + 16 + 256, &fin) == 1 && fin == 0 &&
              EVP_CIPHER_CTX_ctrl(c, EVP_CTRL_GCM_GET_TAG, 16, res.data() + 16 + 256) == 1;
    EVP_CIPHER_CTX_free(c);
    if (!ok) return TG_ERR_CRYPTO;
    return make_buf(res.data(), res.size(), out);
}

}  // namespace tg

extern "C" uint64_t tg_key_id(const tg_key *k) { return k == nullptr ? 0 : k->id; }

extern "C" tg_status tg_key_seal(const tg_key *k, const uint8_t wrap[32], tg_buf *out) {
    TG_GUARD_BEGIN
    uint8_t nonce[12];
    if (!tg::random_bytes(nonce, sizeof nonce)) return TG_ERR_CRYPTO;
    return tg::key_seal_with_nonce(k, wrap, nonce, out);
    TG_GUARD_END
}

extern "C" tg_status tg_key_unseal(const uint8_t *in, size_t len, const uint8_t wrap[32], tg_key **out) {
    TG_GUARD_BEGIN
    if (in == nullptr || wrap == nullptr || out == nullptr) return TG_ERR_ARG;
    *out = nullptr;
    if (len != tg::kSealedLen || std::memcmp(in, tg::kMagic, 4) != 0) return TG_ERR_ARG;
    tg::Secure plain(256);
    EVP_CIPHER_CTX *c = EVP_CIPHER_CTX_new();
    if (c == nullptr) return TG_ERR_NOMEM;
    int outl = 0, fin = 0;
    uint8_t tag[16];
    std::memcpy(tag, in + 16 + 256, 16);
    bool ok = EVP_DecryptInit_ex(c, EVP_aes_256_gcm(), nullptr, nullptr, nullptr) == 1 &&
              EVP_CIPHER_CTX_ctrl(c, EVP_CTRL_GCM_SET_IVLEN, 12, nullptr) == 1 &&
              EVP_DecryptInit_ex(c, nullptr, nullptr, wrap, in + 4) == 1 &&
              EVP_DecryptUpdate(c, nullptr, &outl, tg::kMagic, 4) == 1 &&
              EVP_DecryptUpdate(c, plain.data(), &outl, in + 16, 256) == 1 && outl == 256 &&
              EVP_CIPHER_CTX_ctrl(c, EVP_CTRL_GCM_SET_TAG, 16, tag) == 1 &&
              EVP_DecryptFinal_ex(c, plain.data() + 256, &fin) == 1;
    EVP_CIPHER_CTX_free(c);
    if (!ok) return TG_ERR_CRYPTO;
    tg_key *k = tg::key_from_bytes(plain.data());
    if (k == nullptr) return TG_ERR_NOMEM;
    *out = k;
    return TG_OK;
    TG_GUARD_END
}

extern "C" void tg_key_free(tg_key *k) { delete k; }
