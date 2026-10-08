#!/usr/bin/env bash
# Штатный выход без падения, когда в ленте живы плееры (GIF играет сам), а
# звук уведомления уже открыл общий аудиовывод (порядок уничтожения
# thread_local при выходе: раньше — «fatal runtime error», код 134).
# Звук темы подменён тишиной (XDG_DATA_DIRS): на колонки ничего не идёт.
#
#   tests/exittest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44338}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-exittest.XXXXXX")
mkdir -p "$T/home/config" "$T/share/sounds/freedesktop/stereo"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
ff="ffmpeg -hide_banner -loglevel error -y"
$ff -f lavfi -i anullsrc=r=48000:cl=stereo -t 0.5 -c:a libvorbis "$T/share/sounds/freedesktop/stereo/message-new-instant.oga"
$ff -f lavfi -i testsrc2=size=640x360:rate=30 -f lavfi -i anullsrc=r=48000:cl=stereo -t 6 \
  -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac "$T/video.mp4"
$ff -f lavfi -i anullsrc=r=48000:cl=mono -t 3 -c:a libopus "$T/voice.ogg"
$ff -f lavfi -i testsrc=size=320x240:rate=25 -t 3 -c:v libx264 -preset veryfast -pix_fmt yuv420p -an "$T/gif.mp4"
$ff -f lavfi -i testsrc2=size=384x384:rate=30 -f lavfi -i anullsrc=r=48000:cl=mono -t 4 -c:v libx264 -preset veryfast \
  -pix_fmt yuv420p -c:a aac "$T/round.mp4"
$ff -i "$T/gif.mp4" -frames:v 1 "$T/gif.jpg"
$ff -i "$T/round.mp4" -frames:v 1 "$T/round.jpg"
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -unread -1 -video "$T/video.mp4" -voice "$T/voice.ogg" \
  -start 8s -push 1 > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY -u NUNCIO_TEST_SILENT DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-exittest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_BENCH_OPEN=0 \
  XDG_DATA_HOME="$T/share" XDG_DATA_DIRS="$T/share" "$T/app" > "$T/app.log" 2>&1 &
APP=$!
ok=1
for _ in $(seq 120); do grep -q "звук уведомления" "$T/home/state/nuncio.log" 2>/dev/null && break; sleep 0.25; done
grep -q "звук уведомления" "$T/home/state/nuncio.log" && echo "  ок: звук уведомления проигран (тишина)" || { echo "  ОШИБКА: звука уведомления не было"; ok=0; }
grep -q "воспроизведение\|автовоспроизведение\|кружок" "$T/home/state/nuncio.log" || true
sleep 1
kill -TERM $APP
set +e
wait $APP
rc=$?
set -e
[ $rc = 0 ] && echo "  ок: выход с кодом 0" || { echo "  ОШИБКА: код выхода $rc"; ok=0; }
grep -q "fatal runtime error\|panicked" "$T/app.log" && { echo "  ОШИБКА: паника при выходе"; ok=0; } || echo "  ок: без паники при выходе"
[ $ok = 1 ] && echo "ИТОГ: выход в порядке" || { echo "ИТОГ: ОШИБКА"; exit 1; }
