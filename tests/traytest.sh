#!/usr/bin/env bash
# Веха В9 под Xvfb против поддельного Telegram: значок в трее (SNI) на своей
# шине D-Bus с поддельным наблюдателем и хостом (на рабочий стол человека
# ничего не попадает).
#   1. С хостом: значок зарегистрирован (картинки, подсказка, меню), счётчик
#      растёт с входящими; крестик прячет окно, процесс жив; щелчок по
#      значку возвращает окно; «Выйти» в меню завершает процесс.
#   2. Наблюдатель без хоста: крестик завершает процесс (Нс-5).
# Память спрятанного окна печатается (PSS) для сверки с П-5.
#
#   tests/traytest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44330}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-traytest.XXXXXX")
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver && go build -o "$T/fakedesk" ./cmd/fakedesk)
cp "$ROOT/target/release/nuncio" "$T/app"
SRV= BUS= DESK= APP=
source "$ROOT/tests/common.sh"
stop_all() { kill $SRV $BUS $DESK 2>/dev/null || true; [ -n "$APP" ] && kill -KILL $APP 2>/dev/null || true; }
trap 'rc=$?; stop_all; rm -f "$T/fakedesk"; tmp_done $rc' EXIT

# run N HOST — прогон N: сервер, шина, рабочий стол (-host=HOST), приложение.
run() {
  local n=$1 host=$2 dir="$T/$1"
  mkdir -p "$dir/home/config"
  printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$dir/home/config/api.env"
  chmod 600 "$dir/home/config/api.env"
  "$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$dir/key" -history 5 -unread -1 -start 4s -push 2 -every 1s \
    > "$dir/server.log" 2>&1 &
  SRV=$!
  dbus-daemon --session --nofork --nopidfile --print-address=3 --address="unix:path=$dir/bus" 3> "$dir/bus.addr" &
  BUS=$!
  until [ -s "$dir/bus.addr" ]; do sleep 0.1; done
  local addr
  addr=$(head -1 "$dir/bus.addr")
  DBUS_SESSION_BUS_ADDRESS=$addr "$T/fakedesk" -tray -host="$host" > "$dir/desk.log" 2>&1 &
  DESK=$!
  until grep -q готов "$dir/server.log" && grep -q готов "$dir/desk.log"; do sleep 0.1; done
  env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$dir/home" \
    NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$dir/key" NUNCIO_KEYRING_APP=nuncio-traytest$n \
    NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_DESKTOP_BUS="$addr" \
    "$T/app" > "$dir/app.log" 2>&1 &
  APP=$!
  until grep -q "шаг READY" "$dir/home/state/net.log" 2>/dev/null; do
    kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$dir/app.log"; exit 1; }
    sleep 0.2
  done
  DISPLAY="$DISPLAY_NUM" python3 "$ROOT/tests/tray.py" "$dir" "$APP" "$DESK" "$host"
  stop_all
  wait $SRV $BUS $DESK 2>/dev/null || true
  APP=
}

echo "1. с хостом значка"
run 1 true
echo "2. без хоста"
run 2 false
echo "ИТОГ: трей в порядке"
