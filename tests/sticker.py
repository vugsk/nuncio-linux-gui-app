"""Проверки стикеров для tests/stickertest.sh.   sticker.py КАТАЛОГ PID"""
import subprocess, sys, time
from Xlib import X, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root
PID_ATOM = d.intern_atom("_NET_WM_PID")


def find(w):
    try:
        p = w.get_full_property(PID_ATOM, X.AnyPropertyType)
        if p and int(p.value[0]) == PID:
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
    time.sleep(0.7)


def shot(name):
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", f"{t}/{name}.png"], check=True)


def text(path):
    try:
        return open(path, encoding="utf-8", errors="replace").read()
    except FileNotFoundError:
        return ""


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


server = f"{t}/server.log"
net = f"{t}/home/state/net.log"
click(180, 84)                 # чат «Стикеры»
time.sleep(2.5)
shot("1-feed")
from PIL import Image
img = Image.open(f"{t}/1-feed.png").convert("RGB")
# Стикер без пузыря: у края картинки — фон ленты, не рамка пузыря.
check(img.getpixel((473, 148))[0] > 200, f"статичный стикер виден: {img.getpixel((473, 148))}")
check(img.getpixel((400, 100)) == img.getpixel((1000, 100)), "прозрачный угол стикера — фон ленты (без пузыря)")
# TGS (второй стикер): кадры меняются (ThorVG), вокруг — фон ленты.
tbox = (392, 275, 554, 437)
ta = img.crop(tbox)
time.sleep(0.3)
tb = shot_img = None
shot("1t-feed")
tb = Image.open(f"{t}/1t-feed.png").convert("RGB")
check(ta.tobytes() != tb.crop(tbox).tobytes(), "TGS играет (кадры меняются)")
check(tb.getpixel((394, 277)) == tb.getpixel((1000, 277)), "у TGS прозрачный фон")
# Видеостикер: кадры меняются, вокруг круга — фон ленты (прозрачность VP9).
box = (392, 482, 554, 644)
a = img.crop(box)
time.sleep(0.45)
shot("1b-feed")
b = Image.open(f"{t}/1b-feed.png").convert("RGB")
check(a.tobytes() != b.crop(box).tobytes(), "видеостикер играет (кадры меняются)")
check(b.getpixel((396, 486)) == b.getpixel((1000, 486)), "у видеостикера прозрачный фон, без чёрного квадрата")
click(1032, 776)               # кнопка стикеров
check(wait(lambda: "наборы стикеров: 2" in text(net)), "наборы получены")
time.sleep(2)
shot("2-panel")
img = Image.open(f"{t}/2-panel.png").convert("RGB")
check(img.getpixel((407, 534))[0] > 200, "в «Недавних» — картинки стикеров")
click(557, 476)                # набор «Анимации»
time.sleep(2)
shot("3-set")
img = Image.open(f"{t}/3-set.png").convert("RGB")
check(img.getpixel((407, 534))[1] > 150, "набор «Анимации» загружен, превью видны")
click(407, 534)                # первый стикер набора — отправить
check(wait(lambda: "отправлен документ по ссылке: 9401" in text(server)), "стикер отправлен ссылкой на документ")
time.sleep(2)
click(1032, 776)               # закрыть панель
time.sleep(1)
shot("4-sent")
img = Image.open(f"{t}/4-sent.png").convert("RGB")
# Своё — справа: стикер у правого края ленты.
right = [img.getpixel((x, 620)) for x in range(900, 1080, 6)]
check(any(p[1] > 150 and p[0] < 150 for p in right), "отправленный стикер в ленте справа, без пузыря")
print("ИТОГ:", "стикеры в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
