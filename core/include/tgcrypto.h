#ifndef NUNCIO_TGCRYPTO_H
#define NUNCIO_TGCRYPTO_H

/*
 * Публичный C ABI крипто-ядра. Источник истины — эскиз в
 * Архитектура/nuncio - Границы и C ABI.md; здесь объявляется только то,
 * что уже реализовано.
 *
 * Правила: видимы только tg_*-символы (-fvisibility=hidden), выходные
 * tg_buf выдаёт ядро и освобождает tg_buf_free, исключения C++ не
 * пересекают границу, ядро вызывает только Go.
 *
 * Веха В0: tg_version и tg_buf_free (для проверки связки Rust -> Go -> ядро).
 * Примитивы (конверт, IGE, RSA_PAD, DH, SRP, обфускация) — веха В1.
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
    TG_ERR_ARG,
    TG_ERR_NOMEM,
    TG_ERR_CRYPTO,
    TG_ERR_MSG_KEY,
    TG_ERR_DH,
    TG_ERR_STATE
} tg_status;

/* Строка вида "tgcrypto 0.1.0"; статическая, освобождать не нужно. */
TG_API const char *tg_version(void);

/* Освобождает буфер, выданный ядром. Безопасна для {NULL, 0}. */
TG_API void tg_buf_free(tg_buf b);

#ifdef __cplusplus
}
#endif

#endif /* NUNCIO_TGCRYPTO_H */
