#!/usr/bin/env bash
# Веха В8 под Xvfb против поддельного Telegram (-video/-voice: чат «Видео» с
# роликом H.264 6 с и голосовым Opus 3 с — звук в них тишина: приложение
# выводит звук на настоящее устройство): видео открывается в окне просмотра,
# кадры идут, пробел ставит паузу, Esc закрывает, память возвращается;
# голосовое играет в ленте до конца. Проверки — по снимкам окна, журналу
# приложения (служебные строки) и PSS.
#
#   tests/playtest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44327}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-playtest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"
ff="ffmpeg -hide_banner -loglevel error -y"
$ff -f lavfi -i testsrc2=size=640x360:rate=30 -f lavfi -i anullsrc=r=48000:cl=stereo -t 6 \
  -c:v libx264 -preset veryfast -pix_fmt yuv420p -c:a aac "$T/video.mp4"
$ff -f lavfi -i anullsrc=r=48000:cl=mono -t 3 -c:a libopus "$T/voice.ogg"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -unread -1 -video "$T/video.mp4" -voice "$T/voice.ogg" \
  > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-playtest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 3

DISPLAY="$DISPLAY_NUM" RUNS="${RUNS:-2}" python3 - "$T" "$APP" <<'PY'
import os, subprocess, sys, time
from PIL import Image, ImageChops
from Xlib import X, XK, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root
PID_ATOM = d.intern_atom("_NET_WM_PID")

def windows(w=root, out=None):
    out = [] if out is None else out
    try:
        prop = w.get_full_property(PID_ATOM, X.AnyPropertyType)
        if prop and int(prop.value[0]) == PID and w.get_attributes().map_state == X.IsViewable:
            out.append(w)
            return out
    except Exception:
        pass
    for c in w.query_tree().children:
        windows(c, out)
    return out

win = windows()[0]
g = win.get_geometry()
pos = win.translate_coords(root, 0, 0)
ox, oy, W, H = -pos.x, -pos.y, g.width, g.height
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()

def click(x, y):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    xtest.fake_input(d, X.ButtonPress, 1)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    time.sleep(0.4)

def key(name):
    k = d.keysym_to_keycode(XK.string_to_keysym(name))
    xtest.fake_input(d, X.KeyPress, k)
    xtest.fake_input(d, X.KeyRelease, k)
    d.sync()
    time.sleep(0.3)

def grab(w, name):
    gg = w.get_geometry()
    p = w.translate_coords(root, 0, 0)
    path = f"{t}/{name}.png"
    subprocess.run(["import", "-window", "root", "-crop", f"{gg.width}x{gg.height}+{-p.x}+{-p.y}", path], check=True)
    return Image.open(path).convert("RGB")

def log():
    return open(f"{t}/home/state/nuncio.log", encoding="utf-8").read()

def smaps():
    m = {}
    for line in open(f"/proc/{PID}/smaps_rollup"):
        k, _, v = line.partition(":")
        if v.strip().endswith("kB"):
            m[k] = int(v.split()[0]) / 1024
    return m

def pss():
    m = smaps()
    print(f"    (PSS {m['Pss']:.1f}: анонимная {m['Pss_Anon']:.1f}, файлы {m['Pss_File']:.1f})")
    return m["Pss"]

def wait(cond, secs=15):
    for _ in range(int(secs / 0.25)):
        if cond():
            return True
        time.sleep(0.25)
    return False

ok = True
def check(cond, what):
    global ok
    print(("  ок: " if cond else "  ОШИБКА: ") + what)
    ok &= bool(cond)

LINK = (0x6a, 0xb3, 0xf3)
def buttons(img):
    """Круглые кнопки файлов (цвет ссылки, 40 px): центры сверху вниз."""
    px = img.load()
    found = []
    for y in range(60, img.height - 60, 2):
        run = 0
        for x in range(380, img.width):
            c = px[x, y]
            if all(abs(a - b) <= 6 for a, b in zip(c, LINK)):
                run += 1
                continue
            if 30 <= run <= 42 and not any(abs(fy - y) < 30 for _, fy in found):
                found.append((x - run // 2, y))
            run = 0
    return sorted(found, key=lambda b: b[1])

click(180, 52 + 32)          # чат «Видео» — первый в списке
time.sleep(2)
img = grab(win, "1-chat")
bs = buttons(img)
check(len(bs) == 2, f"в чате ролик и голосовое: кнопки {bs}")
if len(bs) != 2:
    print("ИТОГ: ОШИБКА"); sys.exit(1)

# Видео: окно просмотра, кадры идут, пауза; дважды — для проверки утечки.
base = smaps()
afters = []
for run in range(1, int(os.environ.get("RUNS", "2")) + 1):
    win.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    n = log().count("воспроизведение: видео")
    click(*bs[0])
    check(wait(lambda: len(windows()) == 2, 10), f"видео открылось в окне просмотра ({run})")
    check(wait(lambda: log().count("воспроизведение: видео") > n, 15), f"видео скачано и играет ({run})")
    v = [w for w in windows() if w.id != win.id][0]
    time.sleep(1.2)
    a = grab(v, f"2-video-a{run}")
    time.sleep(1.0)
    b = grab(v, f"3-video-b{run}")
    check(ImageChops.difference(a, b).getbbox() is not None, f"кадры меняются ({run})")
    during = smaps()
    v.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    key("space")             # пауза
    time.sleep(0.5)
    c = grab(v, f"4-paused-a{run}")
    time.sleep(1.0)
    e = grab(v, f"5-paused-b{run}")
    check(ImageChops.difference(c, e).getbbox() is None, f"пробел — пауза: кадр стоит ({run})")
    key("space")
    key("Escape")
    check(wait(lambda: len(windows()) == 1, 5), f"Esc закрыл окно просмотра ({run})")
    time.sleep(2.5)
    afters.append(smaps())
    names = sorted(open(f"/proc/{PID}/task/{x}/comm").read().strip() for x in os.listdir(f"/proc/{PID}/task"))
    print(f"  потоков после закрытия ({run}): {len(names)}: {names}")
    print(f"  PSS ({run}): до {base['Pss']:.1f}, во время {during['Pss']:.1f}, после закрытия {afters[-1]['Pss']:.1f} МБ; "
          f"анонимная {base['Pss_Anon']:.1f} → {during['Pss_Anon']:.1f} → {afters[-1]['Pss_Anon']:.1f}, "
          f"файлы {base['Pss_File']:.1f} → {afters[-1]['Pss_File']:.1f}")
grow = afters[1]["Pss_Anon"] - afters[0]["Pss_Anon"]
check(grow < 2, f"второй просмотр не добавил анонимной памяти (утечки нет): {grow:+.1f} МБ")
check(afters[0]["Pss_Anon"] - base["Pss_Anon"] < 12,
      f"после закрытия анонимная память близка к исходной: {afters[0]['Pss_Anon'] - base['Pss_Anon']:+.1f} МБ")

# Голосовое: играет в ленте до конца (3 с).
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()
n = log().count("воспроизведение закончено")
click(*bs[1])
check(wait(lambda: "воспроизведение: звук" in log(), 10), "голосовое скачано и играет")
time.sleep(1)
grab(win, "6-voice-playing")
check(wait(lambda: log().count("воспроизведение закончено") > n, 8), "голосовое доиграло до конца")
print("ИТОГ:", "воспроизведение в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
kill -TERM $APP 2>/dev/null || true
