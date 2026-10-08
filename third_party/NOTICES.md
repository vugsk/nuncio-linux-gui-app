# Сторонние компоненты, встроенные в бинарник

Список для выпуска (План реализации, раздел 5). Версии, адреса и SHA-256 —
в `deps.toml`; сборка — `just deps` (`fetch_deps.py`). Исходники сторонних
библиотек в репозитории не хранятся.

## FFmpeg 9.0.1 — LGPL-2.1-or-later, статически

- Что: `libavcodec`, `libavformat`, `libavutil`, `libswscale`,
  `libswresample` (воспроизведение видео и звука, веха В8).
- Сборка: минимальная, `--disable-everything` и только собственные декодеры
  FFmpeg (H.264, HEVC, VP8/9, MPEG-4, AAC, Opus, Vorbis, MP3, FLAC, PCM),
  без GPL-частей и внешних библиотек; `configure` сообщает
  «License: LGPL version 2.1 or later». Флаги — `deps.toml`.
- Подлинность: архив сверяется по SHA-256, подпись `.asc` — ключом выпусков
  FFmpeg (отпечаток `FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8`)
  при каждой сборке.
- Линковка статическая. Обязательства LGPL при распространении: текст
  лицензии (`COPYING.LGPLv2.1`, кладётся `just deps` в
  `target/native/ffmpeg/`), исходники FFmpeg или предложение их передать
  (адрес и рецепт — `deps.toml`), возможность пересобрать программу с
  изменённой библиотекой — выполняется тем, что исходники приложения открыты
  (GPL-3.0-or-later) и сборка воспроизводима (`just deps && just build`).
- Привязки Rust: `ffmpeg-next`, `ffmpeg-sys-next` — WTFPL.
- Патентные вопросы H.264/HEVC не рассматривались (П-6).

## OpenSSL 3.6.4 — Apache-2.0, статически

- `libcrypto` урезанной сборкой для ядра (`core/`, решение Р-11). Текст
  лицензии кладётся `just deps` в `target/native/OpenSSL.LICENSE.txt`.

## Noto Color Emoji — SIL OFL 1.1

- Картинки эмодзи и флагов извлекаются при сборке (`ui/build.rs`); текст
  лицензии скачивается рядом (`target/native/NotoColorEmoji.LICENSE`).

## Skia — BSD-3-Clause, статически (с 2026-10-04, «Графика: GPU»)

- Рендерер Slint `renderer-skia-opengl` (Нс-8). Библиотеку собирает не
  проект: `skia-bindings` при первой сборке скачивает готовые бинарники
  Skia с GitHub (`rust-skia/skia-binaries`); подписи у них нет. Скачанный
  архив сверяется по SHA-256 из `deps.toml` (`tools/verify_skia.sh`, входит
  в `just build`, `lint`, `e2e`); сумма записана с первой загрузки.
- Вместе с Skia в бинарник входят её сторонние библиотеки (FreeType,
  HarfBuzz, ICU и др.); их лицензии — в репозитории `rust-skia`. Полный
  перечень для пакета — к выпуску.
- Вместе с Skia из умолчаний Slint подтягивается `wgpu` (MIT/Apache-2.0,
  проверяется `cargo deny`).

## Крейты Rust и модули Go

Лицензии крейтов проверяет `cargo deny check` (`deny.toml`): разрешены
MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, CC0, Unlicense, BSL-1.0,
WTFPL и GPL-3.0-only (Slint). Модули Go: `godbus/dbus` (BSD-2-Clause),
`google.golang.org/protobuf` (BSD-3-Clause); проверяет `tools/go_licenses.sh`
(входит в `just lint`).
