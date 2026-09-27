#ifndef NUNCIO_TGCRYPTO_H
#define NUNCIO_TGCRYPTO_H

/*
 * Публичный C ABI крипто-ядра nuncio. Источник истины — эскиз в
 * Архитектура/nuncio - Границы и C ABI.md (стык 2); отличия от эскиза
 * помечены «дополнение В1».
 *
 * Правила:
 *  - видимы только tg_*-символы (-fvisibility=hidden);
 *  - выходные tg_buf выдаёт ядро, освобождает tg_buf_free; хэндлы — своей парой _free;
 *  - входные буферы ядро не сохраняет; исключения C++ границу не пересекают;
 *  - ядро не разбирает TL (О-2): на вход и выход — непрозрачные байты;
 *  - ядро вызывает только Go (О-1);
 *  - ключевой материал стирается при освобождении.
 *
 * Потоки: функции безопасны для разных хэндлов; tg_key неизменяем и может
 * разделяться между потоками; tg_obfs — шифрование и расшифровка независимы
 * (писатель и читатель не пересекаются); tg_authgen — один владелец.
 */

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define TG_API __attribute__((visibility("default")))

typedef struct {
    uint8_t *ptr;
    size_t len;
} tg_buf;

typedef enum {
    TG_OK = 0,
    TG_ERR_ARG,      /* неверные аргументы или вход не прошёл проверки формата */
    TG_ERR_NOMEM,
    TG_ERR_CRYPTO,   /* ошибка примитива или не сошлась проверка целостности */
    TG_ERR_MSG_KEY,  /* конверт: не тот auth_key_id или не сошёлся msg_key */
    TG_ERR_DH,       /* параметры DH/SRP не прошли проверки безопасности */
    TG_ERR_STATE     /* вызов не в том состоянии хэндла */
} tg_status;

/* ---- Общее ---- */

/* Строка вида "tgcrypto 0.2.0"; статическая, освобождать не нужно. */
TG_API const char *tg_version(void);
/* Стирает и освобождает буфер, выданный ядром. Безопасна для {NULL, 0}. */
TG_API void tg_buf_free(tg_buf b);
TG_API tg_status tg_sha256(const uint8_t *in, size_t len, uint8_t out[32]);
/* Криптостойкий генератор (RAND_bytes). */
TG_API tg_status tg_random(uint8_t *out, size_t len);

/* ---- Ключ авторизации ----
 * Байты auth_key наружу не отдаются; только идентификатор и запечатанный вид
 * для файла сессии (AES-256-GCM ключом обёртки из Secret Service, О-6). */
typedef struct tg_key tg_key;

/* auth_key_id: 64 младших бита SHA1(auth_key), в порядке Telegram (little-endian). */
TG_API uint64_t tg_key_id(const tg_key *k);
TG_API tg_status tg_key_seal(const tg_key *k, const uint8_t wrap[32], tg_buf *out);
TG_API tg_status tg_key_unseal(const uint8_t *in, size_t len, const uint8_t wrap[32], tg_key **out);
TG_API void tg_key_free(tg_key *k);

/* ---- Обмен ключами (DH) ----
 * TL-сообщения строит и разбирает Go; ядро делает математику и проверки. */

/* Разложение pq (произведение двух простых < 2^32) на p < q. */
TG_API tg_status tg_pq_factorize(uint64_t pq, uint32_t *p, uint32_t *q);
/* RSA_PAD из спецификации для ключа с данным отпечатком; data не длиннее 144 байт.
 * Выход — 256 байт. TG_ERR_ARG, если ключ с таким отпечатком неизвестен. */
TG_API tg_status tg_rsa_pad_encrypt(const uint8_t *data, size_t len, uint64_t fingerprint, tg_buf *out);
/* Дополнение В1: известен ли ядру ключ сервера с таким отпечатком (1 — да). */
TG_API int tg_rsa_has_key(uint64_t fingerprint);
/* Дополнение В2: зарегистрировать открытый ключ сервера (модуль 2048 бит big-endian,
 * экспонента). Отпечаток вычисляется ядром (64 младших бита SHA1(TL bytes n + TL bytes e))
 * и возвращается. Нужен для смены ключей Telegram, ключей тестовых серверов
 * my.telegram.org и поддельного сервера в тестах сети. Не больше 16 ключей. */
TG_API tg_status tg_rsa_add_key(const uint8_t *n, size_t nlen, uint32_t e, uint64_t *fingerprint);

