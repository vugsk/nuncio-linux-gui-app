#!/usr/bin/env bash
# Реклама (условия API Telegram) под Xvfb против поддельного Telegram (-ads:
# бот «Новости» с одним объявлением): одна строка в конце ленты; просмотр
# отмечается, когда строка в окне; щелчок раскрывает карточку; ссылка
# открывается (в проверке — в файл) с отметкой щелчка; ⓘ — «О рекламодателе»;
# ✕ — причины, выбор, жалоба принята, строка исчезает. В личном чате рекламы нет.
#
#   tests/adtest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44337}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-adtest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 5 -unread -1 -ads > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-adtest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 3
DISPLAY="$DISPLAY_NUM" python3 "$ROOT/tests/ad.py" "$T" "$APP"
kill -TERM $APP 2>/dev/null || true
