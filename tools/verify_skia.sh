#!/usr/bin/env bash
# Сверить архивы Skia, скачанные skia-bindings в target/*/build/*/out/.cache,
# с суммами SHA-256 из third_party/deps.toml ([skia.archives]). Неизвестный
# архив или несовпадение — ошибка (бинарники подписи не имеют, см. deps.toml).
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - <<'PY'
import glob, hashlib, os, sys, tomllib
want = tomllib.load(open("third_party/deps.toml", "rb"))["skia"]["archives"]
found = glob.glob("target/*/build/skia-bindings-*/out/.cache/skia-binaries-*.tar.gz")
bad = 0
for path in sorted(found):
    name = os.path.basename(path)
    got = hashlib.sha256(open(path, "rb").read()).hexdigest()
    if name not in want:
        print(f"ОШИБКА: неизвестный архив Skia {name} — сверить и добавить в third_party/deps.toml")
        bad = 1
    elif want[name] != got:
        print(f"ОШИБКА: архив Skia {name} не совпадает с SHA-256 из deps.toml ({path})")
        bad = 1
print(f"Skia: проверено архивов {len(found)}" + ("" if found else " (ещё не скачан)"))
sys.exit(bad)
PY
