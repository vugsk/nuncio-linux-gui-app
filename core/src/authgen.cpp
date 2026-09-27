// Обмен ключами (core.telegram.org/mtproto/auth_key, пп. 5–9). TL строит и
// разбирает Go; ядро выводит временный ключ, (рас)шифровывает DH-сообщения,
// проверяет параметры, считает g_b и auth_key, хэши new_nonce_hash{1,2,3}.
//
//   tmp_aes_key := SHA1(new_nonce + server_nonce) + substr(SHA1(server_nonce + new_nonce), 0, 12)
//   tmp_aes_iv  := substr(SHA1(server_nonce + new_nonce), 12, 8) + SHA1(new_nonce + new_nonce)
//                  + substr(new_nonce, 0, 4)
//   answer_with_hash := SHA1(answer) + answer + (0..15 случайных байт), длина кратна 16
//   auth_key := g_a ^ b mod dh_prime
//   auth_key_aux_hash := 64 старших бита SHA1(auth_key)
//   new_nonce_hash{n} := 128 младших бит SHA1(new_nonce + [n] + auth_key_aux_hash)
#include "internal.h"

#include <memory>

#include <openssl/bn.h>

struct tg_authgen {
    uint8_t new_nonce[32];
    uint8_t server_nonce[16];
    uint8_t tmp_key[32];
    uint8_t tmp_iv[32];
    uint8_t auth_key[256];
    bool have_key = false;
    bool finished = false;
    ~tg_authgen() { OPENSSL_cleanse(this, sizeof *this); }
};

namespace tg {
namespace {

struct BnDel {
    void operator()(BIGNUM *b) const { BN_clear_free(b); }
};
struct CtxDel {
    void operator()(BN_CTX *c) const { BN_CTX_free(c); }
};
using Bn = std::unique_ptr<BIGNUM, BnDel>;
using Ctx = std::unique_ptr<BN_CTX, CtxDel>;

bool usable(const tg_authgen *g) { return g != nullptr && !g->finished; }

}  // namespace

void authgen_tmp_key(const uint8_t new_nonce[32], const uint8_t server_nonce[16], uint8_t key[32],
                     uint8_t iv[32]) {
    uint8_t ns[20], sn[20], nn[20];
    sha1_parts({{new_nonce, 32}, {server_nonce, 16}}, ns);
    sha1_parts({{server_nonce, 16}, {new_nonce, 32}}, sn);
    sha1_parts({{new_nonce, 32}, {new_nonce, 32}}, nn);
    std::memcpy(key, ns, 20);
    std::memcpy(key + 20, sn, 12);
    std::memcpy(iv, sn + 12, 8);
    std::memcpy(iv + 8, nn, 20);
    std::memcpy(iv + 28, new_nonce, 4);
    OPENSSL_cleanse(ns, sizeof ns);
    OPENSSL_cleanse(sn, sizeof sn);
    OPENSSL_cleanse(nn, sizeof nn);
}

tg_status authgen_encrypt_dh_with_pad(tg_authgen *g, const uint8_t *inner, size_t len, const uint8_t *pad,
                                      size_t pad_len, tg_buf *enc) {
    if (!usable(g) || (inner == nullptr && len != 0) || enc == nullptr) return TG_ERR_ARG;
    if (pad_len > 15 || (20 + len + pad_len) % 16 != 0 || (pad == nullptr && pad_len != 0)) return TG_ERR_ARG;
    if (len > (1u << 20)) return TG_ERR_ARG;
    const size_t total = 20 + len + pad_len;
    Secure data(total);
    sha1(inner, len, data.data());
    if (len != 0) std::memcpy(data.data() + 20, inner, len);
    if (pad_len != 0) std::memcpy(data.data() + 20 + len, pad, pad_len);
    Secure out(total);
    if (!aes_ige_encrypt(g->tmp_key, 32, g->tmp_iv, data.data(), out.data(), total)) return TG_ERR_CRYPTO;
    return make_buf(out.data(), total, enc);
}

tg_status authgen_dh_with_b(tg_authgen *g, const uint8_t *dh_prime, size_t plen, uint32_t gen,
                            const uint8_t *g_a, size_t ga_len, const uint8_t b[256], tg_buf *g_b) {
    if (!usable(g) || dh_prime == nullptr || g_a == nullptr || b == nullptr || g_b == nullptr) return TG_ERR_ARG;
    g->have_key = false;
    const tg_status pst = dh_check_params(dh_prime, plen, gen);
    if (pst != TG_OK) return pst;
    if (!dh_check_value(g_a, ga_len, dh_prime, plen, true)) return TG_ERR_DH;

    Ctx ctx(BN_CTX_new());
    Bn p(BN_bin2bn(dh_prime, 256, nullptr));
    Bn ga(BN_bin2bn(g_a, static_cast<int>(ga_len), nullptr));
    Bn bb(BN_bin2bn(b, 256, nullptr));
    Bn gg(BN_new());
    Bn gb(BN_new());
    Bn key(BN_new());
    if (!ctx || !p || !ga || !bb || !gg || !gb || !key) return TG_ERR_NOMEM;
    BN_set_flags(bb.get(), BN_FLG_CONSTTIME);
    if (BN_set_word(gg.get(), gen) != 1 ||
        BN_mod_exp(gb.get(), gg.get(), bb.get(), p.get(), ctx.get()) != 1 ||
        BN_mod_exp(key.get(), ga.get(), bb.get(), p.get(), ctx.get()) != 1)
        return TG_ERR_CRYPTO;
    uint8_t gb_bytes[256];
    if (BN_bn2binpad(gb.get(), gb_bytes, 256) != 256) return TG_ERR_CRYPTO;
    // Своё значение тоже проверяем по рекомендованному диапазону.
    if (!dh_check_value(gb_bytes, 256, dh_prime, 256, true)) return TG_ERR_STATE;
    if (BN_bn2binpad(key.get(), g->auth_key, 256) != 256) return TG_ERR_CRYPTO;
    const tg_status st = make_buf(gb_bytes, 256, g_b);
    if (st == TG_OK) g->have_key = true;
    return st;
}

}  // namespace tg

