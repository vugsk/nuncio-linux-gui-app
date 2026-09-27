// Конверт MTProto 2.0 (core.telegram.org/mtproto/description).
//
//   plaintext = salt(8) session_id(8) msg_id(8) seq_no(4) len(4) body padding(12..1024)
//   msg_key_large = SHA256(substr(auth_key, 88+x, 32) + plaintext)
//   msg_key       = substr(msg_key_large, 8, 16)
//   sha256_a = SHA256(msg_key + substr(auth_key, x, 36))
//   sha256_b = SHA256(substr(auth_key, 40+x, 36) + msg_key)
//   aes_key  = a[0..8] + b[8..24] + a[24..32]
//   aes_iv   = b[0..8] + a[8..24] + b[24..32]
//   wire     = auth_key_id(8) + msg_key(16) + AES256_IGE(plaintext, aes_key, aes_iv)
// x = 0 — клиент -> сервер, x = 8 — сервер -> клиент.
#include "internal.h"

namespace tg {
namespace {

constexpr size_t kHeader = 32;       // salt, session_id, msg_id, seq_no, len
constexpr size_t kMinPad = 12;
constexpr size_t kMaxPad = 1024;
constexpr size_t kMaxBody = 1u << 26;  // 64 МиБ: защита от огромных выделений

void put_u64(uint8_t *p, uint64_t v) {
    for (int i = 0; i < 8; ++i) p[i] = static_cast<uint8_t>(v >> (8 * i));
}
void put_u32(uint8_t *p, uint32_t v) {
    for (int i = 0; i < 4; ++i) p[i] = static_cast<uint8_t>(v >> (8 * i));
}
uint64_t get_u64(const uint8_t *p) {
    uint64_t v = 0;
    for (int i = 0; i < 8; ++i) v |= static_cast<uint64_t>(p[i]) << (8 * i);
    return v;
}
uint32_t get_u32(const uint8_t *p) {
    uint32_t v = 0;
    for (int i = 0; i < 4; ++i) v |= static_cast<uint32_t>(p[i]) << (8 * i);
    return v;
}

void msg_key_of(const uint8_t *auth_key, int x, const uint8_t *pt, size_t len, uint8_t msg_key[16]) {
    uint8_t large[32];
    sha256_parts({{auth_key + 88 + x, 32}, {pt, len}}, large);
    std::memcpy(msg_key, large + 8, 16);
    OPENSSL_cleanse(large, sizeof large);
}

void derive(const uint8_t *auth_key, int x, const uint8_t msg_key[16], uint8_t key[32], uint8_t iv[32]) {
    uint8_t a[32], b[32];
    sha256_parts({{msg_key, 16}, {auth_key + x, 36}}, a);
    sha256_parts({{auth_key + 40 + x, 36}, {msg_key, 16}}, b);
    std::memcpy(key, a, 8);
    std::memcpy(key + 8, b + 8, 16);
    std::memcpy(key + 24, a + 24, 8);
    std::memcpy(iv, b, 8);
    std::memcpy(iv + 8, a + 8, 16);
    std::memcpy(iv + 24, b + 24, 8);
    OPENSSL_cleanse(a, sizeof a);
    OPENSSL_cleanse(b, sizeof b);
}

}  // namespace

tg_status env_parse_plaintext(const uint8_t *pt, size_t len, tg_env_hdr *h, size_t *body_off, size_t *body_len) {
    if (pt == nullptr || h == nullptr || body_off == nullptr || body_len == nullptr) return TG_ERR_ARG;
    if (len < kHeader + kMinPad || len % 16 != 0) return TG_ERR_ARG;
    const uint32_t n = get_u32(pt + 28);
    if (n % 4 != 0 || n > len - kHeader - kMinPad || n > kMaxBody) return TG_ERR_ARG;
    const size_t pad = len - kHeader - n;
    if (pad < kMinPad || pad > kMaxPad) return TG_ERR_ARG;
    h->salt = get_u64(pt);
    h->session_id = get_u64(pt + 8);
    h->msg_id = get_u64(pt + 16);
    h->seq_no = get_u32(pt + 24);
    *body_off = kHeader;
    *body_len = n;
    return TG_OK;
}

tg_status env_seal(const tg_key *k, int x, const tg_env_hdr *h, const uint8_t *body, size_t len,
                   const uint8_t *padding, size_t pad_len, tg_buf *out) {
    if (k == nullptr || h == nullptr || out == nullptr || (body == nullptr && len != 0)) return TG_ERR_ARG;
    if (x != 0 && x != 8) return TG_ERR_ARG;
    if (len % 4 != 0 || len > kMaxBody) return TG_ERR_ARG;
    if (pad_len < kMinPad || pad_len > kMaxPad || (kHeader + len + pad_len) % 16 != 0) return TG_ERR_ARG;
    if (padding == nullptr) return TG_ERR_ARG;
    const uint8_t *ak = key_bytes(k);

    const size_t pt_len = kHeader + len + pad_len;
    Secure pt(pt_len);
    put_u64(pt.data(), h->salt);
    put_u64(pt.data() + 8, h->session_id);
    put_u64(pt.data() + 16, h->msg_id);
    put_u32(pt.data() + 24, h->seq_no);
    put_u32(pt.data() + 28, static_cast<uint32_t>(len));
    if (len != 0) std::memcpy(pt.data() + kHeader, body, len);
    std::memcpy(pt.data() + kHeader + len, padding, pad_len);

    Secure wire(24 + pt_len);
    put_u64(wire.data(), tg_key_id(k));
    msg_key_of(ak, x, pt.data(), pt_len, wire.data() + 8);
    uint8_t key[32], iv[32];
    derive(ak, x, wire.data() + 8, key, iv);
    const bool ok = aes_ige_encrypt(key, 32, iv, pt.data(), wire.data() + 24, pt_len);
    OPENSSL_cleanse(key, sizeof key);
    OPENSSL_cleanse(iv, sizeof iv);
    if (!ok) return TG_ERR_CRYPTO;
    return make_buf(wire.data(), wire.size(), out);
}

tg_status env_open(const tg_key *k, int x, const uint8_t *in, size_t len, tg_env_hdr *h, tg_buf *body) {
    if (k == nullptr || in == nullptr || h == nullptr || body == nullptr) return TG_ERR_ARG;
    if (x != 0 && x != 8) return TG_ERR_ARG;
    body->ptr = nullptr;
    body->len = 0;
    if (len < 24 + kHeader + kMinPad || (len - 24) % 16 != 0) return TG_ERR_ARG;
    if (get_u64(in) != tg_key_id(k)) return TG_ERR_MSG_KEY;
    const uint8_t *ak = key_bytes(k);
    const uint8_t *msg_key = in + 8;

    uint8_t key[32], iv[32];
    derive(ak, x, msg_key, key, iv);
    const size_t pt_len = len - 24;
    Secure pt(pt_len);
    const bool ok = aes_ige_decrypt(key, 32, iv, in + 24, pt.data(), pt_len);
    OPENSSL_cleanse(key, sizeof key);
    OPENSSL_cleanse(iv, sizeof iv);
    if (!ok) return TG_ERR_CRYPTO;

    uint8_t check[16];
    msg_key_of(ak, x, pt.data(), pt_len, check);
    const bool same = CRYPTO_memcmp(check, msg_key, 16) == 0;
    OPENSSL_cleanse(check, sizeof check);
    if (!same) return TG_ERR_MSG_KEY;

    tg_env_hdr hdr{};
    size_t off = 0, n = 0;
    const tg_status st = env_parse_plaintext(pt.data(), pt_len, &hdr, &off, &n);
    if (st != TG_OK) return st;
    const tg_status bst = make_buf(pt.data() + off, n, body);
    if (bst == TG_OK) *h = hdr;
    return bst;
}

}  // namespace tg

