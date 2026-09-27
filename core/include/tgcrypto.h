#ifndef NUNCIO_TGCRYPTO_H
#define NUNCIO_TGCRYPTO_H

/*
 * Публичный C ABI крипто-ядра. Каркас (веха В0): пока пусто.
 *
 * Настоящие объявления (tg_pq_factorize, tg_rsa_pad_encrypt,
 * tg_authgen_*, tg_env_encrypt/decrypt, tg_srp_check, tg_obfs_*, ...)
 * добавляются на веху В1 по эскизу в
 * Архитектура/nuncio - Границы и C ABI.md — эта заметка источник
 * истины, здесь их не дублировать заранее, чтобы не разойтись.
 *
 * Правила ABI (см. ту же заметку): только tg_*-символы видимы
 * (-fvisibility=hidden в CMakeLists.txt), память, выданную ядром,
 * освобождает ядро (tg_buf_free), ядро вызывает только Go, никогда
 * Rust напрямую.
 */

#ifdef __cplusplus
extern "C" {
#endif

/* Пока намеренно пусто. */

#ifdef __cplusplus
}
#endif

#endif /* NUNCIO_TGCRYPTO_H */
