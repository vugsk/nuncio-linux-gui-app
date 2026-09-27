// Обфускация: произвольные init/секрет/данные; круг шифрования с «сервером» не
// нужен (есть в модульных тестах) — здесь поиск ошибок памяти и зависаний.
#include "common.h"
extern "C" int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n) {
    if (n < 64 + 2) return 0;
    const int transport = d[0] % 4;
    const size_t slen = (d[1] % 3 == 0) ? 0 : (d[1] % 3 == 1 ? 16 : 17);
    if (n < 2 + 64 + slen) return 0;
    const int dc = static_cast<int16_t>(d[2] | (d[3] << 8));
    tg_buf init{};
    tg_obfs *o = nullptr;
    if (tg::obfs_new_with_init(transport, dc, d + 66, slen, d + 2, &init, &o) == TG_OK) {
        if (init.len != 64) __builtin_trap();
        std::vector<uint8_t> buf(d + 66 + slen, d + n);
        tg_obfs_encrypt(o, buf.data(), buf.size());
        tg_obfs_decrypt(o, buf.data(), buf.size());
        tg_obfs_free(o);
        tg_buf_free(init);
    }
    return 0;
}
