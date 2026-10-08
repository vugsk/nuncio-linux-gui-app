#!/usr/bin/env bash
# Тема из файла (Нс-7) применяется на лету: файл переписан — окно
# перекрашивается без перезапуска; ошибки файла видны в настройках, верные
# ключи применяются. Под Xvfb, без сервера (экран «нет ключей API» — фон окна).
#
#   tests/themetest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-themetest.XXXXXX")
APP=
source "$ROOT/tests/common.sh"
trap 'rc=$?; [ -n "$APP" ] && kill $APP 2>/dev/null || true; tmp_done $rc' EXIT
mkdir -p "$T/home/config"
printf 'window_bg = "#203040"\n' > "$T/theme.toml"
printf '[app]\ntheme = "file"\ntheme_file = "%s"\n' "$T/theme.toml" > "$T/home/config/config.toml"
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_WINDOW_SIZE=900x700 "$T/app" > "$T/app.log" 2>&1 &
APP=$!
sleep 3
DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" <<'PY'
import subprocess, sys, time
from PIL import Image
from Xlib import X, display
t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
A = d.intern_atom("_NET_WM_PID")
def find(w):
    try:
        p = w.get_full_property(A, X.AnyPropertyType)
        if p and int(p.value[0]) == PID:
            return w
    except Exception:
        pass
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r
win = find(d.screen().root)
g = win.get_geometry()
pos = win.translate_coords(d.screen().root, 0, 0)
def bg(name):
    path = f"{t}/{name}.png"
    subprocess.run(["import", "-window", "root", "-crop", f"{g.width}x{g.height}+{-pos.x}+{-pos.y}", path], check=True)
    return Image.open(path).convert("RGB").getpixel((10, g.height - 10))
ok = True
def check(c, what):
    global ok
    print(("  ок: " if c else "  ОШИБКА: ") + what)
    ok &= bool(c)
check(bg("1-start") == (0x20, 0x30, 0x40), f"фон из файла при запуске: {bg('1-start')}")
open(f"{t}/theme.toml", "w").write('window_bg = "#405060"\n')
time.sleep(2.5)
check(bg("2-changed") == (0x40, 0x50, 0x60), f"файл переписан — фон сменился на лету: {bg('2-changed')}")
open(f"{t}/theme.toml", "w").write('window_bg = "#506070"\nnope = "#fff"\ntext = "red"\n')
time.sleep(2.5)
check(bg("3-errors") == (0x50, 0x60, 0x70), "с ошибками в файле верные ключи применяются")
log = open(f"{t}/home/state/nuncio.log", encoding="utf-8").read()
check(log.count("тема: файл изменён") >= 2, "журнал: перечитывание файла")
print("ИТОГ:", "тема на лету в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
