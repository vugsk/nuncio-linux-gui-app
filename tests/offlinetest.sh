#!/usr/bin/env bash
# Проверка очереди отправки без сети (План реализации, В5): сообщения,
# поставленные в очередь при выключенной сети, уходят после восстановления
# ровно по одному разу. Поддельный Telegram (net/cmd/fakeserver) «выключает
# сеть» по SIGUSR1 и включает по SIGUSR2.
#
#   tests/offlinetest.sh [сообщений] [секунд без сети]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SEND=${1:-10}
OFF=${2:-15}
PORT=${PORT:-44322}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-offline.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -dump "$T/server.json" > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
run_app() {
  env -u WAYLAND_DISPLAY DISPLAY="${DISPLAY_NUM:-:99}" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
    NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-offline \
    NUNCIO_KEYRING_COLLECTION=session NUNCIO_BENCH_SEND="$1" "$T/app" >> "$T/app.log" 2>&1 &
  APP=$!
}
# 1. Вход при работающей сети, без исходящих.
run_app 0
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do sleep 0.2; done
sleep 2
kill -TERM $APP; wait $APP 2>/dev/null || true
# 2. Сеть выключена; запуск ставит сообщения в очередь.
kill -USR1 $SRV
: > "$T/home/state/net.log"
run_app "$SEND"
sleep "$OFF"
queued=$(python3 -c "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('select count(*) from outbox').fetchone()[0])" "$T/home/data/nuncio.db")
echo "без сети $OFF с: в очереди $queued из $SEND"
# 3. Сеть включена — очередь уходит сама (переподключение, затем PumpOutbox).
kill -USR2 $SRV
for _ in $(seq 120); do
  n=$(python3 -c "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('select count(*) from outbox').fetchone()[0])" "$T/home/data/nuncio.db")
  [ "$n" = 0 ] && break
  sleep 0.5
done
kill -TERM $APP; wait $APP 2>/dev/null || true
kill -TERM $SRV; wait $SRV 2>/dev/null || true
python3 - "$T" "$SEND" "$queued" <<'PY'
import json, sqlite3, sys
t, send, queued = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
srv = [m["text"] for m in json.load(open(f"{t}/server.json")) if m["out"]]
counts = [srv.count(f"исходящее {i}") for i in range(1, send + 1)]
left = sqlite3.connect(f"{t}/home/data/nuncio.db").execute("SELECT count(*) FROM outbox").fetchone()[0]
ok = queued == send and all(c == 1 for c in counts) and left == 0
print(f"после восстановления: доставлено {sum(counts)} из {send}, дублей {sum(c > 1 for c in counts)}, в очереди {left}")
print("ИТОГ:", "дошло после восстановления, без дублей" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
