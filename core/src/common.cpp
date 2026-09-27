// Общие функции ядра: версия, буферы, хэши, случайные числа.
#include "internal.h"

#include <cstdlib>
#include <stdexcept>

#include <openssl/evp.h>
#include <openssl/rand.h>
#include <openssl/sha.h>

#ifndef TGCRYPTO_VERSION
#error "TGCRYPTO_VERSION задаётся в CMakeLists.txt"
#endif

namespace tg {

tg_status make_buf(const uint8_t *p, size_t n, tg_buf *out) {
    out->ptr = nullptr;
    out->len = 0;
    auto *mem = static_cast<uint8_t *>(std::malloc(n == 0 ? 1 : n));
    if (mem == nullptr) return TG_ERR_NOMEM;
    if (n != 0) std::memcpy(mem, p, n);
    out->ptr = mem;
    out->len = n;
    return TG_OK;
}

void sha1(const uint8_t *in, size_t len, uint8_t out[20]) { SHA1(in, len, out); }
void sha256(const uint8_t *in, size_t len, uint8_t out[32]) { SHA256(in, len, out); }

namespace {
void digest_parts(const EVP_MD *md, std::initializer_list<std::pair<const uint8_t *, size_t>> parts,
                  uint8_t *out) {
    EVP_MD_CTX *c = EVP_MD_CTX_new();
    if (c == nullptr) throw std::bad_alloc();
    bool ok = EVP_DigestInit_ex(c, md, nullptr) == 1;
    for (const auto &[p, n] : parts) ok = ok && EVP_DigestUpdate(c, p, n) == 1;
    ok = ok && EVP_DigestFinal_ex(c, out, nullptr) == 1;
    EVP_MD_CTX_free(c);
    if (!ok) throw std::runtime_error("digest");
}
}  // namespace

void sha256_parts(std::initializer_list<std::pair<const uint8_t *, size_t>> parts, uint8_t out[32]) {
    digest_parts(EVP_sha256(), parts, out);
}

void sha1_parts(std::initializer_list<std::pair<const uint8_t *, size_t>> parts, uint8_t out[20]) {
    digest_parts(EVP_sha1(), parts, out);
}

bool random_bytes(uint8_t *out, size_t len) {
    while (len > 0) {
        int chunk = len > (1u << 30) ? (1 << 30) : static_cast<int>(len);
        if (RAND_bytes(out, chunk) != 1) return false;
        out += chunk;
        len -= static_cast<size_t>(chunk);
    }
    return true;
}

}  // namespace tg

extern "C" const char *tg_version(void) { return "tgcrypto " TGCRYPTO_VERSION; }

extern "C" void tg_buf_free(tg_buf b) {
    if (b.ptr == nullptr) return;
    // Буфер может содержать ключевой материал: стираем перед освобождением.
    OPENSSL_cleanse(b.ptr, b.len);
    std::free(b.ptr);
}

extern "C" tg_status tg_sha256(const uint8_t *in, size_t len, uint8_t out[32]) {
    if ((in == nullptr && len != 0) || out == nullptr) return TG_ERR_ARG;
    tg::sha256(in, len, out);
    return TG_OK;
}

extern "C" tg_status tg_random(uint8_t *out, size_t len) {
    if (out == nullptr && len != 0) return TG_ERR_ARG;
    return tg::random_bytes(out, len) ? TG_OK : TG_ERR_CRYPTO;
}
