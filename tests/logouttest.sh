#!/usr/bin/env bash
# Веха В9 под Xvfb против поддельного Telegram: «Выйти из аккаунта» (Нс-6)
# удаляет сессию, ключ, базу и кэш. Перед выходом в кэше есть вложения,
# аватарки и вставленные картинки (подложены как файлы), в базе — сообщения.
# После подтверждения: сервер получил auth.logOut; прежнего session.bin нет
# (новый — неавторизованная сессия для входа по QR); ключ
# обёртки в связке сменился (старый удалён, новый — для следующего входа);
# в базе нет сообщений, диалогов, собеседников, вложений; каталог кэша пуст;
# на экране — вход.
#
#   tests/logouttest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44331}
KEYAPP=nuncio-logouttest
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-logouttest.XXXXXX")
mkdir -p "$T/home/config" "$T/home/cache/paste" "$T/home/cache/avatars" "$T/home/cache/media"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
for d in paste avatars media; do head -c 4096 /dev/urandom > "$T/home/cache/$d/seed.bin"; done

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 5 -unread -1 -media > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=$KEYAPP \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 3
# Ключ обёртки — по пути элемента в связке (сам ключ не читается).
key_items() { secret-tool search --all application "$KEYAPP" purpose session-wrap-key 2>&1 | grep '^\[' | sort | tr '\n' ' ' || true; }
KEY_BEFORE=$(key_items)
SESSION_BEFORE=$(sha256sum < "$T/home/data/session.bin")
N_BEFORE=$(sqlite3 "file:$T/home/data/nuncio.db?mode=ro" "SELECT COUNT(*) FROM messages")
[ "$N_BEFORE" -gt 0 ] || { echo "ОШИБКА: в базе нет сообщений до выхода"; exit 1; }
[ -n "$KEY_BEFORE" ] || { echo "ОШИБКА: ключа обёртки нет в связке до выхода"; exit 1; }

DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" <<'PY'
import subprocess, sys, time
from Xlib import X, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root
PID_ATOM = d.intern_atom("_NET_WM_PID")

def find(w):
    try:
        prop = w.get_full_property(PID_ATOM, X.AnyPropertyType)
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

def click(x, y):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    xtest.fake_input(d, X.ButtonPress, 1)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    time.sleep(0.5)

def shot(name):
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", f"{t}/{name}.png"], check=True)

CX, CY = (W - 820) // 2, (H - 600) // 2
X0, Y0 = CX + 201 + 22, CY + 53 + 22
click(30, 26)                 # ☰
click(100, 56 + 6 + 18)       # «Настройки»
time.sleep(1)
click(CX + 100, CY + 53 + 10 + 5 * 40 + 19)   # «Аккаунт»
time.sleep(1.5)
shot("1-account")
click(CX + 323, CY + 391)   # «Выйти»
time.sleep(0.8)
shot("2-confirm")
click(W // 2 + 81, H // 2 + 36)  # подтверждение «Выйти»
PY

ok=1
check() { if eval "$1"; then echo "  ок: $2"; else echo "  ОШИБКА: $2"; ok=0; fi; }
DB="$T/home/data/nuncio.db"
count() { sqlite3 "file:$DB?mode=ro" "SELECT (SELECT COUNT(*) FROM messages)+(SELECT COUNT(*) FROM dialogs)+(SELECT COUNT(*) FROM peers)+(SELECT COUNT(*) FROM media)"; }
for _ in $(seq 40); do grep -q "выход из аккаунта" "$T/server.log" && grep -q "шаг LOGGED_OUT" "$T/home/state/net.log" && break; sleep 0.25; done
sleep 2
check 'grep -q "выход из аккаунта" "$T/server.log"' "сервер получил auth.logOut"
# Новый файл — неавторизованная сессия для следующего входа (QR).
check '[ ! -e "$T/home/data/session.bin" ] || [ "$(sha256sum < "$T/home/data/session.bin")" != "$SESSION_BEFORE" ]' "старого файла сессии нет"
KEY_AFTER=$(key_items)
check '[ "$KEY_AFTER" != "$KEY_BEFORE" ]' "старый ключ обёртки удалён из связки (было: $KEY_BEFORE, стало: ${KEY_AFTER:-нет})"
check '[ "$(count)" = 0 ]' "в базе нет сообщений, диалогов, собеседников, вложений ($(count))"
check '[ -z "$(find "$T/home/cache" -mindepth 1 -print -quit)" ]' "кэш пуст ($(find "$T/home/cache" -mindepth 1 | wc -l) записей)"
check 'tail -3 "$T/home/state/net.log" | grep -q "шаг QR"' "снова экран входа"
check 'kill -0 $APP' "приложение работает"
kill -TERM $APP 2>/dev/null || true
[ $ok = 1 ] && echo "ИТОГ: выход из аккаунта в порядке" || { echo "ИТОГ: ОШИБКА"; exit 1; }
