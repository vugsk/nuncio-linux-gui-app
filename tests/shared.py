"""Проверки вкладок общих медиа для tests/sharedtest.sh.

    shared.py КАТАЛОГ PID
"""
import re, subprocess, sys, time
from Xlib import X, display
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
    time.sleep(0.6)


def shot(name):
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", f"{t}/{name}.png"], check=True)


def log():
    return open(f"{t}/home/state/nuncio.log", encoding="utf-8", errors="replace").read()


def wait(cond, secs=10):
    for _ in range(int(secs / 0.25)):
        if cond():
            return True
        time.sleep(0.25)
    return False


def loaded(kind):
    m = re.findall(rf"общие медиа: {kind} (\d+) из (\d+)", log())
    return (int(m[-1][0]), int(m[-1][1])) if m else None


ok = True


def check(cond, what):
    global ok
    print(("  ок: " if cond else "  ОШИБКА: ") + what)
    ok &= bool(cond)


click(180, 52 + 32)            # чат «Вложения»
time.sleep(1.5)
click(360 + 120, 26)           # заголовок — «Сведения»
check(wait(lambda: loaded("Media") is not None), f"вкладка «Медиа» открылась сама: {loaded('Media')}")
time.sleep(2)
shot("1-media")
n = loaded("Media")
check(n and n[0] > 0, "в сетке есть фото и видео")
TABS = 270                     # строка вкладок: «Медиа», «Файлы», «Ссылки», «Голосовые»
click(W - 282, TABS)           # «Файлы»
check(wait(lambda: loaded("Files") is not None), f"«Файлы»: {loaded('Files')}")
time.sleep(1)
shot("2-files")
click(W - 218, TABS)           # «Ссылки»
check(wait(lambda: loaded("Links") is not None), f"«Ссылки»: {loaded('Links')}")
time.sleep(1)
shot("3-links")
click(W - 142, TABS)           # «Голосовые»
check(wait(lambda: loaded("Voice") is not None), f"«Голосовые»: {loaded('Voice')}")
time.sleep(1)
shot("4-voice")
f, l = loaded("Files"), loaded("Links")
check(f and f[0] >= 1 and l and l[0] >= 1, "в «Файлах» и «Ссылках» есть строки")
# Снова «Медиа» и щелчок по первой ячейке — окно просмотра.
click(W - 342, TABS)
time.sleep(1.5)
click(W - 380 + 12 + 58, TABS + 26 + 58)
check(wait(lambda: len(windows()) >= 2, 8), f"окно просмотра открылось из сетки (окон: {len(windows())})")
time.sleep(1)
for w in windows():
    if w.id != win.id:
        vg = w.get_geometry()
        vp = w.translate_coords(root, 0, 0)
        subprocess.run(["import", "-window", "root", "-crop", f"{vg.width}x{vg.height}+{-vp.x}+{-vp.y}", f"{t}/5-viewer.png"], check=True)
print("ИТОГ:", "общие медиа в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
