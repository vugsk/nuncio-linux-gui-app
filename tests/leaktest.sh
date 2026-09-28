#!/usr/bin/env bash
# Память без утечек (решение по памяти, 2026-09-28): поддельный Telegram с
# длинной историей, приложение под Xvfb; PSS с разбором после открытия чата и
# после нескольких полных проходов колесом по истории (подгрузка страниц,
# сдвиги окна, смена строк). Рост от прохода к проходу — подозрение на утечку.
#
#   tests/leaktest.sh [сообщений] [проходов]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
N=${1:-3000}
PASSES=${2:-4}
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44324}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-leaktest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history "$N" > "$T/server.log" 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-leaktest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_BENCH_OPEN=0 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do sleep 0.2; done
sleep 4

DISPLAY="$DISPLAY_NUM" python3 - "$APP" "$PASSES" <<'PY'
import sys, time
from Xlib import X, display
from Xlib.ext import xtest

pid, passes = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root

def find(w):
    name = w.get_wm_name()
    if name and "nuncio" in name.lower():
        return w
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r

win = find(root)
pos = win.translate_coords(root, 0, 0)
ox, oy = -pos.x, -pos.y

def mem():
    m = {}
    for line in open(f"/proc/{pid}/smaps_rollup"):
        k, _, v = line.partition(":")
        if v.strip().endswith("kB"):
            m[k] = int(v.split()[0]) / 1024
    return m

def show(label):
    m = mem()
    print(f"{label}: PSS {m['Pss']:.1f} МБ (анонимная {m['Pss_Anon']:.1f}, файлы {m['Pss_File']:.1f}), "
          f"RSS {m['Rss']:.1f}, собственные {m['Private_Clean'] + m['Private_Dirty']:.1f} МБ")
    return m["Pss"]

def wheel(btn, n):
    xtest.fake_input(d, X.MotionNotify, x=ox + 700, y=oy + 400)
    for _ in range(n):
        xtest.fake_input(d, X.ButtonPress, btn)
        xtest.fake_input(d, X.ButtonRelease, btn)
        d.sync()
        time.sleep(0.004)

start = show("открыт чат")
res = []
for p in range(1, passes + 1):
    # Вверх до начала (с подгрузкой) и обратно вниз.
    for _ in range(40):
        wheel(4, 60)
        time.sleep(0.25)
    for _ in range(40):
        wheel(5, 60)
        time.sleep(0.25)
    time.sleep(2)
    res.append(show(f"проход {p}"))
growth = res[-1] - res[1] if len(res) > 1 else 0.0
print(f"рост со 2-го по последний проход: {growth:+.1f} МБ")
print("ИТОГ:", "роста нет" if growth < 3.0 else "РОСТ — проверить")
PY
kill -TERM $APP; wait $APP 2>/dev/null || true
echo "результат: $T"
