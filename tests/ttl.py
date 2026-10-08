"""Проверки самоуничтожающихся вложений для tests/ttltest.sh.   ttl.py КАТАЛОГ PID"""
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
click(180, 84)                 # чат «Секрет»
time.sleep(2)
shot("1-feed")
import os, sqlite3


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


def cache_files():
    return [f for _, _, fs in os.walk(f"{t}/home/cache") for f in fs]


check(not cache_files(), "до нажатия ничего не скачано (без автоскачивания)")
click(403, 606)                # самоуничтожающееся фото
check(wait(lambda: len(windows()) >= 2), "открылось окно просмотра")
check(wait(lambda: "прочитано содержимое" in text(server)), "серверу — «содержимое просмотрено»")
check(wait(lambda: len(cache_files()) >= 1), "файл скачан для просмотра")
time.sleep(1)
v = [w for w in windows() if w.id != win.id][0]
vg = v.get_geometry()
vp = v.translate_coords(root, 0, 0)
subprocess.run(["import", "-window", "root", "-crop", f"{vg.width}x{vg.height}+{-vp.x}+{-vp.y}", f"{t}/2-viewer.png"], check=True)
# Закрыть окно просмотра: Esc.
from Xlib import XK
v.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()
k = d.keysym_to_keycode(XK.string_to_keysym("Escape"))
xtest.fake_input(d, X.KeyPress, k)
xtest.fake_input(d, X.KeyRelease, k)
d.sync()
check(wait(lambda: len(windows()) == 1), "окно просмотра закрыто")
check(wait(lambda: not cache_files()), f"файл удалён после просмотра ({cache_files()})")
db = sqlite3.connect(f"file:{t}/home/data/nuncio.db?mode=ro", uri=True)
check(db.execute("SELECT COUNT(*) FROM media").fetchone()[0] == 0, "в индексе кэша его нет")
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()
click(403, 670)                # истёкшее
time.sleep(0.8)
shot("3-expired")
print("ИТОГ:", "самоуничтожающиеся в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
