"""Длительный прогон для tests/soaktest.sh: действия раз в минуту, замеры раз в
10 мин (samples.csv), в конце — вывод о росте памяти (report.txt).

    soak.py КАТАЛОГ PID ЧАСЫ
"""
import os, statistics, sys, time
from Xlib import X, XK, display
from Xlib.ext import xtest

t, PID, HOURS = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
d = display.Display()
root = d.screen().root
A = d.intern_atom("_NET_WM_PID")


def find(w):
    try:
        p = w.get_full_property(A, X.AnyPropertyType)
        if p and int(p.value[0]) == PID and w.get_attributes().map_state == X.IsViewable:
            return w
    except Exception:
        pass
    for c in w.query_tree().children:
        r = find(c)
        if r:
            return r


win = None
for _ in range(100):
    win = find(root)
    if win:
        break
    time.sleep(0.2)
g = win.get_geometry()
pos = win.translate_coords(root, 0, 0)
ox, oy, W, H = -pos.x, -pos.y, g.width, g.height


def alive():
    try:
        return open(f"/proc/{PID}/stat").read().split(")")[1].split()[0] != "Z"
    except OSError:
        return False


def click(x, y, b=1):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    xtest.fake_input(d, X.ButtonPress, b)
    xtest.fake_input(d, X.ButtonRelease, b)
    d.sync()
    time.sleep(0.4)


def wheel(x, y, up, n):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    for _ in range(n):
        b = 4 if up else 5
        xtest.fake_input(d, X.ButtonPress, b)
        xtest.fake_input(d, X.ButtonRelease, b)
        d.sync()
        time.sleep(0.05)


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
    time.sleep(0.03)


def sample(start):
    pss = anon = rss = 0
    for line in open(f"/proc/{PID}/smaps_rollup"):
        k, v = line.split(":")[0], line.split()[1:2]
        if k == "Pss":
            pss = int(v[0])
        elif k == "Pss_Anon":
            anon = int(v[0])
        elif k == "Rss":
            rss = int(v[0])
    st = open(f"/proc/{PID}/stat").read().split(")")[1].split()
    cpu = (int(st[11]) + int(st[12])) / os.sysconf("SC_CLK_TCK")
    threads = len(os.listdir(f"/proc/{PID}/task"))
    fds = len(os.listdir(f"/proc/{PID}/fd"))
    return ((time.time() - start) / 3600, pss / 1024, anon / 1024, rss / 1024, threads, fds, cpu)


csv = open(f"{t}/samples.csv", "w")
csv.write("часы,pss_мб,анон_мб,rss_мб,потоки,файлы,процессор_с\n")
start = time.time()
end = start + HOURS * 3600
next_sample = start
cycle = 0
samples = []
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()
while time.time() < end and alive():
    if time.time() >= next_sample:
        s = sample(start)
        samples.append(s)
        csv.write(",".join(f"{v:.2f}" if isinstance(v, float) else str(v) for v in s) + "\n")
        csv.flush()
        next_sample += 600
    # Действия по кругу (раз в минуту).
    click(180, 84 + 64 * (cycle % 4))          # один из четырёх верхних чатов
    time.sleep(1.5)
    wheel(W // 2 + 180, H // 2, True, 40)      # вверх по истории (подгрузка страниц)
    time.sleep(1)
    wheel(W // 2 + 180, H // 2, False, 60)     # обратно вниз
    if cycle % 5 == 0:                         # «Сведения»: открыть и закрыть
        click(360 + 120, 26)
        time.sleep(2)
        click(W - 26, 26)
    if cycle % 3 == 0:                         # панель стикеров
        click(W - 68, H - 24)
        time.sleep(2)
        click(W - 68, H - 24)
    if cycle % 10 == 0:                        # набрать и отправить
        click(W // 2 + 180, H - 24)
        for ch in f"soak {cycle}":
            key("space" if ch == " " else ch)
        key("Return")
    cycle += 1
    time.sleep(max(0.0, 60 - 8))
csv.close()

ok = alive()
lines = [f"длительность: {(time.time() - start) / 3600:.1f} ч, циклов действий: {cycle}, процесс жив: {ok}"]
warm = [s for s in samples if s[0] >= 1.0]
if len(warm) >= 6:
    head = [s[1] for s in warm if s[0] < 3.0] or [warm[0][1]]
    tail = [s[1] for s in warm if s[0] >= warm[-1][0] - 2.0]
    h, e = statistics.median(head), statistics.median(tail)
    xs, ys = [s[0] for s in warm], [s[1] for s in warm]
    mx, my = statistics.mean(xs), statistics.mean(ys)
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / max(1e-9, sum((x - mx) ** 2 for x in xs))
    growth = (e - h) / h * 100
    lines += [f"PSS: начало (1–3 ч) {h:.1f} МБ, конец (последние 2 ч) {e:.1f} МБ, рост {growth:+.1f} %",
              f"наклон PSS после первого часа: {slope:+.2f} МБ/ч",
              f"потоки: {warm[0][4]} → {warm[-1][4]}, открытые файлы: {warm[0][5]} → {warm[-1][5]}",
              f"процессор за прогон: {samples[-1][6]:.0f} с"]
    verdict = ok and growth < 10 and slope < 0.5 and warm[-1][5] - warm[0][5] < 20
    lines.append("ИТОГ: " + ("роста памяти нет" if verdict else "ПОДОЗРЕНИЕ НА РОСТ"))
else:
    lines.append("ИТОГ: мало замеров для вывода")
open(f"{t}/report.txt", "w").write("\n".join(lines) + "\n")
print("\n".join(lines))
