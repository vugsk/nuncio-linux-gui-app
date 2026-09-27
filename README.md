# nuncio-linux-gui-app

Клиент Telegram под Linux. Три слоя в одном процессе: интерфейс и домен (Rust + Slint),
сеть (Go, c-archive), крипто-ядро (C++, статическая `libtgcrypto.a`).

Архитектура, дизайн экранов и все решения — в Obsidian-хранилище проекта, этот репозиторий
их не дублирует:

- карта проекта: `Проекты/Проект клиент телеграмма/nuncio - Карта проекта.md`
- структура репозитория и шаги сборки: `Архитектура/nuncio - Сборка и структура репозитория.md`
- план реализации по вехам: `Архитектура/nuncio - План реализации.md`

**Статус: веха В0 закрыта.** Каркас и вертикаль-заглушка: окно Slint → поток-насос →
Go (c-archive, `nn_*`) → ядро C++ (`tg_version`) → обратно в окно. Криптографии, TL-кодека
и сети ещё нет — это вехи В1–В2.

## Сборка

Нужны: Rust, Go, CMake, компилятор C++20, `protoc` + `protoc-gen-go`, `just`;
для `just lint` ещё `cargo-deny`, `staticcheck`, `clang-tidy`.

```
just build   # шаги 1–5: tl, core, proto, net, cargo build
just test    # ctest, go test -race, cargo test
just lint    # clippy, cargo deny, go vet + staticcheck, clang-tidy
just deps    # скачивает и собирает OpenSSL и FFmpeg (третьи стороны, third_party/deps.toml)
```

Подробности каждого шага — в заметке о сборке (ссылка выше).

## Лицензия

GPL-3.0-or-later, см. `LICENSE`.
