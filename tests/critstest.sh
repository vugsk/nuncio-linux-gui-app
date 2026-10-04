#!/usr/bin/env bash
# Веха В9 под Xvfb: критические состояния показывают экран, а не роняют
# процесс (Дизайн «Состояния»):
#   1. повреждённая база (мусор в nuncio.db) — экран «база повреждена»;
#   2. нет Secret Service (своя пустая шина D-Bus) — экран «нет связки ключей»;
#   3. нет api.env — экран «нет ключей API»;
#   4. сессия завершена с другого устройства (поддельный сервер отзывает
#      все сеансы) — экран входа с объяснением, локальные данные удалены.
# В каждом случае процесс жив; снимки остаются при NUNCIO_KEEP=1.
#
#   tests/critstest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44332}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-critstest.XXXXXX")
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
cp "$ROOT/target/release/nuncio" "$T/app"
SRV= BUS= APP=
source "$ROOT/tests/common.sh"
stop_all() { kill $SRV $BUS 2>/dev/null || true; if [ -n "$APP" ]; then { kill -KILL $APP; wait $APP; } 2>/dev/null || true; fi; APP=; }
trap 'rc=$?; stop_all; tmp_done $rc' EXIT
ok=1
check() { if eval "$1"; then echo "  ок: $2"; else echo "  ОШИБКА: $2"; ok=0; fi; }

# home N [без api.env] — каталог прогона N.
home() {
  local dir="$T/$1/home"
  mkdir -p "$dir/config" "$dir/data"
  if [ "${2:-}" != noapi ]; then
    printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$dir/config/api.env"
    chmod 600 "$dir/config/api.env"
  fi
  echo "$dir"
}

# start HOME [VAR=знач…] — запустить приложение с этим каталогом.
start() {
  local h=$1
  shift
  env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$h" \
    NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-critstest \
    NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 "$@" \
    "$T/app" > "$h/../app.log" 2>&1 &
  APP=$!
}

# wait_log ФАЙЛ ТЕКСТ — дождаться строки в журнале (до 20 с), пока процесс жив.
wait_log() {
  for _ in $(seq 80); do
    grep -q "$2" "$1" 2>/dev/null && return 0
    kill -0 $APP 2>/dev/null || return 1
    sleep 0.25
  done
  return 1
}

shot() {
  DISPLAY="$DISPLAY_NUM" python3 - "$APP" "$1" <<'PY'
import subprocess, sys, time
from Xlib import X, display
pid, out = int(sys.argv[1]), sys.argv[2]
d = display.Display()
atom = d.intern_atom("_NET_WM_PID")
def find(w):
    try:
        p = w.get_full_property(atom, X.AnyPropertyType)
        if p and int(p.value[0]) == pid:
            return w
    except Exception:
        pass
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r
for _ in range(40):
    w = find(d.screen().root)
    if w:
        break
    time.sleep(0.25)
g = w.get_geometry()
pos = w.translate_coords(d.screen().root, 0, 0)
time.sleep(1)
subprocess.run(["import", "-window", "root", "-crop", f"{g.width}x{g.height}+{-pos.x}+{-pos.y}", out], check=True)
PY
}

"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 5 -unread -1 -revoke 6s > "$T/server.log" 2>&1 &
SRV=$!
until grep -q готов "$T/server.log"; do sleep 0.1; done

echo "1. повреждённая база"
H=$(home 1)
head -c 65536 /dev/urandom > "$H/data/nuncio.db"
start "$H"
check 'wait_log "$H/state/nuncio.log" "критическое состояние: DatabaseCorrupt"' "экран «база повреждена»"
shot "$T/1-db.png"
check 'kill -0 $APP' "процесс жив"
stop_all

echo "2. нет Secret Service"
H=$(home 2)
private_bus "$T/2"
start "$H" DBUS_SESSION_BUS_ADDRESS="$(head -1 "$T/2/bus.addr")"
check 'wait_log "$H/state/nuncio.log" "критическое состояние: KeyringUnavailable"' "экран «нет связки ключей»"
shot "$T/2-keyring.png"
check 'kill -0 $APP' "процесс жив"
kill $BUS 2>/dev/null || true
BUS=
stop_all
SRV=$(pgrep -f "$T/fakeserver" || true)

echo "3. нет api.env"
H=$(home 3 noapi)
start "$H"
check 'wait_log "$H/state/nuncio.log" "критическое состояние: NoApiCredentials"' "экран «нет ключей API»"
shot "$T/3-api.png"
check 'kill -0 $APP' "процесс жив"
stop_all
SRV=$(pgrep -f "$T/fakeserver" || true)

echo "4. сессия завершена с другого устройства"
# Сервер отзывает сеансы через 6 с после первого входа — входим заново.
kill $SRV 2>/dev/null || true
wait $SRV 2>/dev/null || true
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 5 -unread -1 -revoke 6s > "$T/server.log" 2>&1 &
SRV=$!
until grep -q готов "$T/server.log"; do sleep 0.1; done
H=$(home 4)
start "$H"
check 'wait_log "$H/state/net.log" "шаг READY"' "вошли"
check 'wait_log "$T/server.log" "сеансы завершены"' "сервер завершил сеансы"
check 'wait_log "$H/state/net.log" "шаг LOGGED_OUT session_ended"' "клиент узнал о завершении сессии"
check 'wait_log "$H/state/nuncio.log" "экран: вход"' "экран входа"
sleep 1
shot "$T/4-session-ended.png"
check '[ "$(sqlite3 "file:$H/data/nuncio.db?mode=ro" "SELECT COUNT(*) FROM messages")" = 0 ]' "локальные сообщения удалены"
check 'kill -0 $APP' "процесс жив"
stop_all

[ $ok = 1 ] && echo "ИТОГ: критические состояния в порядке" || { echo "ИТОГ: ОШИБКА"; exit 1; }
