#!/usr/bin/env bash
# Проверка Р-8 (План реализации, В5): kill -9 посреди записи не теряет и не
# дублирует сообщения — ни входящие (pts, getDifference), ни исходящие (очередь,
# random_id). Приложение работает под Xvfb против поддельного Telegram
# (net/cmd/fakeserver); связка ключей — коллекция session.
#
#   tests/killtest.sh [число убийств] [входящих] [исходящих]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KILLS=${1:-4}
PUSH=${2:-300}
SEND=${3:-40}
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44321}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-killtest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -push "$PUSH" -every 15ms -start 4s -dump "$T/server.json" \
  > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"

run_app() {
  env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
    NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-killtest \
    NUNCIO_KEYRING_COLLECTION=session NUNCIO_BENCH_SEND="$SEND" "$T/app" >> "$T/app.log" 2>&1 &
  APP=$!
}

run_app
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do sleep 0.2; done
echo "вход выполнен; убийств: $KILLS"
for i in $(seq "$KILLS"); do
  sleep "$(python3 -c 'import random; print(round(random.uniform(1.0, 3.0), 2))')"
  kill -KILL $APP; wait $APP 2>/dev/null || true
  echo "  kill -9 №$i ($(grep -c '^push' "$T/server.log") входящих отправлено сервером)"
  run_app
done
until grep -q "push done" "$T/server.log"; do sleep 0.5; done
# Очередь уходит не быстрее ограничителя частоты (2 запроса/с на метод): ждём, пока опустеет.
for _ in $(seq 180); do
  n=$(python3 -c "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('select count(*) from outbox').fetchone()[0])"       "$T/home/data/nuncio.db" 2>/dev/null || echo 1)
  [ "$n" = 0 ] && break
  sleep 0.5
done
sleep 3 # последние обновления
kill -TERM $APP; wait $APP 2>/dev/null || true
kill -TERM $SRV; wait $SRV 2>/dev/null || true

python3 - "$T" "$SEND" <<'PY'
import json, sqlite3, sys
t, send = sys.argv[1], int(sys.argv[2])
srv = json.load(open(f"{t}/server.json"))
db = sqlite3.connect(f"{t}/home/data/nuncio.db")
ok = True
# Входящие от собеседника 100: множество id совпадает, дублей нет (ключ (peer_id, id)).
srv_in = sorted(m["id"] for m in srv if m["peer"] == 100 and not m["out"])
db_in = sorted(r[0] for r in db.execute("SELECT id FROM messages WHERE peer_id = 100 AND id > 0 AND out = 0"))
missing, extra = sorted(set(srv_in) - set(db_in)), sorted(set(db_in) - set(srv_in))
print(f"входящие: сервер {len(srv_in)}, база {len(db_in)}, потеряно {len(missing)}, лишних {len(extra)}")
ok &= not missing and not extra
# Исходящие: каждое «исходящее i» на сервере ровно один раз, в базе — одно настоящее.
srv_out = [m["text"] for m in srv if m["out"]]
counts = {f"исходящее {i}": srv_out.count(f"исходящее {i}") for i in range(1, send + 1)}
bad = {k: v for k, v in counts.items() if v != 1}
db_out = [r[0] for r in db.execute("SELECT text FROM messages WHERE peer_id = 100 AND id > 0 AND out = 1")]
left = db.execute("SELECT count(*) FROM outbox").fetchone()[0]
local = db.execute("SELECT count(*) FROM messages WHERE id < 0").fetchone()[0]
print(f"исходящие: на сервере {sum(counts.values())} из {send}, не ровно по одному: {len(bad)}; "
      f"в базе {len(db_out)}, осталось в очереди {left}, локальных строк {local}")
ok &= not bad and len(db_out) == send and len(set(db_out)) == send and left == 0 and local == 0
print("ИТОГ:", "без потерь и дублей" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
