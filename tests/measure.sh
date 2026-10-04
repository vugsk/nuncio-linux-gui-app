#!/usr/bin/env bash
# Замер PSS и кадра (метод П-5, вехи В4, В5, …): release под Xvfb, открыт чат,
# через 35 с — полный кадр через take_snapshot (NUNCIO_BENCH_FRAMES), затем
# PSS процесса из smaps_rollup. Данные аккаунта копируются во временный
# каталог: настоящая база не меняется.
#
#   tests/measure.sh [каталог данных] [диалог: номер или self]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SRC=${1:-$HOME/.local/share/nuncio}
OPEN=${2:-self}
DISPLAY_NUM=${DISPLAY_NUM:-:99}
for size in 1100x800 1900x1000; do
  for run in 1 2; do
    T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-measure.XXXXXX")
    mkdir -p "$T/home/config" "$T/home/data"
    cp "$SRC/nuncio.db" "$SRC/session.bin" "$T/home/data/"
    cp "$HOME/.config/nuncio/api.env" "$T/home/config/"
    env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
      NUNCIO_WINDOW_SIZE="$size" NUNCIO_BENCH_OPEN="$OPEN" NUNCIO_BENCH_FRAMES=60 NUNCIO_DESKTOP_BUS=off \
      "$ROOT/target/release/nuncio" > "$T/app.log" 2>&1 &
    APP=$!
    # Процесс завершается при любом выходе из скрипта (иначе останется работать на копии данных).
    trap 'kill -TERM $APP 2>/dev/null || true' EXIT
    for _ in $(seq 80); do
      grep -q "кадр:" "$T/home/state/nuncio.log" 2>/dev/null && break
      sleep 0.5
    done
    sleep 2
    mem=$(awk '/^Pss:/ {p=$2} /^Pss_Anon:/ {a=$2} END {printf "PSS %.1f МБ (анонимная %.1f МБ)", p/1024, a/1024}' \
      "/proc/$APP/smaps_rollup")
    frame=$(grep -o "кадр: .*" "$T/home/state/nuncio.log" | head -1)
    opened=$(grep -c "замер: открыт диалог" "$T/home/state/nuncio.log" || true)
    echo "$size прогон $run: $mem; $frame; чат открыт: $([ "$opened" -gt 0 ] && echo да || echo НЕТ)"
    kill -TERM $APP; wait $APP 2>/dev/null || true
    rm -rf "$T"
  done
done