typedef struct tg_authgen tg_authgen;
/* Выводит временные tmp_aes_key/tmp_aes_iv из new_nonce и server_nonce. */
TG_API tg_status tg_authgen_new(const uint8_t new_nonce[32], const uint8_t server_nonce[16], tg_authgen **out);
/* Расшифровывает encrypted_answer, проверяет SHA1 и возвращает answer (TL server_DH_inner_data). */
TG_API tg_status tg_authgen_decrypt_dh(tg_authgen *g, const uint8_t *enc, size_t len, tg_buf *inner);
/* Шифрует client_DH_inner_data (SHA1 + данные + дополнение до 16). */
TG_API tg_status tg_authgen_encrypt_dh(tg_authgen *g, const uint8_t *inner, size_t len, tg_buf *enc);
/* Проверяет dh_prime, g, g_a; выбирает b; выдаёт g_b (256 байт) и вычисляет auth_key.
 * Повторный вызов (после dh_gen_retry) выбирает новый b. */
TG_API tg_status tg_authgen_dh(tg_authgen *g, const uint8_t *dh_prime, size_t plen, uint32_t gen,
                               const uint8_t *g_a, size_t ga_len, tg_buf *g_b);
/* new_nonce_hash{n}, n = 1, 2, 3 (dh_gen_ok / retry / fail). */
TG_API tg_status tg_authgen_nonce_hash(const tg_authgen *g, int n, uint8_t out[16]);
/* Дополнение В1: auth_key_aux_hash (64 старших бита SHA1(auth_key)) — нужен как
 * retry_id при повторе после dh_gen_retry. По протоколу передаётся открыто. */
TG_API tg_status tg_authgen_aux_hash(const tg_authgen *g, uint64_t *out);
/* Отдаёт готовый ключ; после этого хэндл больше не используется, кроме _free. */
TG_API tg_status tg_authgen_finish(tg_authgen *g, tg_key **key);
TG_API void tg_authgen_free(tg_authgen *g);

/* ---- Конверт MTProto 2.0 ----
 * Тело — готовые байты TL. Проверки session_id и чётности msg_id делает Go (session). */
typedef struct {
    uint64_t salt, session_id, msg_id;
    uint32_t seq_no;
} tg_env_hdr;

/* Клиент -> сервер (x = 0). Длина тела кратна 4. Дополнение 12..1024 байт выбирается случайно. */
TG_API tg_status tg_env_encrypt(const tg_key *k, const tg_env_hdr *h, const uint8_t *body, size_t len, tg_buf *out);
/* Сервер -> клиент (x = 8). Проверяет auth_key_id, msg_key (в постоянном времени),
 * длину тела и дополнения. */
TG_API tg_status tg_env_decrypt(const tg_key *k, const uint8_t *in, size_t len, tg_env_hdr *h, tg_buf *body);

/* ---- SRP (2FA) ----
 * passwordKdfAlgoSHA256SHA256PBKDF2HMACSHA512iter100000SHA256ModPow.
 * Выход: A (256 байт) и M1 (32 байта). */
TG_API tg_status tg_srp_check(const uint8_t *pw, size_t pw_len,
                              const uint8_t *salt1, size_t s1, const uint8_t *salt2, size_t s2,
                              uint32_t g, const uint8_t *p, size_t plen,
                              const uint8_t *srp_b, size_t blen, tg_buf *a_out, tg_buf *m1_out);

/* ---- Обфускация транспорта (obfuscated2, AES-256-CTR) ---- */
enum {
    TG_TRANSPORT_ABRIDGED = 0,     /* 0xefefefef */
    TG_TRANSPORT_INTERMEDIATE = 1, /* 0xeeeeeeee */
    TG_TRANSPORT_PADDED = 2        /* 0xdddddddd */
};
typedef struct tg_obfs tg_obfs;
/* init — 64 байта, отправляются первыми после TCP-рукопожатия.
 * dc_id: 0 — не указывать; иначе кодируется int16 LE по смещению 60 (для MTProxy,
 * отрицательный для медиа-DC, +10000 для тестовых). secret: 0, 16 или 17 байт
 * (у 17-байтного первый байт — тип транспорта, отбрасывается). */
TG_API tg_status tg_obfs_new(int transport, int dc_id, const uint8_t *secret, size_t slen,
                             tg_buf *init, tg_obfs **out);
TG_API void tg_obfs_encrypt(tg_obfs *o, uint8_t *data, size_t len); /* на месте, только писатель */
TG_API void tg_obfs_decrypt(tg_obfs *o, uint8_t *data, size_t len); /* на месте, только читатель */
TG_API void tg_obfs_free(tg_obfs *o);

#ifdef __cplusplus
}
#endif

#endif /* NUNCIO_TGCRYPTO_H */
