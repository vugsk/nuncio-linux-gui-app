#!/usr/bin/env bash
# Стикеры под Xvfb против поддельного Telegram (-stickers): в ленте — без
# пузыря, с прозрачностью (статичный WebP и превью «анимированного» TGS);
# панель над полем ввода — «Недавние» и наборы, щелчок отправляет стикер
# ссылкой на документ. Картинки стикеров — синтетические (Pillow, WebP с альфой).
#
#   tests/stickertest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44342}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-stickertest.XXXXXX")
mkdir -p "$T/home/config" "$T/stickers"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
python3 - "$T/stickers" <<'PY'
import sys
from PIL import Image, ImageDraw
for i, c in enumerate([(240, 80, 80), (80, 200, 120), (90, 140, 250), (240, 200, 60)]):
    im = Image.new("RGBA", (512, 512), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.ellipse((40, 40, 472, 472), fill=c + (255,))
    d.ellipse((150, 170, 210, 230), fill=(20, 20, 20, 255))
    d.ellipse((302, 170, 362, 230), fill=(20, 20, 20, 255))
    d.arc((150, 230, 362, 400), 20, 160, fill=(20, 20, 20, 255), width=18)
    im.save(f"{sys.argv[1]}/s{i}.webp", "WEBP", lossless=True)
# Видеостикер: синий круг ездит по прозрачному полю (кадры PNG → WebM VP9 с альфой).
for f in range(10):
    im = Image.new("RGBA", (256, 256), (0, 0, 0, 0))
    x = 20 + f * 15
    ImageDraw.Draw(im).ellipse((x, 78, x + 100, 178), fill=(60, 120, 240, 255))
    im.save(f"{sys.argv[1]}/f{f:02d}.png")
    if f == 0:
        im.save(f"{sys.argv[1]}/v0.webp", "WEBP", lossless=True)
PY
ffmpeg -hide_banner -loglevel error -y -framerate 10 -i "$T/stickers/f%02d.png" -c:v libvpx-vp9 -pix_fmt yuva420p \
  -auto-alt-ref 0 "$T/stickers/v0.webm"
(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 3 -unread -1 -stickers "$T/stickers" > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-stickertest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 3
DISPLAY="$DISPLAY_NUM" python3 "$ROOT/tests/sticker.py" "$T" "$APP"
kill -TERM $APP 2>/dev/null || true
