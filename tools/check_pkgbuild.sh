#!/usr/bin/env bash
# Версии и SHA-256 сторонних файлов в packaging/arch/PKGBUILD — те же, что в
# third_party/deps.toml (OpenSSL, FFmpeg, шрифт эмодзи и его лицензия, архив
# Skia). Расхождение — ошибка: пакет собирался бы не из того, что проверено.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import re, sys, tomllib
deps = tomllib.load(open("third_party/deps.toml", "rb"))
pkg = open("packaging/arch/PKGBUILD", encoding="utf-8").read()
sums = re.search(r"sha256sums=\((.*?)\)", pkg, re.S).group(1)
need = {
    "версия OpenSSL": f"_openssl={deps['openssl']['version']}" in pkg,
    "версия FFmpeg": f"_ffmpeg={deps['ffmpeg']['version']}" in pkg,
    "версия шрифта эмодзи": f"_noto={deps['noto-emoji']['version']}" in pkg,
    "SHA-256 OpenSSL": deps["openssl"]["sha256"] in sums,
    "SHA-256 FFmpeg": deps["ffmpeg"]["sha256"] in sums,
    "SHA-256 шрифта эмодзи": deps["noto-emoji"]["sha256"] in sums,
    "SHA-256 лицензии шрифта": deps["noto-emoji"]["license_sha256"] in sums,
}
for name, digest in deps["skia"]["archives"].items():
    need[f"архив Skia {name[:40]}…"] = f"_skia={name}" in pkg and digest in sums
bad = [k for k, ok in need.items() if not ok]
for k in bad:
    print(f"ОШИБКА: PKGBUILD расходится с deps.toml: {k}")
print("PKGBUILD сверен с deps.toml" if not bad else "")
sys.exit(1 if bad else 0)
PY
