#!/usr/bin/env bash
# Общие медиа в панели «Сведения» (Св-1) под Xvfb против поддельного
# Telegram (чат «Вложения», -media): вкладка «Медиа» открывается сама у
# личного чата и показывает сетку; «Файлы», «Ссылки» — списки; щелчок по
# ячейке сетки открывает окно просмотра. Проверки — по журналу (счётчики) и
# снимкам (синтетические данные).
#
#   tests/sharedtest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44335}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-sharedtest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -unread -1 -media > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-sharedtest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1300x800 "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 4

DISPLAY="$DISPLAY_NUM" python3 "$ROOT/tests/shared.py" "$T" "$APP"
kill -TERM $APP 2>/dev/null || true
