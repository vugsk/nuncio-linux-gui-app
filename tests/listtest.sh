#!/usr/bin/env bash
# Веха В6 под Xvfb против поддельного Telegram (-extras: ещё собеседники,
# архивный чат, папка «Работа»): фильтр папок и архив, поиск в шапке (база и
# сервер), открытие результата, поиск по чату, сведения, меню диалога
# («без звука», «в архив»). Проверки — по журналу отладки (счётчики), базе и
# снимкам экрана (синтетические данные).
#
#   tests/listtest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44325}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-listtest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 60 -unread -1 -extras > "$T/server.log" 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true; kill $APP 2>/dev/null || true' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-listtest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1300x800 NUNCIO_DEBUG_SCROLL=1 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do sleep 0.2; done
sleep 4

DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" <<'PY'
import sqlite3, subprocess, sys, time
from Xlib import X, XK, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root

def find(w):
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

win = find(root)
g = win.get_geometry()
pos = win.translate_coords(root, 0, 0)
ox, oy, W, H = -pos.x, -pos.y, g.width, g.height
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()

def click(x, y, b=1):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    xtest.fake_input(d, X.ButtonPress, b)
    xtest.fake_input(d, X.ButtonRelease, b)
    d.sync()
    time.sleep(0.6)

def typ(s):
    for ch in s:
        # Кириллица — через keysym Cyrillic_* (xkb подставит на свободную клавишу не умеет):
        # в поддельных данных ищем латиницей и цифрами.
        k = d.keysym_to_keycode(XK.string_to_keysym({" ": "space"}.get(ch, ch)))
        xtest.fake_input(d, X.KeyPress, k)
        xtest.fake_input(d, X.KeyRelease, k)
        d.sync()
        time.sleep(0.05)

def shot(name):
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", f"{t}/{name}.png"], check=True)

def log():
    return open(f"{t}/home/state/nuncio.log", encoding="utf-8").read()

def db(q, *a):
    return sqlite3.connect(f"{t}/home/data/nuncio.db").execute(q, a).fetchall()

ok = True
def check(cond, what):
    global ok
    print(("  ок: " if cond else "  ОШИБКА: ") + what)
    ok &= bool(cond)

shot("1-list")
rows = db("SELECT peer_id, folder FROM dialogs ORDER BY peer_id")
check((102, 1) in rows and (101, 0) in rows, "архивный чат в базе с folder = 1")
# Фильтр: «Все чаты» → «Работа» → «Архив».
click(110, 26)
shot("2-filter")
click(120, 50 + 6 + 18 + 36)     # «Работа»
shot("3-folder")
click(110, 26)
click(120, 50 + 6 + 18 + 72)     # «Архив»
shot("4-archive")
click(110, 26)
click(120, 50 + 6 + 18)          # «Все чаты»
# Поиск в шапке (на сервер — от двух знаков): «15» — «сообщение 15» у «Отправителя».
click(360 - 8 - 44 - 8 - 18, 26)
typ("15")
time.sleep(2.5)
shot("5-search")
L = log()
check("в базе 0" in L, "поиск по базе выполнен (история чата ещё не загружена)")
check("поиск: на сервере 1" in L, "поиск на сервере нашёл сообщение")
# Результат с сервера: заголовок раздела 30 px, строка 58 px.
click(180, 52 + 30 + 29)
time.sleep(2)
shot("6-opened")
check(db("SELECT count(*) FROM messages WHERE peer_id = 100 AND id = 16") == [(1,)], "открыт чат результата")
# Выйти из поиска: крестик справа от поля.
click(360 - 8 - 18, 26)
# Поиск по чату: лупа в шапке чата.
click(W - 12 - 18, 26)
time.sleep(0.8)
typ("5")
time.sleep(2)
shot("7-chat-search")
check("поиск по чату:" in log(), "поиск по чату выполнен")
# Сведения: клик по названию чата.
click(420, 26)
time.sleep(1.5)
shot("8-info")
check("сведения:" in log(), "сведения о чате получены")
click(W - 12 - 18, 26)           # закрыть панель (крестик)
# «В папку…»: Мария в «Работе» явно — убрать (меню: 3 пункта, черта, пометка, «В папку…»).
click(180, 52 + 64 + 32, 3)
click(200, 52 + 64 + 32 + 6 + 3 * 32 + 1 + 32 + 16)
shot("9a-folders-menu")
click(200, 52 + 64 + 32 + 6 + 16)        # единственная папка «Работа»
time.sleep(1.5)
check(db("SELECT include FROM folders WHERE id = 2") == [("",)], "«В папку…» убрало чат из папки")
# Меню диалога: второй чат (Мария) — «Без звука», затем «В архив».
click(180, 52 + 64 + 32, 3)
shot("9-menu")
click(200, 52 + 64 + 32 + 6 + 16 + 32)   # второй пункт: «Без звука»
time.sleep(1.5)
check(db("SELECT muted FROM dialogs WHERE peer_id = 101") == [(1,)], "«Без звука» применено")
click(180, 52 + 64 + 32, 3)
click(200, 52 + 64 + 32 + 6 + 16 + 64)   # третий пункт: «В архив»
time.sleep(1.5)
check(db("SELECT folder FROM dialogs WHERE peer_id = 101") == [(1,)], "«В архив» применено")
shot("10-after-actions")
print("ИТОГ:", "поиск и папки в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
kill -TERM $APP 2>/dev/null || true
echo "результат: $T"
