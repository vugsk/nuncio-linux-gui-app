// SRP для двухэтапной проверки (core.telegram.org/api/srp),
// алгоритм passwordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow.
// Числа в хэшах — big-endian, дополненные до 2048 бит; вся арифметика по модулю p.
//
//   H(d) = SHA256(d);  SH(d, s) = H(s | d | s)
//   PH1 = SH(SH(password, salt1), salt2)
//   PH2 = SH(pbkdf2(sha512, PH1, salt1, 100000), salt2);  x = PH2
//   k = H(p | g);  u = H(g_a | g_b);  v = g^x;  k_v = k*v mod p
//   g_a = g^a;  t = (g_b - k_v) mod p;  s_a = t^(a + u*x) mod p;  k_a = H(s_a)
//   M1 = H(H(p) xor H(g) | H(salt1) | H(salt2) | g_a | g_b | k_a)
#include "internal.h"

#include <memory>

#include <openssl/bn.h>
#include <openssl/evp.h>

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

constexpr size_t kMaxSalt = 1024;
constexpr size_t kMaxPassword = 4096;

bool to256(const BIGNUM *b, uint8_t out[256]) { return BN_bn2binpad(b, out, 256) == 256; }

}  // namespace

tg_status srp_check_with_a(const uint8_t *pw, size_t pw_len, const uint8_t *salt1, size_t s1,
                           const uint8_t *salt2, size_t s2, uint32_t g, const uint8_t *p, size_t plen,
                           const uint8_t *srp_b, size_t blen, const uint8_t a[256], int iterations,
                           tg_buf *a_out, tg_buf *m1_out) {
    if ((pw == nullptr && pw_len != 0) || (salt1 == nullptr && s1 != 0) || (salt2 == nullptr && s2 != 0) ||
        p == nullptr || srp_b == nullptr || a == nullptr || a_out == nullptr || m1_out == nullptr ||
        iterations < 1)
        return TG_ERR_ARG;
    a_out->ptr = m1_out->ptr = nullptr;
    a_out->len = m1_out->len = 0;
    if (pw_len > kMaxPassword || s1 > kMaxSalt || s2 > kMaxSalt) return TG_ERR_ARG;
    const tg_status pst = dh_check_params(p, plen, g);
    if (pst != TG_OK) return pst;
    if (!dh_check_value(srp_b, blen, p, plen, false)) return TG_ERR_DH;

    // x = PH2(password, salt1, salt2)
    uint8_t t1[32], ph1[32], x_bytes[32];
    Secure pb(64);
    sha256_parts({{salt1, s1}, {pw, pw_len}, {salt1, s1}}, t1);
    sha256_parts({{salt2, s2}, {t1, 32}, {salt2, s2}}, ph1);
    if (PKCS5_PBKDF2_HMAC(reinterpret_cast<const char *>(ph1), 32, salt1, static_cast<int>(s1), iterations,
                          EVP_sha512(), 64, pb.data()) != 1)
        return TG_ERR_CRYPTO;
    sha256_parts({{salt2, s2}, {pb.data(), 64}, {salt2, s2}}, x_bytes);
    OPENSSL_cleanse(t1, 32);
    OPENSSL_cleanse(ph1, 32);

    uint8_t g_pad[256] = {};
    g_pad[252] = static_cast<uint8_t>(g >> 24);
    g_pad[253] = static_cast<uint8_t>(g >> 16);
    g_pad[254] = static_cast<uint8_t>(g >> 8);
    g_pad[255] = static_cast<uint8_t>(g);
    uint8_t gb_pad[256] = {};
    std::memcpy(gb_pad + (256 - blen), srp_b, blen);

    Ctx ctx(BN_CTX_new());
    Bn bp(BN_bin2bn(p, 256, nullptr)), bg(BN_new()), bx(BN_bin2bn(x_bytes, 32, nullptr));
    Bn ba(BN_bin2bn(a, 256, nullptr)), bgb(BN_bin2bn(gb_pad, 256, nullptr));
    Bn bga(BN_new()), bv(BN_new()), bk(BN_new()), bkv(BN_new()), bt(BN_new()), bu(BN_new());
    Bn bexp(BN_new()), bs(BN_new());
    OPENSSL_cleanse(x_bytes, 32);
    if (!ctx || !bp || !bg || !bx || !ba || !bgb || !bga || !bv || !bk || !bkv || !bt || !bu || !bexp || !bs)
        return TG_ERR_NOMEM;
    BN_set_flags(ba.get(), BN_FLG_CONSTTIME);
    BN_set_flags(bx.get(), BN_FLG_CONSTTIME);
    if (BN_set_word(bg.get(), g) != 1) return TG_ERR_CRYPTO;

    // g_a = g^a mod p, проверка диапазона.
    uint8_t ga_pad[256];
    if (BN_mod_exp(bga.get(), bg.get(), ba.get(), bp.get(), ctx.get()) != 1 || !to256(bga.get(), ga_pad))
        return TG_ERR_CRYPTO;
    if (!dh_check_value(ga_pad, 256, p, 256, true)) return TG_ERR_STATE;

    // k = H(p | g), u = H(g_a | g_b)
    uint8_t k_h[32], u_h[32];
    sha256_parts({{p, 256}, {g_pad, 256}}, k_h);
    sha256_parts({{ga_pad, 256}, {gb_pad, 256}}, u_h);
    if (BN_bin2bn(k_h, 32, bk.get()) == nullptr || BN_bin2bn(u_h, 32, bu.get()) == nullptr) return TG_ERR_NOMEM;
    if (BN_is_zero(bu.get())) return TG_ERR_DH;

    // v = g^x, k_v = k*v, t = (g_b - k_v) mod p
    if (BN_mod_exp(bv.get(), bg.get(), bx.get(), bp.get(), ctx.get()) != 1 ||
        BN_mod_mul(bkv.get(), bk.get(), bv.get(), bp.get(), ctx.get()) != 1 ||
        BN_mod_sub(bt.get(), bgb.get(), bkv.get(), bp.get(), ctx.get()) != 1)
        return TG_ERR_CRYPTO;
    // s_a = t^(a + u*x) mod p (показатель без редукции — как в протоколе)
    if (BN_mul(bexp.get(), bu.get(), bx.get(), ctx.get()) != 1 || BN_add(bexp.get(), bexp.get(), ba.get()) != 1)
        return TG_ERR_CRYPTO;
    BN_set_flags(bexp.get(), BN_FLG_CONSTTIME);
    uint8_t sa[256], ka[32];
    if (BN_mod_exp(bs.get(), bt.get(), bexp.get(), bp.get(), ctx.get()) != 1 || !to256(bs.get(), sa))
        return TG_ERR_CRYPTO;
    sha256(sa, 256, ka);
    OPENSSL_cleanse(sa, sizeof sa);

    // M1 = H(H(p) xor H(g) | H(salt1) | H(salt2) | g_a | g_b | k_a)
    uint8_t hp[32], hg[32], hs1[32], hs2[32], m1[32];
    sha256(p, 256, hp);
    sha256(g_pad, 256, hg);
    for (int i = 0; i < 32; ++i) hp[i] ^= hg[i];
    sha256(salt1, s1, hs1);
    sha256(salt2, s2, hs2);
    sha256_parts({{hp, 32}, {hs1, 32}, {hs2, 32}, {ga_pad, 256}, {gb_pad, 256}, {ka, 32}}, m1);
    OPENSSL_cleanse(ka, sizeof ka);

    tg_status st = make_buf(ga_pad, 256, a_out);
    if (st != TG_OK) return st;
    st = make_buf(m1, 32, m1_out);
    if (st != TG_OK) {
        tg_buf_free(*a_out);
        a_out->ptr = nullptr;
        a_out->len = 0;
    }
    return st;
}

}  // namespace tg

extern "C" tg_status tg_srp_check(const uint8_t *pw, size_t pw_len, const uint8_t *salt1, size_t s1,
                                  const uint8_t *salt2, size_t s2, uint32_t g, const uint8_t *p, size_t plen,
                                  const uint8_t *srp_b, size_t blen, tg_buf *a_out, tg_buf *m1_out) {
    TG_GUARD_BEGIN
    for (int attempt = 0; attempt < 8; ++attempt) {
        tg::Secure a(256);
        if (!tg::random_bytes(a.data(), 256)) return TG_ERR_CRYPTO;
        const tg_status st = tg::srp_check_with_a(pw, pw_len, salt1, s1, salt2, s2, g, p, plen, srp_b, blen,
                                                  a.data(), 100000, a_out, m1_out);
        if (st != TG_ERR_STATE) return st;
    }
    return TG_ERR_CRYPTO;
    TG_GUARD_END
}