extern "C" tg_status tg_env_encrypt(const tg_key *k, const tg_env_hdr *h, const uint8_t *body, size_t len,
                                    tg_buf *out) {
    TG_GUARD_BEGIN
    if (len % 4 != 0) return TG_ERR_ARG;
    // Случайное дополнение 12..1024 байт с кратностью 16 всего открытого текста.
    uint8_t r[2];
    if (!tg::random_bytes(r, 2)) return TG_ERR_CRYPTO;
    const size_t base = 32 + len;
    size_t pad = 12 + ((16 - (base + 12) % 16) % 16);         // минимальное допустимое
    const size_t extra = (static_cast<size_t>(r[0]) % 4) * 16;  // 0..48 байт сверху, не больше 1024
    if (pad + extra <= 1024) pad += extra;
    tg::Secure padding(pad);
    if (!tg::random_bytes(padding.data(), pad)) return TG_ERR_CRYPTO;
    OPENSSL_cleanse(r, sizeof r);
    return tg::env_seal(k, 0, h, body, len, padding.data(), pad, out);
    TG_GUARD_END
}

extern "C" tg_status tg_env_decrypt(const tg_key *k, const uint8_t *in, size_t len, tg_env_hdr *h, tg_buf *body) {
    TG_GUARD_BEGIN
    return tg::env_open(k, 8, in, len, h, body);
    TG_GUARD_END
}
