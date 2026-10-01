#!/usr/bin/env bash
# Веха В7 под Xvfb против поддельного Telegram (-media: чат «Вложения» с фото,
# альбомом из трёх, файлом 1,5 МБ, опросом и превью ссылки): автоскачивание фото
# в кэш, скачивание файла с ходом и отменой, окно просмотра (отдельное окно,
# Ctrl+S сохраняет, Esc закрывает), голос в опросе; отправка: альбом с подписью,
# документ, отмена выгрузки; очистка кэша. Системный диалог выбора файлов
# подменён списком (NUNCIO_TEST_PICK=@файл), сохранения — каталогом.
# Проверки — по базе, файлам кэша, окнам X и снимкам экрана (синтетические
# данные). Кнопки находятся по цвету на снимке.
#
#   tests/mediatest.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DISPLAY_NUM=${DISPLAY_NUM:-:99}
PORT=${PORT:-44326}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-mediatest.XXXXXX")
mkdir -p "$T/home/config"
printf 'API_ID=1\nAPI_HASH=00000000000000000000000000000000\n' > "$T/home/config/api.env"
chmod 600 "$T/home/config/api.env"

(cd "$ROOT/net" && go build -o "$T/fakeserver" ./cmd/fakeserver)
"$T/fakeserver" -listen "127.0.0.1:$PORT" -key "$T/key" -unread -1 -media -part-delay 1s > "$T/server.log" 2>&1 &
SRV=$!
source "$ROOT/tests/common.sh"
trap 'rc=$?; kill $SRV 2>/dev/null || true; pkill -KILL -f "$T/app" 2>/dev/null || true; tmp_done $rc' EXIT
until grep -q готов "$T/server.log"; do sleep 0.1; done
mkdir -p "$T/saved"
cp "$ROOT/target/release/nuncio" "$T/app"
env -u WAYLAND_DISPLAY DISPLAY="$DISPLAY_NUM" SLINT_BACKEND=winit-software NUNCIO_HOME="$T/home" \
  NUNCIO_TEST_DC="127.0.0.1:$PORT" NUNCIO_TEST_RSA="$T/key" NUNCIO_KEYRING_APP=nuncio-mediatest \
  NUNCIO_KEYRING_COLLECTION=session NUNCIO_WINDOW_SIZE=1300x800 \
  NUNCIO_TEST_PICK="@$T/pick.txt" NUNCIO_TEST_SAVE="$T/saved" \
  "$T/app" > "$T/app.log" 2>&1 &
APP=$!
until grep -q "шаг READY" "$T/home/state/net.log" 2>/dev/null; do sleep 0.2; done
sleep 4

DISPLAY="$DISPLAY_NUM" python3 - "$T" "$APP" <<'PY'
import json, os, sqlite3, subprocess, sys, time
from PIL import Image
from Xlib import X, XK, display
from Xlib.ext import xtest

t, PID = sys.argv[1], int(sys.argv[2])
d = display.Display()
root = d.screen().root
PID_ATOM = d.intern_atom("_NET_WM_PID")

def windows(w=root, out=None):
    """Окна приложения верхнего уровня (с _NET_WM_PID и видимые)."""
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

def click(x, y, b=1):
    xtest.fake_input(d, X.MotionNotify, x=ox + x, y=oy + y)
    xtest.fake_input(d, X.ButtonPress, b)
    xtest.fake_input(d, X.ButtonRelease, b)
    d.sync()
    time.sleep(0.4)

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
    time.sleep(0.4)

def wait(cond, secs=15):
    for _ in range(int(secs / 0.25)):
        if cond():
            return True
        time.sleep(0.25)
    return False

def shot(name):
    path = f"{t}/{name}.png"
    subprocess.run(["import", "-window", "root", "-crop", f"{W}x{H}+{ox}+{oy}", path], check=True)
    return Image.open(path).convert("RGB")

def db(q, *a):
    return sqlite3.connect(f"{t}/home/data/nuncio.db").execute(q, a).fetchall()

ok = True
def check(cond, what):
    global ok
    print(("  ок: " if cond else "  ОШИБКА: ") + what)
    ok &= bool(cond)

def near(p, c, tol=6):
    return all(abs(a - b) <= tol for a, b in zip(p, c))

LINK = (0x6a, 0xb3, 0xf3)
BORDER = (0x3d, 0x4f, 0x61)

