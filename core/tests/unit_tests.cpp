// Модульные тесты крипто-ядра (веха В1). Сторона сервера там, где она нужна
// (DH, обфускация), смоделирована прямо здесь через OpenSSL. Независимая сверка
// с эталоном на Python — tests/reference.py.
#include "internal.h"
#include "tgcrypto.h"

#include <cstdio>
#include <cstdlib>
#include <functional>
#include <memory>
#include <string>
#include <vector>

#include <openssl/bn.h>
#include <openssl/evp.h>
#include <openssl/rand.h>

namespace {

int g_failures = 0;
int g_checks = 0;

#define CHECK(cond)                                                                     \
    do {                                                                                \
        ++g_checks;                                                                     \
        if (!(cond)) {                                                                  \
            (void)std::fprintf(stderr, "%s:%d: CHECK(%s) не выполнено\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                               \
        }                                                                               \
    } while (0)

using Bytes = std::vector<uint8_t>;

Bytes hex(const char *s) {
    Bytes out;
    for (size_t i = 0; s[i] != '\0' && s[i + 1] != '\0'; i += 2) {
        out.push_back(static_cast<uint8_t>(std::stoi(std::string(s + i, 2), nullptr, 16)));
    }
    return out;
}

Bytes rnd(size_t n) {
    Bytes b(n);
    if (n != 0) RAND_bytes(b.data(), static_cast<int>(n));
    return b;
}

uint32_t rnd_u32() {
    uint32_t v;
    RAND_bytes(reinterpret_cast<uint8_t *>(&v), sizeof v);
    return v;
}

Bytes take(tg_buf b) {
    Bytes out(b.ptr, b.ptr + b.len);
    tg_buf_free(b);
    return out;
}

struct BnDel {
    void operator()(BIGNUM *b) const { BN_free(b); }
};
using Bn = std::unique_ptr<BIGNUM, BnDel>;

// ---------------------------------------------------------------- общее

void test_common() {
    CHECK(std::string(tg_version()).rfind("tgcrypto ", 0) == 0);
    uint8_t h[32];
    const uint8_t abc[] = {'a', 'b', 'c'};
    CHECK(tg_sha256(abc, 3, h) == TG_OK);
    CHECK(Bytes(h, h + 32) == hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    CHECK(tg_sha256(nullptr, 1, h) == TG_ERR_ARG);
    uint8_t r1[32] = {}, r2[32] = {};
    CHECK(tg_random(r1, 32) == TG_OK && tg_random(r2, 32) == TG_OK);
    CHECK(std::memcmp(r1, r2, 32) != 0);
    tg_buf_free(tg_buf{nullptr, 0});
}

// ---------------------------------------------------------------- AES-IGE

void ecb_ige_ref(const Bytes &key, const Bytes &iv, const Bytes &in, Bytes &out, bool enc) {
    // Независимый эталон: IGE поверх одиночных блоков EVP AES-ECB.
    out.resize(in.size());
    EVP_CIPHER_CTX *c = EVP_CIPHER_CTX_new();
    EVP_CipherInit_ex(c, key.size() == 16 ? EVP_aes_128_ecb() : EVP_aes_256_ecb(), nullptr, key.data(), nullptr,
                      enc ? 1 : 0);
    EVP_CIPHER_CTX_set_padding(c, 0);
    uint8_t x[16], y[16], t[16];
    std::memcpy(x, iv.data(), 16);
    std::memcpy(y, iv.data() + 16, 16);
    for (size_t off = 0; off < in.size(); off += 16) {
        int l = 0;
        if (enc) {
            for (int i = 0; i < 16; ++i) t[i] = in[off + i] ^ x[i];
            EVP_CipherUpdate(c, t, &l, t, 16);
            for (int i = 0; i < 16; ++i) out[off + i] = t[i] ^ y[i];
            std::memcpy(x, &out[off], 16);
            std::memcpy(y, &in[off], 16);
        } else {
            for (int i = 0; i < 16; ++i) t[i] = in[off + i] ^ y[i];
            EVP_CipherUpdate(c, t, &l, t, 16);
            for (int i = 0; i < 16; ++i) out[off + i] = t[i] ^ x[i];
            std::memcpy(y, &out[off], 16);
            std::memcpy(x, &in[off], 16);
        }
    }
    EVP_CIPHER_CTX_free(c);
}

void test_ige_paths(bool fallback) {
    tg::aes_ige_force_fallback(fallback);
    // Векторы из статьи Бена Лори об IGE (они же в тестах OpenSSL igetest.c), AES-128.
    {
        const Bytes key = hex("000102030405060708090a0b0c0d0e0f");
        const Bytes iv = hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        const Bytes pt(32, 0);
        const Bytes ct = hex("1a8519a6557be652e9da8e43da4ef4453cf456b4ca488aa383c79c98b34797cb");
        Bytes out(32), back(32);
        CHECK(tg::aes_ige_encrypt(key.data(), 16, iv.data(), pt.data(), out.data(), 32));
        CHECK(out == ct);
        CHECK(tg::aes_ige_decrypt(key.data(), 16, iv.data(), ct.data(), back.data(), 32));
        CHECK(back == pt);
    }
    {
        const Bytes key = hex("5468697320697320616e20696d706c65");
        const Bytes iv = hex("6d656e746174696f6e206f6620494745206d6f646520666f72204f70656e5353");
        const Bytes pt = hex("99706487a1cde613bc6de0b6f24b1c7aa448c8b9c3403e3467a8cad89340f53b");
        const Bytes ct = hex("4c2e204c6574277320686f70652042656e20676f74206974207269676874210a");
        Bytes out(32), back(32);
        CHECK(tg::aes_ige_encrypt(key.data(), 16, iv.data(), pt.data(), out.data(), 32));
        CHECK(out == ct);
        CHECK(tg::aes_ige_decrypt(key.data(), 16, iv.data(), ct.data(), back.data(), 32));
        CHECK(back == pt);
    }
    // Случайные: совпадение с эталоном, обратимость, работа на месте.
    for (int i = 0; i < 300; ++i) {
        const size_t klen = (i % 2 == 0) ? 32 : 16;
        const Bytes key = rnd(klen), iv = rnd(32), pt = rnd(size_t{16} * (rnd_u32() % 64));
        Bytes ct(pt.size()), ref, back(pt.size());
        CHECK(tg::aes_ige_encrypt(key.data(), klen, iv.data(), pt.data(), ct.data(), pt.size()));
        ecb_ige_ref(key, iv, pt, ref, true);
        CHECK(ct == ref);
        CHECK(tg::aes_ige_decrypt(key.data(), klen, iv.data(), ct.data(), back.data(), ct.size()));
        CHECK(back == pt);
        Bytes inplace = pt;
        CHECK(tg::aes_ige_encrypt(key.data(), klen, iv.data(), inplace.data(), inplace.data(), inplace.size()));
        CHECK(inplace == ct);
        CHECK(tg::aes_ige_decrypt(key.data(), klen, iv.data(), inplace.data(), inplace.data(), inplace.size()));
        CHECK(inplace == pt);
    }
    // Неверные аргументы.
    const Bytes key = rnd(32), iv = rnd(32), pt = rnd(20);
    Bytes out(20);
    CHECK(!tg::aes_ige_encrypt(key.data(), 32, iv.data(), pt.data(), out.data(), 20));  // не кратно 16
    CHECK(!tg::aes_ige_encrypt(key.data(), 24, iv.data(), pt.data(), out.data(), 16));  // AES-192 не нужен
    tg::aes_ige_force_fallback(false);
}

// ---------------------------------------------------------------- ключ

std::unique_ptr<tg_key, void (*)(tg_key *)> make_key(const Bytes &ak) {
    return {tg::key_from_bytes(ak.data()), tg_key_free};
}

void test_key() {
    const Bytes ak = rnd(256);
    auto k = make_key(ak);
    uint8_t h[20];
    tg::sha1(ak.data(), 256, h);
    uint64_t id = 0;
    for (int i = 0; i < 8; ++i) id |= static_cast<uint64_t>(h[12 + i]) << (8 * i);
    CHECK(tg_key_id(k.get()) == id);

    const Bytes wrap = rnd(32);
    tg_buf sealed{};
    CHECK(tg_key_seal(k.get(), wrap.data(), &sealed) == TG_OK);
    Bytes s = take(sealed);
    CHECK(s.size() == 288);
    CHECK(std::memcmp(s.data(), "NKS1", 4) == 0);
    tg_key *back = nullptr;
    CHECK(tg_key_unseal(s.data(), s.size(), wrap.data(), &back) == TG_OK);
    CHECK(back != nullptr && tg_key_id(back) == id);
    CHECK(back != nullptr && std::memcmp(tg::key_bytes(back), ak.data(), 256) == 0);
    tg_key_free(back);

    // Чужой ключ обёртки и любая порча отвергаются.
    const Bytes other = rnd(32);
    back = nullptr;
    CHECK(tg_key_unseal(s.data(), s.size(), other.data(), &back) == TG_ERR_CRYPTO && back == nullptr);
    for (size_t pos : {size_t(4), size_t(10), size_t(20), size_t(200), size_t(287)}) {
        Bytes bad = s;
        bad[pos] ^= 0x01;
        CHECK(tg_key_unseal(bad.data(), bad.size(), wrap.data(), &back) == TG_ERR_CRYPTO);
    }
    Bytes badmagic = s;
    badmagic[0] = 'X';
    CHECK(tg_key_unseal(badmagic.data(), badmagic.size(), wrap.data(), &back) == TG_ERR_ARG);
    CHECK(tg_key_unseal(s.data(), s.size() - 1, wrap.data(), &back) == TG_ERR_ARG);
    // Два запечатывания одного ключа различаются (случайный nonce).
    tg_buf s2{};
    CHECK(tg_key_seal(k.get(), wrap.data(), &s2) == TG_OK);
    CHECK(take(s2) != s);
    CHECK(tg_key_id(nullptr) == 0);
}

// ---------------------------------------------------------------- конверт

void test_envelope() {
    const Bytes ak = rnd(256);
    auto k = make_key(ak);
    for (int i = 0; i < 300; ++i) {
        tg_env_hdr h{};
        RAND_bytes(reinterpret_cast<uint8_t *>(&h), sizeof h);
        const Bytes body = rnd(size_t{4} * (rnd_u32() % 300));
        // Клиент -> сервер: tg_env_encrypt, «сервер» открывает с x = 0.
        tg_buf wire{};
        CHECK(tg_env_encrypt(k.get(), &h, body.data(), body.size(), &wire) == TG_OK);
        Bytes w = take(wire);
        CHECK((w.size() - 24) % 16 == 0);
        CHECK(w.size() - 24 - 32 - body.size() >= 12 && w.size() - 24 - 32 - body.size() <= 1024);
        tg_env_hdr h2{};
        tg_buf out{};
        CHECK(tg::env_open(k.get(), 0, w.data(), w.size(), &h2, &out) == TG_OK);
        CHECK(take(out) == body);
        CHECK(h2.salt == h.salt && h2.session_id == h.session_id && h2.msg_id == h.msg_id && h2.seq_no == h.seq_no);
        // Направления не взаимозаменяемы: клиентское сообщение не открывается как серверное.
        CHECK(tg_env_decrypt(k.get(), w.data(), w.size(), &h2, &out) == TG_ERR_MSG_KEY);

        // Сервер -> клиент: «сервер» шифрует с x = 8, клиент открывает tg_env_decrypt.
        const size_t base = 32 + body.size();
        size_t pad = 12 + ((16 - (base + 12) % 16) % 16) + size_t{16} * (rnd_u32() % 8);
        const Bytes padding = rnd(pad);
        CHECK(tg::env_seal(k.get(), 8, &h, body.data(), body.size(), padding.data(), pad, &wire) == TG_OK);
        w = take(wire);
        CHECK(tg_env_decrypt(k.get(), w.data(), w.size(), &h2, &out) == TG_OK);
        CHECK(take(out) == body);
        CHECK(h2.msg_id == h.msg_id);

        // Любой изменённый байт отвергается.
        Bytes bad = w;
        const size_t pos = rnd_u32() % bad.size();
        bad[pos] ^= static_cast<uint8_t>(1u << (rnd_u32() % 8));
        const tg_status st = tg_env_decrypt(k.get(), bad.data(), bad.size(), &h2, &out);
        CHECK(st != TG_OK);
        CHECK(out.ptr == nullptr);
    }
    // Чужой ключ: не тот auth_key_id.
    {
        auto k2 = make_key(rnd(256));
        tg_env_hdr h{};
        const Bytes body = rnd(16);
        tg_buf wire{}, out{};
        const Bytes pad = rnd(16);
        CHECK(tg::env_seal(k.get(), 8, &h, body.data(), 16, pad.data(), 16, &wire) == TG_OK);
        Bytes w = take(wire);
        CHECK(tg_env_decrypt(k2.get(), w.data(), w.size(), &h, &out) == TG_ERR_MSG_KEY);
        CHECK(tg_env_decrypt(k.get(), w.data(), w.size() - 16, &h, &out) != TG_OK);  // укорочено
        CHECK(tg_env_decrypt(k.get(), w.data(), 23, &h, &out) == TG_ERR_ARG);
    }
    // Проверки формата при шифровании.
    {
        tg_env_hdr h{};
        const Bytes body = rnd(6);
        tg_buf wire{};
        CHECK(tg_env_encrypt(k.get(), &h, body.data(), 6, &wire) == TG_ERR_ARG);  // не кратно 4
        const Bytes b16 = rnd(16), pad8 = rnd(8), pad1040 = rnd(1040);
        CHECK(tg::env_seal(k.get(), 8, &h, b16.data(), 16, pad8.data(), 8, &wire) == TG_ERR_ARG);        // < 12
        CHECK(tg::env_seal(k.get(), 8, &h, b16.data(), 16, pad1040.data(), 1040, &wire) == TG_ERR_ARG);  // > 1024
        CHECK(tg_env_encrypt(nullptr, &h, b16.data(), 16, &wire) == TG_ERR_ARG);
    }
    // Разбор открытого текста: заявленная длина тела не может выйти за буфер.
    {
        Bytes pt(64, 0);
        tg_env_hdr h{};
        size_t off = 0, n = 0;
        pt[28] = 20;  // тело 20, дополнение 12
        CHECK(tg::env_parse_plaintext(pt.data(), 64, &h, &off, &n) == TG_OK && off == 32 && n == 20);
        pt[28] = 24;  // дополнение 8 < 12
        CHECK(tg::env_parse_plaintext(pt.data(), 64, &h, &off, &n) == TG_ERR_ARG);
        pt[28] = 0xff; pt[29] = 0xff; pt[30] = 0xff; pt[31] = 0xff;
        CHECK(tg::env_parse_plaintext(pt.data(), 64, &h, &off, &n) == TG_ERR_ARG);
        pt[28] = 18; pt[29] = pt[30] = pt[31] = 0;  // не кратно 4
        CHECK(tg::env_parse_plaintext(pt.data(), 64, &h, &off, &n) == TG_ERR_ARG);
        Bytes big(32 + 4 + 1040, 0);  // дополнение больше 1024
        big[28] = 4;
        CHECK(tg::env_parse_plaintext(big.data(), big.size(), &h, &off, &n) == TG_ERR_ARG);
    }
}

// ---------------------------------------------------------------- RSA и pq

void test_rsa_pq() {
    CHECK(tg_rsa_has_key(0xd09d1d85de64fd85ULL) == 1);
    CHECK(tg_rsa_has_key(0xb25898df208d2603ULL) == 1);
    CHECK(tg_rsa_has_key(0x1234) == 0);
    // Отпечаток, вычисленный ядром при регистрации, совпадает с опубликованным.
    {
        uint64_t fp = 0;
        CHECK(tg_rsa_add_key(tg::rsa_key_modulus(0xd09d1d85de64fd85ULL), 256, 65537, &fp) == TG_OK);
        CHECK(fp == 0xd09d1d85de64fd85ULL);
        CHECK(tg_rsa_add_key(tg::rsa_key_modulus(0xb25898df208d2603ULL), 256, 65537, &fp) == TG_OK);
        CHECK(fp == 0xb25898df208d2603ULL);
        // Новый ключ регистрируется и становится доступен RSA_PAD.
        Bytes n = rnd(256);
        n[0] |= 0x80;
        CHECK(tg_rsa_add_key(n.data(), 256, 65537, &fp) == TG_OK);
        CHECK(tg_rsa_has_key(fp) == 1);
        tg_buf enc{};
        const Bytes d = rnd(50);
        const tg_status st = tg_rsa_pad_encrypt(d.data(), d.size(), fp, &enc);
        CHECK(st == TG_OK);
        if (st == TG_OK) CHECK(take(enc).size() == 256);
        Bytes small = n;
        small[0] = 0x10;  // меньше 2048 бит
        CHECK(tg_rsa_add_key(small.data(), 256, 65537, &fp) == TG_ERR_ARG);
        CHECK(tg_rsa_add_key(n.data(), 256, 4, &fp) == TG_ERR_ARG);
    }
    const Bytes data = rnd(144);
    tg_buf out{};
    CHECK(tg_rsa_pad_encrypt(data.data(), 144, 0xd09d1d85de64fd85ULL, &out) == TG_OK);
    CHECK(take(out).size() == 256);
    const Bytes too_long = rnd(145);
    CHECK(tg_rsa_pad_encrypt(too_long.data(), 145, 0xd09d1d85de64fd85ULL, &out) == TG_ERR_ARG);
    CHECK(tg_rsa_pad_encrypt(data.data(), 144, 0x1234, &out) == TG_ERR_ARG);
    // Две попытки на одних данных различаются (случайный temp_key и дополнение).
    tg_buf o1{}, o2{};
    CHECK(tg_rsa_pad_encrypt(data.data(), 100, 0xd09d1d85de64fd85ULL, &o1) == TG_OK);
    CHECK(tg_rsa_pad_encrypt(data.data(), 100, 0xd09d1d85de64fd85ULL, &o2) == TG_OK);
    CHECK(take(o1) != take(o2));

    // pq: пример из документации.
    uint32_t p = 0, q = 0;
    CHECK(tg_pq_factorize(0x17ED48941A08F981ULL, &p, &q) == TG_OK);
    CHECK(p == 0x494C553Bu && q == 0x53911073u);
    // Случайные произведения простых 31–32 бит.
    for (int i = 0; i < 200; ++i) {
        Bn a(BN_new()), b(BN_new());
        BN_generate_prime_ex(a.get(), 31 + (i % 2), 0, nullptr, nullptr, nullptr);
        BN_generate_prime_ex(b.get(), 31 + ((i / 2) % 2), 0, nullptr, nullptr, nullptr);
        const uint64_t pa = BN_get_word(a.get()), pb = BN_get_word(b.get());
        CHECK(tg_pq_factorize(pa * pb, &p, &q) == TG_OK);
        CHECK(p == (pa < pb ? pa : pb) && q == (pa < pb ? pb : pa));
    }
    // Простое число и слишком малые входы разложить нельзя, но и зависнуть нельзя.
    CHECK(tg_pq_factorize(0xFFFFFFFFFFFFFFC5ULL, &p, &q) == TG_ERR_ARG);  // 2^64 - 59, простое
    CHECK(tg_pq_factorize(3, &p, &q) == TG_ERR_ARG);
    CHECK(tg_pq_factorize(15, nullptr, &q) == TG_ERR_ARG);
}

// ---------------------------------------------------------------- DH

Bytes modexp(uint32_t g, const Bytes &e, const Bytes &p) {
    BN_CTX *ctx = BN_CTX_new();
    Bn bg(BN_new()), be(BN_bin2bn(e.data(), static_cast<int>(e.size()), nullptr)),
        bp(BN_bin2bn(p.data(), static_cast<int>(p.size()), nullptr)), r(BN_new());
    BN_set_word(bg.get(), g);
    BN_mod_exp(r.get(), bg.get(), be.get(), bp.get(), ctx);
    Bytes out(256);
    BN_bn2binpad(r.get(), out.data(), 256);
    BN_CTX_free(ctx);
    return out;
}

Bytes modexp_b(const Bytes &base, const Bytes &e, const Bytes &p) {
    BN_CTX *ctx = BN_CTX_new();
    Bn bb(BN_bin2bn(base.data(), static_cast<int>(base.size()), nullptr)),
        be(BN_bin2bn(e.data(), static_cast<int>(e.size()), nullptr)),
        bp(BN_bin2bn(p.data(), static_cast<int>(p.size()), nullptr)), r(BN_new());
    BN_mod_exp(r.get(), bb.get(), be.get(), bp.get(), ctx);
    Bytes out(256);
    BN_bn2binpad(r.get(), out.data(), 256);
    BN_CTX_free(ctx);
    return out;
}

void test_dh() {
    const Bytes p(tg::dh_known_prime(), tg::dh_known_prime() + 256);
    // Встроенное простое действительно безопасное (проверка без кэша ядра).
    {
        BN_CTX *ctx = BN_CTX_new();
        Bn bp(BN_bin2bn(p.data(), 256, nullptr)), q(BN_dup(bp.get()));
        BN_sub_word(q.get(), 1);
        BN_rshift1(q.get(), q.get());
        CHECK(BN_check_prime(bp.get(), ctx, nullptr) == 1);
        CHECK(BN_check_prime(q.get(), ctx, nullptr) == 1);
        CHECK(BN_num_bits(bp.get()) == 2048);
        BN_CTX_free(ctx);
    }
    // Условия на g из спецификации для встроенного простого, посчитанные здесь независимо.
    {
        Bn bp(BN_bin2bn(p.data(), 256, nullptr));
        const auto m = [&](BN_ULONG w) { return BN_mod_word(bp.get(), w); };
        const bool expect[8] = {false, false, m(8) == 7, m(3) == 2, true, m(5) == 1 || m(5) == 4,
                                m(24) == 19 || m(24) == 23, m(7) == 3 || m(7) == 5 || m(7) == 6};
        for (uint32_t gen = 2; gen <= 7; ++gen)
            CHECK((tg::dh_check_params(p.data(), 256, gen) == TG_OK) == expect[gen]);
        CHECK(expect[3] && !expect[2]);  // p mod 3 = 2, p mod 8 = 3: Telegram присылает g = 3
    }
    CHECK(tg::dh_check_params(p.data(), 256, 4) == TG_OK);
    CHECK(tg::dh_check_params(p.data(), 256, 1) == TG_ERR_DH);
    CHECK(tg::dh_check_params(p.data(), 256, 8) == TG_ERR_DH);
    CHECK(tg::dh_check_params(p.data(), 255, 4) == TG_ERR_DH);
    // Не простое: p + 2 (чётность та же) при g = 4 (условие на p mod нет) — ловит проверка простоты.
    {
        Bytes np = p;
        np[255] += 2;
        CHECK(tg::dh_check_params(np.data(), 256, 4) == TG_ERR_DH);
    }

    for (int round = 0; round < 20; ++round) {
        const Bytes new_nonce = rnd(32), server_nonce = rnd(16);
        tg_authgen *g = nullptr;
        CHECK(tg_authgen_new(new_nonce.data(), server_nonce.data(), &g) == TG_OK);
        // Сервер: answer_with_hash под временным ключом; клиент расшифровывает.
        const Bytes answer = rnd(size_t{4} * (100 + rnd_u32() % 200));
        tg_buf enc{};
        CHECK(tg_authgen_encrypt_dh(g, answer.data(), answer.size(), &enc) == TG_OK);
        Bytes e = take(enc);
        CHECK(e.size() % 16 == 0);
        tg_buf inner{};
        CHECK(tg_authgen_decrypt_dh(g, e.data(), e.size(), &inner) == TG_OK);
        CHECK(take(inner) == answer);
        Bytes bad = e;
        bad[e.size() / 2] ^= 0x80;
        CHECK(tg_authgen_decrypt_dh(g, bad.data(), bad.size(), &inner) == TG_ERR_CRYPTO);
        CHECK(tg_authgen_decrypt_dh(g, e.data(), e.size() - 1, &inner) == TG_ERR_ARG);

        // До DH хэшей нет.
        uint8_t nh[16];
        CHECK(tg_authgen_nonce_hash(g, 1, nh) == TG_ERR_ARG);
        tg_key *early = nullptr;
        CHECK(tg_authgen_finish(g, &early) == TG_ERR_STATE);

        // Сервер: a, g_a = 3^a mod p. Клиент: g_b; сервер: auth_key = g_b^a.
        const uint32_t gen = 3;
        const Bytes a = rnd(256);
        const Bytes g_a = modexp(gen, a, p);
        tg_buf gb{};
        CHECK(tg_authgen_dh(g, p.data(), 256, gen, g_a.data(), 256, &gb) == TG_OK);
        const Bytes g_b = take(gb);
        CHECK(g_b.size() == 256);
        CHECK(tg::dh_check_value(g_b.data(), 256, p.data(), 256, true));
        const Bytes server_key = modexp_b(g_b, a, p);

        // new_nonce_hash{1,2,3} по формуле из спецификации.
        uint8_t kh[20];
        tg::sha1(server_key.data(), 256, kh);
        for (int n = 1; n <= 3; ++n) {
            uint8_t expect[20];
            const uint8_t nb = static_cast<uint8_t>(n);
            tg::sha1_parts({{new_nonce.data(), 32}, {&nb, 1}, {kh, 8}}, expect);
            CHECK(tg_authgen_nonce_hash(g, n, nh) == TG_OK);
            CHECK(std::memcmp(nh, expect + 4, 16) == 0);
        }
        CHECK(tg_authgen_nonce_hash(g, 4, nh) == TG_ERR_ARG);
        uint64_t aux = 0, expect_aux = 0;
        for (int i = 0; i < 8; ++i) expect_aux |= static_cast<uint64_t>(kh[i]) << (8 * i);
        CHECK(tg_authgen_aux_hash(g, &aux) == TG_OK && aux == expect_aux);

        tg_key *key = nullptr;
        CHECK(tg_authgen_finish(g, &key) == TG_OK);
        CHECK(key != nullptr && std::memcmp(tg::key_bytes(key), server_key.data(), 256) == 0);
        tg_key *again = nullptr;
        CHECK(tg_authgen_finish(g, &again) == TG_ERR_ARG);
        tg_key_free(key);
        tg_authgen_free(g);
    }
    // Плохие g_a отвергаются.
    {
        const Bytes new_nonce = rnd(32), server_nonce = rnd(16);
        tg_authgen *g = nullptr;
        CHECK(tg_authgen_new(new_nonce.data(), server_nonce.data(), &g) == TG_OK);
        tg_buf gb{};
        Bytes one(256, 0);
        one[255] = 1;
        CHECK(tg_authgen_dh(g, p.data(), 256, 3, one.data(), 256, &gb) == TG_ERR_DH);
        Bytes small(256, 0);
        small[200] = 1;  // < 2^{2048-64}
        CHECK(tg_authgen_dh(g, p.data(), 256, 3, small.data(), 256, &gb) == TG_ERR_DH);
        Bytes pm1 = p;
        pm1[255] -= 1;
        CHECK(tg_authgen_dh(g, p.data(), 256, 3, pm1.data(), 256, &gb) == TG_ERR_DH);
        CHECK(tg_authgen_dh(g, p.data(), 256, 3, p.data(), 256, &gb) == TG_ERR_DH);
        CHECK(tg_authgen_dh(g, p.data(), 256, 9, one.data(), 256, &gb) == TG_ERR_DH);
        tg_authgen_free(g);
    }
    CHECK(tg_authgen_new(nullptr, nullptr, nullptr) == TG_ERR_ARG);
}

// ---------------------------------------------------------------- SRP

void test_srp() {
    const Bytes p(tg::dh_known_prime(), tg::dh_known_prime() + 256);
    const Bytes pw = {'s', 'e', 'c', 'r', 'e', 't'};
    const Bytes s1 = rnd(40), s2 = rnd(16);
    const Bytes srp_b = modexp(3, rnd(256), p);  // правдоподобное g_b
    const Bytes a = rnd(256);
    tg_buf A1{}, M1{}, A2{}, M2{};
    // Быстрый вариант с одной итерацией PBKDF2 — детерминированность при заданном a.
    CHECK(tg::srp_check_with_a(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 4, p.data(), 256,
                               srp_b.data(), 256, a.data(), 1, &A1, &M1) == TG_OK);
    CHECK(tg::srp_check_with_a(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 4, p.data(), 256,
                               srp_b.data(), 256, a.data(), 1, &A2, &M2) == TG_OK);
    const Bytes a1 = take(A1), m1 = take(M1), a2 = take(A2), m2 = take(M2);
    CHECK(a1 == a2 && m1 == m2 && a1.size() == 256 && m1.size() == 32);
    CHECK(a1 == modexp(4, a, p));  // A = g^a mod p
    // Настоящий вызов (100000 итераций) даёт выход нужного вида.
    tg_buf A{}, M{};
    CHECK(tg_srp_check(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 4, p.data(), 256,
                       srp_b.data(), 256, &A, &M) == TG_OK);
    CHECK(take(A).size() == 256 && take(M).size() == 32);
    // Плохие параметры.
    Bytes zero(256, 0);
    CHECK(tg_srp_check(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 4, p.data(), 256,
                       zero.data(), 256, &A, &M) == TG_ERR_DH);
    CHECK(tg_srp_check(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 4, p.data(), 256,
                       p.data(), 256, &A, &M) == TG_ERR_DH);
    CHECK(tg_srp_check(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 11, p.data(), 256,
                       srp_b.data(), 256, &A, &M) == TG_ERR_DH);
    CHECK(tg_srp_check(pw.data(), pw.size(), s1.data(), s1.size(), s2.data(), s2.size(), 4, p.data(), 128,
                       srp_b.data(), 256, &A, &M) == TG_ERR_DH);
}

// ---------------------------------------------------------------- обфускация

struct Ctr {
    EVP_CIPHER_CTX *c;
    Ctr(const uint8_t *key, const uint8_t *iv) : c(EVP_CIPHER_CTX_new()) {
        EVP_EncryptInit_ex(c, EVP_aes_256_ctr(), nullptr, key, iv);
    }
    ~Ctr() { EVP_CIPHER_CTX_free(c); }
    void run(uint8_t *d, size_t n) {
        int l = 0;
        EVP_EncryptUpdate(c, d, &l, d, static_cast<int>(n));
    }
};

void test_obfs_case(int transport, int dc, const Bytes &secret) {
    tg_buf init{};
    tg_obfs *o = nullptr;
    CHECK(tg_obfs_new(transport, dc, secret.empty() ? nullptr : secret.data(), secret.size(), &init, &o) == TG_OK);
    Bytes fi = take(init);
    CHECK(fi.size() == 64);
    CHECK(tg::obfs_init_ok(fi.data()));

    // Сервер: ключ приёма = ключ передачи клиента (из байт 8..56 init).
    const uint8_t *sec = secret.size() == 17 ? secret.data() + 1 : secret.data();
    uint8_t skey_in[32];
    if (secret.empty()) std::memcpy(skey_in, fi.data() + 8, 32);
    else tg::sha256_parts({{fi.data() + 8, 32}, {sec, 16}}, skey_in);
    Ctr srv_in(skey_in, fi.data() + 40);
    Bytes dec = fi;
    srv_in.run(dec.data(), 64);
    const uint8_t tag = transport == 0 ? 0xef : transport == 1 ? 0xee : 0xdd;
    CHECK(dec[56] == tag && dec[57] == tag && dec[58] == tag && dec[59] == tag);
    if (dc != 0) {
        const auto d = static_cast<uint16_t>(static_cast<int16_t>(dc));
        CHECK(dec[60] == static_cast<uint8_t>(d) && dec[61] == static_cast<uint8_t>(d >> 8));
    }
    // Восстановленный полный init сервера: 0..56 открыто, 56..64 расшифровано.
    Bytes full(fi.begin(), fi.begin() + 56);
    full.insert(full.end(), dec.begin() + 56, dec.end());
    Bytes rev(full.rbegin(), full.rend());
    uint8_t skey_out[32];
    if (secret.empty()) std::memcpy(skey_out, rev.data() + 8, 32);
    else tg::sha256_parts({{rev.data() + 8, 32}, {sec, 16}}, skey_out);
    Ctr srv_out(skey_out, rev.data() + 40);

    for (int i = 0; i < 20; ++i) {
        const Bytes msg = rnd(1 + rnd_u32() % 3000);
        Bytes c2s = msg;
        tg_obfs_encrypt(o, c2s.data(), c2s.size());
        CHECK(c2s != msg);
        srv_in.run(c2s.data(), c2s.size());
        CHECK(c2s == msg);
        Bytes s2c = msg;
        srv_out.run(s2c.data(), s2c.size());
        tg_obfs_decrypt(o, s2c.data(), s2c.size());
        CHECK(s2c == msg);
    }
    tg_obfs_free(o);
}

void test_obfs() {
    for (int t = 0; t < 3; ++t) {
        test_obfs_case(t, 0, {});
        test_obfs_case(t, 2, rnd(16));
        test_obfs_case(t, -4, rnd(16));
        Bytes s17 = rnd(17);
        s17[0] = 0xdd;
        test_obfs_case(t, 10002, s17);
    }
    // Запрещённые начала init.
    uint8_t init[64];
    std::memset(init, 0x11, 64);
    CHECK(tg::obfs_init_ok(init));
    init[0] = 0xef;
    CHECK(!tg::obfs_init_ok(init));
    init[0] = 'P'; init[1] = 'O'; init[2] = 'S'; init[3] = 'T';
    CHECK(!tg::obfs_init_ok(init));
    std::memset(init, 0x11, 64);
    std::memset(init + 4, 0, 4);
    CHECK(!tg::obfs_init_ok(init));
    tg_buf b{};
    tg_obfs *o = nullptr;
    CHECK(tg_obfs_new(7, 0, nullptr, 0, &b, &o) == TG_ERR_ARG);
    const Bytes s15 = rnd(15);
    CHECK(tg_obfs_new(0, 0, s15.data(), 15, &b, &o) == TG_ERR_ARG);
    CHECK(tg_obfs_new(0, 40000, nullptr, 0, &b, &o) == TG_ERR_ARG);
    tg_obfs_encrypt(nullptr, nullptr, 10);  // не падает
    tg_obfs_free(nullptr);
}

}  // namespace

int main() {
    test_common();
    const bool ni = tg::aes_ige_uses_aesni();
    std::printf("AES-NI: %s\n", ni ? "да" : "нет");
    test_ige_paths(false);
    test_ige_paths(true);  // запасной путь через EVP
    test_key();
    test_envelope();
    test_rsa_pq();
    test_dh();
    test_srp();
    test_obfs();
    std::printf("проверок: %d, ошибок: %d\n", g_checks, g_failures);
    return g_failures == 0 ? 0 : 1;
}
