#!/usr/bin/env bash
# Свой репозиторий pacman: packaging/repo/publish.sh --to выкладывает пакет в
# каталог (вместо выпуска GitHub) с временным ключом подписи; каталог отдаётся
# по HTTP, свежий archlinux:latest в podman подключает репозиторий так, как
# написано в README (pacman-key --add / --lsign-key, [nuncio] в pacman.conf),
# и ставит nuncio через pacman -S с проверкой подписей. Второй выпуск того же
# пакета заменяет старый файл; подменённая база отвергается.
#
#   tests/repotest.sh            (сначала: cd packaging/arch && makepkg)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PKG=$(ls -t "$ROOT"/packaging/arch/nuncio-*.pkg.tar.zst 2>/dev/null | head -1)
[ -n "$PKG" ] || { echo "нет собранного пакета: cd packaging/arch && makepkg"; exit 1; }
PORT=${PORT:-44380}
T=$(mktemp -d "${TMPDIR:-/tmp}/nuncio-repo.XXXXXX")
export GNUPGHOME="$T/gnupg"
mkdir -m 700 "$GNUPGHOME"
SRV=
trap 'kill $SRV 2>/dev/null || true; gpgconf --kill all 2>/dev/null || true; rm -rf "$T"' EXIT
gpg --batch --quiet --passphrase '' --quick-generate-key "nuncio repo test" ed25519 sign 1d
FPR=$(gpg --with-colons --list-secret-keys | awk -F: '/^fpr/ { print $10; exit }')

ok=1
check() { if eval "$1"; then echo "  ок: $2"; else echo "  ОШИБКА: $2"; ok=0; fi; }
NUNCIO_REPO_KEY=$FPR "$ROOT/packaging/repo/publish.sh" --pkg "$PKG" --to "$T/www" > /dev/null
# Повторная выкладка того же пакета под другим именем файла — старый должен уйти.
FAKE="$T/$(basename "$PKG" | sed 's/-1-x86_64/-2-x86_64/')"
cp "$PKG" "$FAKE"
NUNCIO_REPO_KEY=$FPR "$ROOT/packaging/repo/publish.sh" --pkg "$FAKE" --to "$T/www" > /dev/null || true
check "[ ! -e '$T/www/$(basename "$PKG")' ] && [ -f '$T/www/$(basename "$FAKE").sig' ]" "новый выпуск заменил старый файл пакета"
# Пакет -2 внутри — всё ещё -1 (.PKGINFO), pacman его отверг бы; выкладываем настоящий снова.
NUNCIO_REPO_KEY=$FPR "$ROOT/packaging/repo/publish.sh" --pkg "$PKG" --to "$T/www" > /dev/null
check "[ -f '$T/www/nuncio.db' ] && [ ! -L '$T/www/nuncio.db' ] && gpg --quiet --verify '$T/www/nuncio.db.sig' '$T/www/nuncio.db' 2>/dev/null" "база — файл, подпись верна"
mkdir "$T/bad"
cp "$T/www/"* "$T/bad/"
cp "$T/www/nuncio.files" "$T/bad/nuncio.db"   # база подменена, подпись старая

python3 -m http.server --bind 127.0.0.1 "$PORT" -d "$T" > "$T/http.log" 2>&1 &
SRV=$!
sleep 1
podman run --rm --network host -e PORT="$PORT" -e FPR="$FPR" docker.io/library/archlinux:latest bash -euc '
ok=1
check() { if eval "$1"; then echo "  ок: $2"; else echo "  ОШИБКА: $2"; ok=0; fi; }
pacman-key --init >/dev/null 2>&1
pacman-key --populate archlinux >/dev/null 2>&1
pacman -Syu --noconfirm >/dev/null 2>&1
curl -sfLo /tmp/nuncio-repo.asc "http://127.0.0.1:$PORT/www/nuncio-repo.asc"
pacman-key --add /tmp/nuncio-repo.asc >/dev/null 2>&1
pacman-key --lsign-key "$FPR" >/dev/null 2>&1
cp /etc/pacman.conf /tmp/pacman.conf.orig
printf "\n[nuncio]\nServer = http://127.0.0.1:%s/bad\n" "$PORT" >> /etc/pacman.conf
check "! pacman -Sy >/tmp/bad.log 2>&1" "подменённая база отвергнута"
cp /tmp/pacman.conf.orig /etc/pacman.conf
printf "\n[nuncio]\nServer = http://127.0.0.1:%s/www\n" "$PORT" >> /etc/pacman.conf
check "pacman -Sy --noconfirm nuncio >/tmp/install.log 2>&1" "pacman -S nuncio из своего репозитория (подписи проверены)"
tail -2 /tmp/install.log
check "pacman -Qi nuncio | grep -q \"^Repository *: nuncio\" || pacman -Sl nuncio | grep -q \"installed\"" "пакет числится за репозиторием [nuncio]"
check "[ -x /usr/bin/nuncio ]" "/usr/bin/nuncio на месте"
[ $ok = 1 ]
' || ok=0
[ $ok = 1 ] && echo "ИТОГ: свой репозиторий pacman в порядке" || { echo "ИТОГ: ОШИБКА"; exit 1; }
