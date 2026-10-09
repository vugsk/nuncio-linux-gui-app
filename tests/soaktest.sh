#!/usr/bin/env bash
# Длительный прогон (В10: не меньше суток без роста памяти) под Xvfb против
# поддельного Telegram: длинная история, вложения, стикеры, входящие раз в
# 30 с; своя шина D-Bus с поддельными уведомлениями и треем. Действия раз в
# минуту по кругу: смена чатов, прокрутка, «Сведения», панель стикеров, набор
# и отправка. Раз в 10 мин — PSS, анонимная, RSS, потоки, открытые файлы,
# время процессора (samples.csv); в конце — вывод о росте памяти (soak.py).
#
#   HOURS=24 tests/soaktest.sh            (по умолчанию 24 ч)
#   setsid nohup tests/soaktest.sh > soak.log 2>&1 &   — отдельно от терминала
#
# Каталог прогона не удаляется: в нём samples.csv и итог (report.txt).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
HOURS=${HOURS:-24}
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44343}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-soak.XXXXXX")
echo "каталог прогона: $T"
mkdir -p "$T/home/config" "$T/stickers"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
python3 - "$T/stickers" <<'PY'
import gzip, json, sys
from PIL import Image, ImageDraw
for i, c in enumerate([(240, 80, 80), (80, 200, 120), (90, 140, 250), (240, 200, 60)]):
    im = Image.new("RGBA", (512, 512), (0, 0, 0, 0))
    ImageDraw.Draw(im).ellipse((40, 40, 472, 472), fill=c + (255,))
    im.save(f"{sys.argv[1]}/s{i}.webp", "WEBP", lossless=True)
for f in range(10):
    im = Image.new("RGBA", (256, 256), (0, 0, 0, 0))
    ImageDraw.Draw(im).ellipse((20 + f * 15, 78, 120 + f * 15, 178), fill=(60, 120, 240, 255))
    im.save(f"{sys.argv[1]}/f{f:02d}.png")
    if f == 0:
        im.save(f"{sys.argv[1]}/v0.webp", "WEBP", lossless=True)
lottie = {"v": "5.7.4", "fr": 30, "ip": 0, "op": 30, "w": 512, "h": 512, "layers": [{"ty": 4, "ind": 1, "ip": 0,
    "op": 30, "st": 0, "ks": {"o": {"a": 0, "k": 100}, "r": {"a": 0, "k": 0},
    "p": {"a": 1, "k": [{"t": 0, "s": [130, 256, 0]}, {"t": 30, "s": [382, 256, 0]}]},
    "a": {"a": 0, "k": [0, 0, 0]}, "s": {"a": 0, "k": [100, 100, 100]}},
    "shapes": [{"ty": "rc", "p": {"a": 0, "k": [0, 0]}, "s": {"a": 0, "k": [200, 200]}, "r": {"a": 0, "k": 40}},
               {"ty": "fl", "c": {"a": 0, "k": [0.3, 0.8, 0.45, 1]}, "o": {"a": 0, "k": 100}}]}]}
with gzip.open(f"{sys.argv[1]}/t0.tgs", "wb") as g:
    g.write(json.dumps(lottie).encode())
PY
ffmpeg -hide_banner -loglevel error -y -framerate 10 -i "$T/stickers/f%02d.png" -c:v libvpx-vp9 -pix_fmt yuva420p \
  -auto-alt-ref 0 "$T/stickers/v0.webm"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver && go build -o "$T/fakedesk" ./cmd/fakedesk)
PUSH=$(python3 -c "print(int(float('$HOURS') * 3600 / 30) + 10)")
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 500 -unread -1 -extras -media \
  -stickers "$T/stickers" -start 20s -push "$PUSH" -every 30s > "$T/server.log" 2>&1 &
SRV=$!
BUS= DESK= APP=
source "$ROOT/tests/common.sh"
NUNCIO_KEEP=1   # итог нужен и при успехе
trap 'rc=$?; kill $SRV $BUS $DESK 2>/dev/null || true; [ -n "$APP" ] && kill -TERM $APP 2>/dev/null || true; rm -f "$T/fakedesk"; tmp_done $rc' EXIT
private_bus "$T"
ADDR=$(head -1 "$T/bus.addr")
DBUS_SESSION_BUS_ADDRESS=$ADDR "$T/fakedesk" -tray > "$T/desk.log" 2>&1 &
DESK=$!
until grep -q готов "$T/server.log" && grep -q готов "$T/desk.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-soak \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_DESKTOP_BUS="$ADDR" \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось"; exit 1; }; sleep 0.2; done

DISPLAY="$DISPLAY_NUM" python3 "$ROOT/tests/soak.py" "$T" "$APP" "$HOURS"
