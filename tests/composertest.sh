#!/usr/bin/env bash
# Гибридное поле ввода (Дз-17) настоящими клавишами: XTest (python-xlib) под
# Xvfb, латинская и русская раскладки (setxkbmap), форматирование горячими
# клавишами, своя отмена, отправка Enter. Проверка — по дампу поддельного
# сервера: текст и сущности (жирный, курсив) с верными смещениями UTF-16.
#
#   tests/composertest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44323}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-composertest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 20 -dump "$T/server.json" > "$T/server.log" 2>&1 &
SRV=$!
trap 'setxkbmap -display "$DISPLAY_NUM" -layout us 2>/dev/null || true; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
setxkbmap -display "$DISPLAY_NUM" -layout us
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-composertest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 NUNCIO_BENCH_OPEN=0 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do sleep 0.2; done
sleep 3

cat > "$T/x.py" <<'PY'
# Ввод через XTest. Коды клавиш физические (из латинской раскладки, сохраняются
# в keys.json): после смены раскладки python-xlib в том же соединении ломается,
# поэтому каждый этап — отдельный процесс.
import json, os, subprocess, sys, time
from Xlib import X, XK, display
from Xlib.ext import xtest

t, phase, PID = sys.argv[1], sys.argv[2], int(sys.argv[3])
d = display.Display()
root = d.screen().root

def find(w):
    # Окно нашего процесса (_NET_WM_PID): другое окно nuncio в том же Xvfb
    # иначе перехватило бы ввод.
    atom = d.intern_atom("_NET_WM_PID")
    try:
        prop = w.get_full_property(atom, X.AnyPropertyType)
        if prop and int(prop.value[0]) == PID:
            return w
    except Exception:
        pass
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r
    return None

win = find(root)
g = win.get_geometry()
pos = win.translate_coords(root, 0, 0)
ox, oy, W, H = -pos.x, -pos.y, g.width, g.height
table = f"{t}/keys.json"
NAMES = [chr(c) for c in range(ord("a"), ord("z") + 1)] + ["space", "comma", "period", "Return", "Escape", "Home",
    "End", "Left", "Right", "BackSpace", "Shift_L", "Control_L"]
if not os.path.exists(table):
    json.dump({n: d.keysym_to_keycode(XK.string_to_keysym(n)) for n in NAMES}, open(table, "w"))
codes = json.load(open(table))

def key(name, *mods):
    for m in mods:
        xtest.fake_input(d, X.KeyPress, codes[m])
    xtest.fake_input(d, X.KeyPress, codes[name])
    xtest.fake_input(d, X.KeyRelease, codes[name])
    for m in reversed(mods):
        xtest.fake_input(d, X.KeyRelease, codes[m])
    d.sync()
    time.sleep(0.05)

def typ(s):
    names = {" ": "space", ",": "comma", ".": "period"}
    for ch in s:
        key(names.get(ch, ch))

def button(b, x, y):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    xtest.fake_input(d, X.ButtonPress, b)
    xtest.fake_input(d, X.ButtonRelease, b)
    d.sync()
    time.sleep(0.4)

def shot(name):
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", f"{t}/{name}.png"], check=True)

if phase == "1":
    # Без оконного менеджера фокус клавиатуры X окну никто не даёт.
    win.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    time.sleep(0.3)
    button(1, 600, H - 24)
    typ("bold text plain")
    key("Home")
    for _ in range(4):
        key("Right", "Shift_L")     # выделить «bold»
    key("b", "Control_L")           # жирный (набор сразу после стиля его продолжил бы)
    key("End")
    time.sleep(1.5)                 # пауза — отдельный шаг отмены
    typ("xyz")
    key("z", "Control_L")           # своя отмена: убирает только «xyz»
elif phase == "2":
    typ(" ghbdtn")                  # в русской раскладке — « привет»
    key("Left", "Control_L", "Shift_L")   # выделить слово (родное у TextInput)
    key("i", "Control_L")           # Ctrl+Ш — курсив
    key("End")
    time.sleep(0.3)
    shot("1-formatted")
    key("Return")
    time.sleep(2)
elif phase == "3":
    shot("2-sent")
    # Ссылка: выделить всё, правая кнопка — панель форматирования (снимок),
    # затем Ctrl+K — диалог адреса, Enter — применить, Enter — отправить.
    typ("site")
    key("a", "Control_L")
    button(3, 450, H - 24)
    shot("3-format-panel")
    key("Escape")
    time.sleep(0.3)
    key("a", "Control_L")
    key("k", "Control_L")
    time.sleep(0.5)
    typ("example.org")
    key("Return")
    time.sleep(0.3)
    shot("4-link-applied")
    key("Return")
    time.sleep(2)
PY
run() { DISPLAY="$DISPLAY_NUM" python3 "$T/x.py" "$T" "$1" "$APP"; }
run 1
setxkbmap -display "$DISPLAY_NUM" -layout ru; sleep 0.5
run 2
setxkbmap -display "$DISPLAY_NUM" -layout us; sleep 0.5
run 3
kill -TERM $APP 2>/dev/null || true
sleep 1
kill -TERM $SRV; wait $SRV 2>/dev/null || true

python3 - "$T" <<'PY'
import json, sys
t = sys.argv[1]
srv = json.load(open(f"{t}/server.json"))
out = [m for m in srv if m["out"] and m["peer"] == 100 and "bold" in m["text"]]
ok = len(out) == 1
m = out[0] if out else {}
print("отправлено:", m.get("text"), m.get("entities"))
ok &= m.get("text") == "bold text plain привет"
ents = sorted(m.get("entities") or [])
ok &= ents == ["messageEntityBold 0 4", "messageEntityItalic 16 6"]
link = [m for m in srv if m["out"] and m["text"] == "site"]
print("ссылка:", link[0].get("entities") if link else None)
ok &= bool(link) and link[0].get("entities") == ["messageEntityTextUrl 0 4"]
print("ИТОГ:", "поле ввода в порядке" if ok else "ОШИБКА")
print("результат:", t)
sys.exit(0 if ok else 1)
PY
