// Обфускация транспорта obfuscated2 (core.telegram.org/mtproto/mtproto-transports,
// «Transport obfuscation»).
//
//   init (64) — случайные байты; init[0] != 0xef; первые 4 байта не HEAD, POST, GET ,
//   OPTI, 0x16030102 (TLS), 0xdddddddd, 0xeeeeeeee; байты 4..8 не нули.
//   init[56..60] — тег транспорта; init[60..62] — dc_id int16 LE (если задан).
//   encryptKey = init[8..40],  encryptIV = init[40..56]
//   decryptKey = rev(init)[8..40], decryptIV = rev(init)[40..56]
//   с секретом MTProxy: key = SHA256(key + secret)
//   зашифрованный init; отправляется init[0..56] + enc(init)[56..64]
// Оба потока AES-256-CTR продолжаются на всё соединение.
#include "internal.h"

#include <openssl/evp.h>

struct tg_obfs {
    EVP_CIPHER_CTX *enc = nullptr;
    EVP_CIPHER_CTX *dec = nullptr;
    ~tg_obfs() {
        EVP_CIPHER_CTX_free(enc);
        EVP_CIPHER_CTX_free(dec);
    }
};

namespace tg {
namespace {

void crypt(EVP_CIPHER_CTX *c, uint8_t *data, size_t len) {
    // CTR: шифрование и расшифровка совпадают; обрабатываем кусками по INT_MAX.
    while (len > 0) {
        const int chunk = len > (1u << 30) ? (1 << 30) : static_cast<int>(len);
        int outl = 0;
        EVP_EncryptUpdate(c, data, &outl, data, chunk);
        data += chunk;
        len -= static_cast<size_t>(chunk);
    }
}

}  // namespace

bool obfs_init_ok(const uint8_t init[64]) {
    if (init[0] == 0xef) return false;
    static const uint8_t bad[][4] = {
        {'H', 'E', 'A', 'D'}, {'P', 'O', 'S', 'T'}, {'G', 'E', 'T', ' '}, {'O', 'P', 'T', 'I'},
        {0x16, 0x03, 0x01, 0x02}, {0xdd, 0xdd, 0xdd, 0xdd}, {0xee, 0xee, 0xee, 0xee},
    };
    for (const auto &b : bad)
        if (std::memcmp(init, b, 4) == 0) return false;
    static const uint8_t zero[4] = {};
    return std::memcmp(init + 4, zero, 4) != 0;
}

tg_status obfs_new_with_init(int transport, int dc_id, const uint8_t *secret, size_t slen,
                             const uint8_t init_random[64], tg_buf *init, tg_obfs **out) {
    if (init_random == nullptr || init == nullptr || out == nullptr) return TG_ERR_ARG;
    *out = nullptr;
    init->ptr = nullptr;
    init->len = 0;
    uint8_t tag;
    switch (transport) {
        case TG_TRANSPORT_ABRIDGED: tag = 0xef; break;
        case TG_TRANSPORT_INTERMEDIATE: tag = 0xee; break;
        case TG_TRANSPORT_PADDED: tag = 0xdd; break;
        default: return TG_ERR_ARG;
    }
    if (dc_id < -32768 || dc_id > 32767) return TG_ERR_ARG;
    if (slen != 0 && slen != 16 && slen != 17) return TG_ERR_ARG;
    if (slen != 0 && secret == nullptr) return TG_ERR_ARG;
    const uint8_t *sec = slen == 17 ? secret + 1 : secret;

    Secure buf(init_random, 64);
    uint8_t *ini = buf.data();
    std::memset(ini + 56, tag, 4);
    if (dc_id != 0) {
        const auto d = static_cast<uint16_t>(static_cast<int16_t>(dc_id));
        ini[60] = static_cast<uint8_t>(d);
        ini[61] = static_cast<uint8_t>(d >> 8);
    }
    if (!obfs_init_ok(ini)) return TG_ERR_ARG;

    uint8_t rev[64];
    for (int i = 0; i < 64; ++i) rev[i] = ini[63 - i];
    uint8_t ekey[32], dkey[32];
    if (slen != 0) {
        sha256_parts({{ini + 8, 32}, {sec, 16}}, ekey);
        sha256_parts({{rev + 8, 32}, {sec, 16}}, dkey);
    } else {
        std::memcpy(ekey, ini + 8, 32);
        std::memcpy(dkey, rev + 8, 32);
    }

    auto *o = new tg_obfs;
    o->enc = EVP_CIPHER_CTX_new();
    o->dec = EVP_CIPHER_CTX_new();
    const bool ok = o->enc != nullptr && o->dec != nullptr &&
                    EVP_EncryptInit_ex(o->enc, EVP_aes_256_ctr(), nullptr, ekey, ini + 40) == 1 &&
                    EVP_EncryptInit_ex(o->dec, EVP_aes_256_ctr(), nullptr, dkey, rev + 40) == 1;
    OPENSSL_cleanse(ekey, sizeof ekey);
    OPENSSL_cleanse(dkey, sizeof dkey);
    OPENSSL_cleanse(rev, sizeof rev);
    if (!ok) {
        delete o;
        return TG_ERR_CRYPTO;
    }
    uint8_t encrypted[64];
    std::memcpy(encrypted, ini, 64);
    crypt(o->enc, encrypted, 64);
    uint8_t final_init[64];
    std::memcpy(final_init, ini, 56);
    std::memcpy(final_init + 56, encrypted + 56, 8);
    const tg_status st = make_buf(final_init, 64, init);
    if (st != TG_OK) {
        delete o;
        return st;
    }
    *out = o;
    return TG_OK;
}

}  // namespace tg

extern "C" tg_status tg_obfs_new(int transport, int dc_id, const uint8_t *secret, size_t slen, tg_buf *init,
                                 tg_obfs **out) {
    TG_GUARD_BEGIN
    uint8_t rnd[64];
    for (int attempt = 0; attempt < 1000; ++attempt) {
        if (!tg::random_bytes(rnd, 56)) return TG_ERR_CRYPTO;
        std::memset(rnd + 56, 0, 8);
        if (!tg::random_bytes(rnd + 62, 2)) return TG_ERR_CRYPTO;
        if (!tg::obfs_init_ok(rnd)) continue;
        const tg_status st = tg::obfs_new_with_init(transport, dc_id, secret, slen, rnd, init, out);
        OPENSSL_cleanse(rnd, sizeof rnd);
        return st;
    }
    return TG_ERR_CRYPTO;
    TG_GUARD_END
}

extern "C" void tg_obfs_encrypt(tg_obfs *o, uint8_t *data, size_t len) {
    if (o == nullptr || data == nullptr) return;
    tg::crypt(o->enc, data, len);
}

extern "C" void tg_obfs_decrypt(tg_obfs *o, uint8_t *data, size_t len) {
    if (o == nullptr || data == nullptr) return;
    tg::crypt(o->dec, data, len);
}

extern "C" void tg_obfs_free(tg_obfs *o) { delete o; }