extern "C" tg_status tg_authgen_new(const uint8_t new_nonce[32], const uint8_t server_nonce[16], tg_authgen **out) {
    TG_GUARD_BEGIN
    if (new_nonce == nullptr || server_nonce == nullptr || out == nullptr) return TG_ERR_ARG;
    *out = nullptr;
    auto *g = new tg_authgen;
    std::memcpy(g->new_nonce, new_nonce, 32);
    std::memcpy(g->server_nonce, server_nonce, 16);
    tg::authgen_tmp_key(new_nonce, server_nonce, g->tmp_key, g->tmp_iv);
    *out = g;
    return TG_OK;
    TG_GUARD_END
}

extern "C" tg_status tg_authgen_decrypt_dh(tg_authgen *g, const uint8_t *enc, size_t len, tg_buf *inner) {
    TG_GUARD_BEGIN
    if (!tg::usable(g) || enc == nullptr || inner == nullptr) return TG_ERR_ARG;
    inner->ptr = nullptr;
    inner->len = 0;
    if (len < 32 || len % 16 != 0 || len > (1u << 20)) return TG_ERR_ARG;
    tg::Secure dec(len);
    if (!tg::aes_ige_decrypt(g->tmp_key, 32, g->tmp_iv, enc, dec.data(), len)) return TG_ERR_CRYPTO;
    // Длину answer ядро не знает (TL не разбирает): подбираем дополнение 0..15,
    // при котором SHA1(answer) совпадает с первыми 20 байтами.
    uint8_t h[20];
    for (size_t pad = 0; pad < 16 && 20 + pad < len; ++pad) {
        const size_t n = len - 20 - pad;
        tg::sha1(dec.data() + 20, n, h);
        if (CRYPTO_memcmp(h, dec.data(), 20) == 0) return tg::make_buf(dec.data() + 20, n, inner);
    }
    return TG_ERR_CRYPTO;
    TG_GUARD_END
}

extern "C" tg_status tg_authgen_encrypt_dh(tg_authgen *g, const uint8_t *inner, size_t len, tg_buf *enc) {
    TG_GUARD_BEGIN
    const size_t pad_len = (16 - (20 + len) % 16) % 16;
    uint8_t pad[16];
    if (!tg::random_bytes(pad, pad_len)) return TG_ERR_CRYPTO;
    return tg::authgen_encrypt_dh_with_pad(g, inner, len, pad, pad_len, enc);
    TG_GUARD_END
}

extern "C" tg_status tg_authgen_dh(tg_authgen *g, const uint8_t *dh_prime, size_t plen, uint32_t gen,
                                   const uint8_t *g_a, size_t ga_len, tg_buf *g_b) {
    TG_GUARD_BEGIN
    if (g_b == nullptr) return TG_ERR_ARG;
    g_b->ptr = nullptr;
    g_b->len = 0;
    // Своё g_b вне рекомендованного диапазона (вероятность ~2^-63) — выбираем b заново.
    for (int attempt = 0; attempt < 8; ++attempt) {
        tg::Secure b(256);
        if (!tg::random_bytes(b.data(), 256)) return TG_ERR_CRYPTO;
        const tg_status st = tg::authgen_dh_with_b(g, dh_prime, plen, gen, g_a, ga_len, b.data(), g_b);
        if (st != TG_ERR_STATE) return st;
    }
    return TG_ERR_CRYPTO;
    TG_GUARD_END
}

extern "C" tg_status tg_authgen_nonce_hash(const tg_authgen *g, int n, uint8_t out[16]) {
    TG_GUARD_BEGIN
    if (!tg::usable(g) || !g->have_key || out == nullptr || n < 1 || n > 3) return TG_ERR_ARG;
    uint8_t kh[20], h[20];
    tg::sha1(g->auth_key, 256, kh);
    const uint8_t nb = static_cast<uint8_t>(n);
    tg::sha1_parts({{g->new_nonce, 32}, {&nb, 1}, {kh, 8}}, h);
    std::memcpy(out, h + 4, 16);
    OPENSSL_cleanse(kh, sizeof kh);
    return TG_OK;
    TG_GUARD_END
}

extern "C" tg_status tg_authgen_aux_hash(const tg_authgen *g, uint64_t *out) {
    TG_GUARD_BEGIN
    if (!tg::usable(g) || !g->have_key || out == nullptr) return TG_ERR_ARG;
    uint8_t kh[20];
    tg::sha1(g->auth_key, 256, kh);
    uint64_t v = 0;
    for (int i = 0; i < 8; ++i) v |= static_cast<uint64_t>(kh[i]) << (8 * i);
    *out = v;
    OPENSSL_cleanse(kh, sizeof kh);
    return TG_OK;
    TG_GUARD_END
}

extern "C" tg_status tg_authgen_finish(tg_authgen *g, tg_key **key) {
    TG_GUARD_BEGIN
    if (!tg::usable(g) || key == nullptr) return TG_ERR_ARG;
    *key = nullptr;
    if (!g->have_key) return TG_ERR_STATE;
    tg_key *k = tg::key_from_bytes(g->auth_key);
    if (k == nullptr) return TG_ERR_NOMEM;
    g->finished = true;
    OPENSSL_cleanse(g->auth_key, sizeof g->auth_key);
    *key = k;
    return TG_OK;
    TG_GUARD_END
}

extern "C" void tg_authgen_free(tg_authgen *g) { delete g; }
