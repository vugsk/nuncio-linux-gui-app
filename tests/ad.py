"""Проверки рекламы для tests/adtest.sh.   ad.py КАТАЛОГ PID"""
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


net = f"{t}/home/state/net.log"
server = f"{t}/server.log"
click(180, 84)                 # бот «Новости»
check(wait(lambda: "реклама: 1" in text(net)), "реклама запрошена и получена")
time.sleep(1.5)
shot("1-line")
check(wait(lambda: "реклама: просмотр" in text(server)), "просмотр отмечен (строка в окне)")
click(600, 728)                # строка — раскрыть
time.sleep(1)
shot("2-expanded")
click(410, 728)                # «Открыть →»
check(wait(lambda: "https://example.com/ad" in text(f"{t}/opened.txt")), "ссылка рекламы открыта (в проверке — в файл)")
check(wait(lambda: "реклама: щелчок" in text(server)), "щелчок отмечен")
click(1046, 686)               # ⓘ
time.sleep(0.8)
shot("3-info")
click(658, 463)                # «Закрыть»
click(1064, 686)               # ✕ — причины
time.sleep(1)
shot("4-report")
click(440, 367)                # «Не интересно»
check(wait(lambda: "реклама: жалоба 1" in text(server)), "жалоба с выбранной причиной отправлена")
time.sleep(1)
shot("5-hidden")
from PIL import Image
img = Image.open(f"{t}/5-hidden.png").convert("RGB")
# После жалобы строки рекламы нет: у нижнего края ленты пусто (фон), последняя — «новость 3».
row = [img.getpixel((x, 728)) for x in range(900, 1060, 4)]
check(len(set(row)) <= 2, "строка рекламы исчезла")
before = text(net).count("реклама:")
click(180, 148)                # личный чат «Отправитель»
time.sleep(2)
check(text(net).count("реклама:") == before, "в личном чате рекламу не запрашивали")
print("ИТОГ:", "реклама в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
