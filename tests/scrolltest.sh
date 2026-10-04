#!/usr/bin/env bash
# Своя прокрутка и окно модели на длинной истории (План, В5): поддельный
# Telegram с N сообщениями, приложение под Xvfb, колесо и клики через XTest
# (python-xlib). Проверяет по базе: подгрузка вверх идёт страницами с сервера,
# переход ползунком создаёт отдельный участок, «вниз» возвращает к концу.
# Снимки экрана (синтетические данные) — в каталоге результата.
#
#   tests/scrolltest.sh [сообщений в истории]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
N=${1:-3000}
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44322}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-scrolltest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history "$N" -unread -1 > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-scrolltest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_BENCH_OPEN=0 NUNCIO_DEBUG_SCROLL=1 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 4

DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" <<'PY'
import sqlite3, subprocess, sys, time
from Xlib import X, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root

def find(w):
    # Окно нашего процесса (_NET_WM_PID): другое окно nuncio в том же Xvfb
    # иначе перехватило бы ввод.
    atom = d.intern_atom("_NET_WM_PID")
    try:
        prop = w.get_full_property(atom, X.AnyPropertyType)
        if prop and int(prop.value[0]) == PID:
            return w
    except Exception:
        pass
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r
    return None

win = find(root)
assert win, "окно не найдено"
g = win.get_geometry()
pos = win.translate_coords(root, 0, 0)
ox, oy = -pos.x, -pos.y
W, H = g.width, g.height

def move(x, y):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    d.sync()

def click(btn, n=1, delay=0.02):
    for _ in range(n):
        xtest.fake_input(d, X.ButtonPress, btn)
        xtest.fake_input(d, X.ButtonRelease, btn)
        d.sync()
        time.sleep(delay)

def shot(name):
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", f"{t}/{name}.png"], check=True)

def db():
    c = sqlite3.connect(f"{t}/home/data/nuncio.db")
    n = c.execute("SELECT count(*) FROM messages WHERE peer_id = 100 AND id > 0").fetchone()[0]
    r = c.execute("SELECT lo, hi FROM history_ranges WHERE peer_id = 100 ORDER BY lo").fetchall()
    u = c.execute("SELECT unread, read_inbox_max FROM dialogs WHERE peer_id = 100").fetchone()
    return n, r, u

ok = True
def check(cond, what):
    global ok
    print(("  ок: " if cond else "  ОШИБКА: ") + what)
    ok &= bool(cond)

n0, r0, u0 = db()
print("старт:", (n0, r0, u0))
check(u0[0] == 0, "непрочитанных нет — вход к концу истории")
shot("1-bottom")
# Чат без непрочитанных (вид у низа): несколько щелчков вверх — вид остаётся
# поднятым (раньше строки сообщали высоту до раскладки, и вид возвращался вниз).
move(700, 400)
for _ in range(6):
    click(4, 1, 0.15)
time.sleep(1.5)
last = [l for l in open(f"{t}/app.log") if l.startswith("scroll:")][-1]
check("низ false" in last, "после щелчков вверх вид не вернулся к низу")
# Колесо вверх: 400 щелчков — далеко за первую страницу.
move(700, 400)
for _ in range(8):
    click(4, 50, 0.01)
    time.sleep(0.8)
n1, r1, _ = db()
print("после прокрутки вверх:", (n1, r1))
check(n1 >= n0 + 500, "подгрузка вверх страницами с сервера")
check([x for x in r1 if x[1] == 2147483647 and x[0] > 1] and len(r1) <= 2, "страницы слились в один участок до настоящего")
shot("2-scrolled-up")
# Ползунок: клик на 10 % высоты полосы — переход в начало чата.
bar_x = W - 6
move(bar_x, 100)
xtest.fake_input(d, X.ButtonPress, 1); d.sync(); time.sleep(0.1)
xtest.fake_input(d, X.ButtonRelease, 1); d.sync()
time.sleep(3)
n2, r2, _ = db()
print("после ползунка:", (n2, r2))
check(any(1 < lo and hi < 1000 for lo, hi in r2), "переход ползунком — отдельный участок у начала чата")
shot("3-thumb-jump")
# Колесо вниз немного, затем «вниз» (кнопка у правого нижнего угла ленты).
move(700, 400)
click(5, 20, 0.01)
time.sleep(1)
move(W - 38, H - 48 - 38)
time.sleep(0.2)
click(1)
time.sleep(3)
print("после «вниз»:", db())
shot("4-bottom-again")
print("ИТОГ:", "прокрутка и окно в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
kill -TERM $APP 2>/dev/null || true
sleep 1
grep -E "ошибка|panic|ERROR" "$T/app.log" "$T/home/state/"*.log | head -5 || true
echo "результат: $T"
