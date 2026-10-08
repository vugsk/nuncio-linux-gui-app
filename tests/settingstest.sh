#!/usr/bin/env bash
# Веха В9 под Xvfb против поддельного Telegram: окно настроек (меню ☰ →
# «Настройки»), разделы и сохранение значений в config.toml (тема, размер
# текста, язык, уведомления, лимит кэша), профиль (загрузка, правка, ошибка
# «имя занято»), активные сеансы (список, завершение). Проверки — по
# config.toml, журналу поддельного сервера и снимкам (синтетические данные).
#
#   tests/settingstest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44328}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-settingstest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -history 5 -unread -1 > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-settingstest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1100x800 \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do kill -0 $APP 2>/dev/null || { echo "приложение завершилось:"; tail -5 "$T/app.log"; exit 1; }; sleep 0.2; done
sleep 3

DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" <<'PY'
import os, subprocess, sys, time, tomllib
from PIL import Image
from Xlib import X, XK, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
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

win = find(root)
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
    time.sleep(0.5)

def key(name, ctrl=False):
    k = d.keysym_to_keycode(XK.string_to_keysym(name))
    c = d.keysym_to_keycode(XK.string_to_keysym("Control_L"))
    if ctrl:
        xtest.fake_input(d, X.KeyPress, c)
    xtest.fake_input(d, X.KeyPress, k)
    xtest.fake_input(d, X.KeyRelease, k)
    if ctrl:
        xtest.fake_input(d, X.KeyRelease, c)
    d.sync()
    time.sleep(0.05)

def typ(s):
    for ch in s:
        key(ch)

def shot(name):
    path = f"{t}/{name}.png"
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", path], check=True)
    return Image.open(path).convert("RGB")

def cfg():
    try:
        return tomllib.load(open(f"{t}/home/config/config.toml", "rb"))
    except FileNotFoundError:
        return {}

def app():
    return cfg().get("app", {})

def server():
    return open(f"{t}/server.log", encoding="utf-8").read()

def wait(cond, secs=8):
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

# Окно настроек 820×600 по центру: разделы слева (по 40 px), содержимое справа.
CX, CY = (W - 820) // 2, (H - 600) // 2
def section(i):
    click(CX + 100, CY + 53 + 10 + i * 40 + 19)
    time.sleep(0.8)
X0, Y0 = CX + 201 + 22, CY + 53 + 22     # начало содержимого раздела

click(30, 26)                 # ☰
click(100, 56 + 6 + 18)       # «Настройки»
time.sleep(1)
shot("1-appearance")
# Тема «Светлая» (третья из четырёх в полосе 420 px под заголовком).
click(X0 + 52 + 2 * 105, Y0 + 16 + 12 + 18)
check(wait(lambda: app().get("theme") == "light"), f"тема «Светлая» сохранена: {app().get('theme')}")
img = shot("2-light")
bg = img.getpixel((W - 20, H // 2))
check(sum(bg) > 600, f"фон светлый: {bg}")
# Размер текста — правый край ползунка (20 px).
click(X0 + 9 + 12 + 238, Y0 + 16 + 12 + 36 + 12 + 16 + 12 + 12)
check(wait(lambda: app().get("text_size") == 20), f"размер текста 20 px: {app().get('text_size')}")
# Тема снова тёмная.
click(X0 + 52 + 1 * 105, Y0 + 16 + 12 + 18)
check(wait(lambda: app().get("theme") == "dark"), "тема «Тёмная»")

# Уведомления: третий переключатель — звук.
section(2)
shot("3-notifications")
# Громкость: середина ползунка (под «Звуком уведомления»), проба звука — в журнал.
click(CX + 436, CY + 208)
check(wait(lambda: 45 <= app().get("notify_volume", -1) <= 55), f"громкость ~50 %: {app().get('notify_volume')}")
check(wait(lambda: "звук уведомления" in open(f"{t}/home/state/nuncio.log", encoding="utf-8").read()), "проба звука при отпускании")
click(X0 + 11, Y0 + 33 + 14 + 17 + 14 + 13)
check(wait(lambda: app().get("notify_sound") is False), f"звук уведомлений выключен: {app().get('notify_sound')}")

# Данные и кэш: лимит 2 ГБ (третий вариант).
section(3)
shot("4-data")
click(X0 + 42 + 2 * 84, Y0 + 16 + 12 + 17 + 12 + 16 + 12 + 18)
check(wait(lambda: app().get("cache_limit_mb") == 2048), f"лимит кэша 2 ГБ: {app().get('cache_limit_mb')}")

# Профиль: загрузка, фамилия, сохранение; затем занятое имя пользователя.
section(0)
time.sleep(1)
shot("5-profile")
LAST = Y0 + 64 + 14 + 16 + 10 + 44 + 10 + 16 + 10 + 22     # поле «Фамилия»
click(X0 + 150, LAST)
typ("Ivanov")
SAVE = LAST + 22 + 10 + 16 + 10 + 44 + 10 + 16 + 10 + 44 + 10 + 16 + 10 + 22
click(X0 + 80, SAVE)
check(wait(lambda: "профиль: Тест|ivanov|" in server()), "фамилия сохранена на сервере")
USER = LAST + 22 + 10 + 16 + 10 + 44 + 10 + 16 + 10 + 22
click(X0 + 150, USER)
typ("taken")
click(X0 + 80, SAVE + 20)
time.sleep(1.5)
shot("6-profile-error")

# Аккаунт: три сеанса, завершить второй.
section(5)
time.sleep(1.5)
img = shot("7-account")
click(CX + 820 - 22 - 30, Y0 + 16 + 12 + 50 + 4 + 25)
check(wait(lambda: "сеансов: 2" in server()), "сеанс завершён")
time.sleep(1)
shot("8-account-after")

# Язык: English, затем снова русский.
section(1)
click(X0 + 65 + 130, Y0 + 16 + 12 + 36 + 12 + 16 + 12 + 24 + 12 + 40 + 12 + 16 + 12 + 18)
check(wait(lambda: cfg().get("lang") == "en"), f"язык English сохранён: {cfg().get('lang')}")
shot("9-english")
click(X0 + 65, Y0 + 16 + 12 + 36 + 12 + 16 + 12 + 24 + 12 + 40 + 12 + 16 + 12 + 18)
check(wait(lambda: cfg().get("lang") == "ru"), "язык снова русский")
before = shot("9b-before-esc")
key("Escape")
time.sleep(0.8)
after = shot("10-closed")
from PIL import ImageChops
box = ImageChops.difference(before, after).getbbox()
check(box is not None and (box[2] - box[0]) * (box[3] - box[1]) > W * H // 3, f"Esc закрыл окно настроек: изменилось {box}")
print("ИТОГ:", "настройки в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
kill -TERM $APP 2>/dev/null || true
