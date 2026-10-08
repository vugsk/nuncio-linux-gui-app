#!/usr/bin/env bash
# Замер «Графика: GPU» (Нс-8) на настоящем экране и видеокарте, метод П-5:
# поддельный сервер (синтетическая история), открыт чат, через 35 с — полный
# кадр через take_snapshot (у GPU включает чтение пикселей обратно, цена
# завышена), затем PSS из smaps_rollup. Режимы: software и Skia (настройка
# gpu в config.toml). Трей и уведомления выключены; окно появляется на
# рабочем столе примерно на 40 с за прогон. Разверните его на весь экран
# сразу после появления, чтобы сравнить с П-5 (2560×1440). Окно не закрывать:
# оно закрывается само после замера.
#
#   tests/gpumeasure.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=${PORT:-44333}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-gpumeasure.XXXXXX")
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 300 -unread -1 > "$T/server.log" 2>&1 &
SRV=$!
APP=
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; [ -n "$APP" ] && kill -TERM $APP 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
for mode in software gpu; do
  for run in 1 2; do
    H="$T/$mode-$run/home"
    mkdir -p "$H/config"
    printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$H/config/api.env"
    chmod 600 "$H/config/api.env"
    printf '[app]\ngpu = %s\n' "$([ $mode = gpu ] && echo true || echo false)" > "$H/config/config.toml"
    env -u SLINT_BACKEND NUNCIO_HOME="$H" NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" \
      NUNCIO_KEYRING_APP=nuncio-gpumeasure NUNCIO_KEYRING_COLLECTION=session NUNCIO_DESKTOP_BUS=off \
      NUNCIO_BENCH_OPEN=0 NUNCIO_BENCH_FRAMES=60 "$ROOT/target/release/nuncio" > "$H/../app.log" 2>&1 &
    APP=$!
    for _ in $(seq 100); do
      grep -q "кадр:" "$H/state/nuncio.log" 2>/dev/null && break
      kill -0 $APP 2>/dev/null || break
      sleep 0.5
    done
    sleep 2
    mem=$(awk '/^Pss:/ {p=$2} /^Pss_Anon:/ {a=$2} END {printf "PSS %.1f МБ (анонимная %.1f МБ)", p/1024, a/1024}' \
      "/proc/$APP/smaps_rollup" 2>/dev/null || echo "процесс завершился")
    renderer=$(grep -o "рендерер: .*" "$H/state/nuncio.log" | tail -1 || true)
    frame=$(grep -o "кадр: .*" "$H/state/nuncio.log" | head -1 || true)
    # Окно закрыли раньше замера (закрытие без трея завершает приложение).
    [ -n "$frame" ] || frame="кадр не замерен: окно закрылось раньше 35 с"
    echo "$mode прогон $run: $renderer; $mem; $frame"
    kill -TERM $APP 2>/dev/null || true
    wait $APP 2>/dev/null || true
    APP=
  done
done