def file_button(img, last=False):
    """Круглая кнопка файла 40 px цвета ссылки в области чата: (x, y) центра
    первой сверху (last — нижней)."""
    px = img.load()
    rows = {}
    for y in range(60, H - 60):
        run = 0
        for x in range(380, W):
            if near(px[x, y], LINK):
                run += 1
                continue
            if 16 <= run <= 42:
                rows.setdefault(y, []).append((x - run, x))
            run = 0
    ys = sorted(rows)
    found = []
    y0 = None
    for y in ys:
        if found and y < found[-1][2] + 44:
            continue
        span = [v for v in ys if y <= v < y + 44]
        if len(span) >= 26:
            x0 = min(a for v in span for a, _ in rows[v])
            found.append((x0 + 20, (span[0] + span[-1]) // 2, span[0]))
    if not found:
        return None
    return found[-1] if last else found[0]

def poll_option(img, below, left):
    """Верх первой рамки варианта опроса ниже кнопки файла: рамка 32 px — линии
    одной длины на y и y + 31 (у пузырей такой пары нет)."""
    px = img.load()
    def line(y):
        xs = [x for x in range(left - 20, min(W, left + 400)) if near(px[x, y], BORDER, 4)]
        return (min(xs), len(xs)) if len(xs) >= 150 else None
    for y in range(below, H - 60 - 31):
        a = line(y)
        if a and line(y + 31) == a:
            return a[0], y
    return None

# Чат «Вложения» — первый в списке.
click(180, 52 + 32)
wait(lambda: db("SELECT count(*) FROM media WHERE state = 1 AND file_id LIKE 'p_%'") == [(5,)], 20)
time.sleep(0.5)
img = shot("1-chat")
photos = db("SELECT file_id, cache_path, size FROM media WHERE state = 1 AND file_id LIKE 'p_%'")
check(len(photos) == 5, f"фото окна скачаны в кэш сами (5 из 5: одиночное, альбом из трёх, превью ссылки): {len(photos)}")
check(all(os.path.getsize(p) == s for _, p, s in photos), "размеры файлов кэша совпадают с индексом")
mj = db("SELECT media_json FROM messages WHERE peer_id = 103 AND grouped_id = 77")
check(len(mj) == 3, "альбом: у трёх сообщений один grouped_id")

btn = file_button(img)
check(btn is not None, f"кнопка файла найдена на снимке: {btn}")
if btn is None:
    print("ИТОГ: ОШИБКА"); sys.exit(1)
bx, by, btop = btn

# Скачать → отменить → скачать снова (продолжение с записанного).
click(bx, by)
time.sleep(0.2)
shot("2-downloading")
click(bx, by)
time.sleep(1.2)
check(db("SELECT count(*) FROM media WHERE file_id = 'd_9001_0'") == [(0,)], "второе нажатие отменило скачивание")
click(bx, by)
wait(lambda: db("SELECT count(*) FROM media WHERE file_id = 'd_9001_0' AND state = 1") == [(1,)], 20)
row = db("SELECT cache_path, size FROM media WHERE file_id = 'd_9001_0'")
check(row and os.path.getsize(row[0][0]) == 1536 * 1024 + 123, f"файл скачан целиком после отмены: {row}")
check(row and row[0][0].endswith(".pdf") and not os.path.exists(row[0][0] + ".part"), "в кэше под своим расширением, без .part")
time.sleep(0.6)
img = shot("3-downloaded")

# Голос в опросе: первый вариант.
opt = poll_option(img, btop + 40, bx - 20)
check(opt is not None, f"вариант опроса найден на снимке: {opt}")
if opt:
    click(opt[0] + 60, opt[1] + 16)
    wait(lambda: '"chosen":true' in (db("SELECT media_json FROM messages WHERE peer_id = 103 AND text = 'опрос'") or [("",)])[0][0], 10)
    mj = db("SELECT media_json FROM messages WHERE peer_id = 103 AND text = 'опрос'")
    poll = json.loads(mj[0][0])["poll"] if mj and mj[0][0] else {}
    chosen = [o["text"] for o in poll.get("options", []) if o.get("chosen")]
    check(chosen == ["В пятницу"] and poll.get("total_voters") == 4, f"голос учтён, результаты перечитаны: {chosen}, {poll.get('total_voters')}")
    shot("4-voted")

# Окно просмотра: нижняя правая ячейка альбома прямо над файлом.
click(bx + 180, btop - 130)
time.sleep(1.5)
ws = windows()
check(len(ws) == 2, f"окно просмотра открыто отдельным окном: окон {len(ws)}")
if len(ws) == 2:
    v = [w for w in ws if w.id != win.id][0]
    vg = v.get_geometry()
    vp = v.translate_coords(root, 0, 0)
    subprocess.run(["import", "-window", "root", "-crop", f"{vg.width}x{vg.height}+{-vp.x}+{-vp.y}", f"{t}/5-viewer.png"], check=True)
    v.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    key("Left")
    subprocess.run(["import", "-window", "root", "-crop", f"{vg.width}x{vg.height}+{-vp.x}+{-vp.y}", f"{t}/6-viewer-prev.png"], check=True)
    key("s", ctrl=True)
    check(wait(lambda: any(n.startswith("photo_") for n in os.listdir(f"{t}/saved")), 5),
        f"Ctrl+S сохранил фото через диалог сохранения: {os.listdir(t + '/saved')}")
    key("Escape")
    time.sleep(1)
    check(len(windows()) == 1, "Esc закрыл окно просмотра")

# Отправка (Св-4). Файлы — синтетические.
from PIL import Image as PImage
PImage.new("RGB", (800, 600), (200, 80, 40)).save(f"{t}/one.jpg", quality=85)
PImage.new("RGB", (640, 480), (40, 160, 90)).save(f"{t}/two.png")
with open(f"{t}/report.pdf", "wb") as f:
    f.write(os.urandom(700 * 1024))
with open(f"{t}/big.bin", "wb") as f:
    f.write(os.urandom(3 * 1024 * 1024))

def pick(*names):
    with open(f"{t}/pick.txt", "w") as f:
        f.write("\n".join(f"{t}/{n}" for n in names))

CLIP, SEND = (383, H - 24), (W - 8 - 18, H - 24)
win.set_input_focus(X.RevertToParent, X.CurrentTime)
d.sync()

def sent(where):
    return db(f"SELECT id, text, grouped_id, media_json FROM messages WHERE peer_id = 103 AND out = 1 AND id > 0 AND {where}")

# Альбом из двух картинок с подписью (Enter в поле ввода).
pick("one.jpg", "two.png")
click(*CLIP)
time.sleep(1.5)
shot("7-prepare")
click(W // 2, H - 24)
for ch in "album":
    key(ch)
key("Return")
check(wait(lambda: len(sent("grouped_id != 0 AND media_json LIKE '%\"Photo\"%'")) == 2),
    f"альбом из двух фото отправлен: {[(r[0], r[1], r[2]) for r in sent('grouped_id != 0')]}")
rows = sent("grouped_id != 0")
check(rows and rows[0][1] == "album", "подпись у первого фото альбома")
check(db("SELECT count(*) FROM outbox") == [(0,)], "очередь пуста после отправки")
time.sleep(1)
shot("8-album-sent")

# Документ: «как файл», кнопка «отправить».
pick("report.pdf")
click(*CLIP)
time.sleep(1.5)
shot("9-prepare-file")
click(*SEND)
check(wait(lambda: len(sent("media_json LIKE '%report.pdf%'")) == 1), "документ отправлен")
mj = sent("media_json LIKE '%report.pdf%'")
check(mj and json.loads(mj[0][3]).get("size") == 700 * 1024, "размер документа на сервере совпал")

# Отмена выгрузки: большой файл, нажатие на кнопку своего неотправленного.
pick("big.bin")
click(*CLIP)
time.sleep(1.5)
click(*SEND)
time.sleep(0.8)
img = shot("10-uploading")
up = file_button(img, last=True)
check(up is not None, f"кнопка выгружаемого файла найдена: {up}")
if up:
    click(up[0], up[1])
    check(wait(lambda: db("SELECT state, error FROM outbox") == [(2, "cancelled")], 8),
        f"выгрузка отменена, «не отправлено»: {db('SELECT state, error FROM outbox')}")
    shot("11-cancelled")

# Очистка кэша (временный пункт меню).
before = sum(os.path.getsize(os.path.join(f"{t}/home/cache/media", n)) for n in os.listdir(f"{t}/home/cache/media"))
click(30, 26)
click(100, 56 + 6 + 18)
time.sleep(1.5)
left = os.listdir(f"{t}/home/cache/media")
check(db("SELECT count(*) FROM media") == [(0,)] and not left, f"кэш очищен: было {before // 1024} КБ, осталось {left}")
shot("12-cache-cleared")
print("ИТОГ:", "вложения в порядке" if ok else "ОШИБКА")
sys.exit(0 if ok else 1)
PY
kill -TERM $APP 2>/dev/null || true
echo "результат: $T"
