"""Проверки окна и значка в трее для tests/traytest.sh (один прогон).

    tray.py КАТАЛОГ PID_ПРИЛОЖЕНИЯ PID_FAKEDESK true|false
"""
import os, re, signal, sys, time
from Xlib import X, display, protocol

t, PID, DESK, HOST = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4] == "true"
d = display.Display()
root = d.screen().root
PID_ATOM = d.intern_atom("_NET_WM_PID")
WM_PROTOCOLS = d.intern_atom("WM_PROTOCOLS")
WM_DELETE = d.intern_atom("WM_DELETE_WINDOW")


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


def wait(cond, secs=10):
    for _ in range(int(secs / 0.25)):
        if cond():
            return True
        time.sleep(0.25)
    return False


win = None
wait(lambda: find(root) is not None)
win = find(root)


def mapped():
    try:
        return win.get_attributes().map_state == X.IsViewable
    except Exception:
        return False


def alive():
    try:
        os.kill(PID, 0)
        # Зомби — уже завершился.
        return open(f"/proc/{PID}/stat").read().split(")")[1].split()[0] != "Z"
    except (OSError, IndexError):
        return False


def close_window():
    ev = protocol.event.ClientMessage(window=win, client_type=WM_PROTOCOLS, data=(32, [WM_DELETE, X.CurrentTime, 0, 0, 0]))
    win.send_event(ev)
    d.sync()


def desk():
    return open(f"{t}/desk.log", encoding="utf-8").read()


def pss():
    try:
        for line in open(f"/proc/{PID}/smaps_rollup"):
            if line.startswith("Pss:"):
                return int(line.split()[1]) // 1024
    except OSError:
        pass
    return 0


ok = True


def check(cond, what):
    global ok
    print(("  ок: " if cond else "  ОШИБКА: ") + what)
    ok &= bool(cond)


check(wait(lambda: "трей: зарегистрирован true" in desk()), "значок зарегистрирован у наблюдателя")
check(wait(lambda: re.search(r"значок 22:true,32:true,48:true,64:true .*меню=Открыть nuncio\|Выйти", desk())),
      "картинки 22–64 и меню «Открыть nuncio», «Выйти»")
if HOST:
    check(wait(lambda: 'подсказка="Непрочитанных: 2"' in desk(), 15), "счётчик непрочитанных растёт с входящими")
    check(mapped(), "окно видно")
    shown = pss()
    close_window()
    check(wait(lambda: not mapped(), 5), "крестик спрятал окно")
    time.sleep(3)
    check(alive(), "процесс жив после закрытия окна")
    print(f"  память (PSS): окно видно {shown} МБ, спрятано {pss()} МБ")
    os.kill(DESK, signal.SIGUSR2)
    check(wait(lambda: mapped(), 5), "щелчок по значку вернул окно")
    os.kill(DESK, signal.SIGHUP)
    check(wait(lambda: not alive(), 8), "«Выйти» в меню значка завершил процесс")
else:
    close_window()
    check(wait(lambda: not alive(), 8), "без хоста крестик завершил процесс")
sys.exit(0 if ok else 1)
