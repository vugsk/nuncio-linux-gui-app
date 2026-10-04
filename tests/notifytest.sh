#!/usr/bin/env bash
# Веха В9 под Xvfb против поддельного Telegram: уведомления рабочего стола.
# Своя шина D-Bus с поддельным сервером уведомлений (на рабочий стол
# человека ничего не попадает). Входящие из неоткрытого чата — уведомление,
# следующее заменяет прежнее (одно на чат); щелчок открывает чат; входящее
# в открытый чат в окне в фокусе — без уведомления.
#
#   tests/notifytest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44329}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-notifytest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver && go build -o "$T/fakenotify" ./cmd/fakenotify)
# Входящие раз в 6 с: первое и второе — до щелчка, третье — в открытый чат.
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 5 -unread -1 -start 4s -push 3 -every 6s \
  > "$T/server.log" 2>&1 &
SRV=$!
dbus-daemon --session --nofork --nopidfile --print-address=3 --address="unix:path=$T/bus" 3> "$T/bus.addr" &
BUS=$!
NTF=
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV $BUS $NTF 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; rm -f "$T/fakenotify"; tmp_done $rc' EXIT
until [ -s "$T/bus.addr" ]; do sleep 0.1; done
ADDR=$(head -1 "$T/bus.addr")
DBUS_SESSION_BUS_ADDRESS=$ADDR "$T/fakenotify" > "$T/notify.log" 2>&1 &
NTF=$!
until grep -q готов "$T/server.log" && grep -q готов "$T/notify.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-notifytest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_NOTIFY_BUS="$ADDR" \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done

DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" "$NTF" <<'PY'
import os, re, signal, subprocess, sys, time
from PIL import Image, ImageChops
from Xlib import X, display

t, PID, NTF = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
d = display.Display()
root = d.screen().root
PID_ATOM = d.intern_atom("_NET_WM_PID")

def find(w):
    try:
        prop = w.get_full_property(PID_ATOM, X.AnyPropertyType)
        if prop and int(prop.value[0]) == PID:
            return w
    except Exception:
        pass
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r

win = None
while win is None:
    win = find(root)
    time.sleep(0.2)
g = win.get_geometry()
pos = win.translate_coords(root, 0, 0)
ox, oy, W, H = -pos.x, -pos.y, g.width, g.height
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()

def shot(name):
    path = f"{t}/{name}.png"
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", path], check=True)
    return Image.open(path).convert("RGB")

def notes():
    return re.findall(r"^уведомление (.*)$", open(f"{t}/notify.log", encoding="utf-8").read(), re.M)

def pushed():
    return open(f"{t}/server.log", encoding="utf-8").read().count("\npush ")

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

check(wait(lambda: len(notes()) >= 1), "первое входящее — уведомление")
n = notes()
check(n and "title=Отправитель" in n[0] and "body=входящее 1" in n[0] and "sound=true" in n[0]
      and "replaces=0" in n[0] and "actions=[default ]" in n[0], f"заголовок, текст, звук, действие: {n[:1]}")
first = re.search(r"id=(\d+)", n[0]).group(1) if n else "?"
check(wait(lambda: len(notes()) >= 2), "второе входящее — уведомление")
n = notes()
check(len(n) >= 2 and f"replaces={first}" in n[1] and "body=входящее 2" in n[1], f"заменяет прежнее (одно на чат): {n[1:2]}")
before = shot("1-before-click")
os.kill(NTF, signal.SIGUSR1)
time.sleep(1.5)
after = shot("2-after-click")
box = ImageChops.difference(before.crop((W // 2, 0, W, H)), after.crop((W // 2, 0, W, H))).getbbox()
check(box is not None, f"щелчок открыл чат: изменилось {box}")
count = len(notes())
check(wait(lambda: pushed() >= 3, 10), "третье входящее отправлено")
time.sleep(2)
check(len(notes()) == count, f"в открытый чат в окне в фокусе — без уведомления ({len(notes())} уведомлений)")
print("ИТОГ:", "уведомления в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
kill -TERM $APP 2>/dev/null || true
