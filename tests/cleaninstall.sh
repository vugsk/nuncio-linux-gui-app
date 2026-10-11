#!/usr/bin/env bash
# Пакет на чистой системе (В10): свежий archlinux:latest в podman (без root),
# установка собранного пакета через pacman -U — все зависимости находятся в
# репозиториях; бинарнику хватает библиотек (ldd); .desktop проходит
# desktop-file-validate; nuncio запускается под Xvfb в контейнере и без ключей
# API показывает экран «не заданы api_id и api_hash», не падая.
#
#   tests/cleaninstall.sh            (сначала: cd packaging/arch && makepkg)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PKG=$(ls -t "$ROOT"/packaging/arch/nuncio-*.pkg.tar.zst 2>/dev/null | head -1)
[ -n "$PKG" ] || { echo "нет собранного пакета: cd packaging/arch && makepkg"; exit 1; }
echo "пакет: $(basename "$PKG")"
podman run --rm -v "$(dirname "$PKG"):/pkg:ro" -e PKG="$(basename "$PKG")" docker.io/library/archlinux:latest bash -euc '
ok=1
check() { if eval "$1"; then echo "  ок: $2"; else echo "  ОШИБКА: $2"; ok=0; fi; }
pacman -Syu --noconfirm >/dev/null 2>&1
pacman -S --noconfirm --needed xorg-server-xvfb desktop-file-utils >/dev/null 2>&1
check "pacman -U --noconfirm /pkg/$PKG > /tmp/install.log 2>&1" "пакет установился, зависимости разрешились"
tail -3 /tmp/install.log
check "! ldd /usr/bin/nuncio | grep -q \"not found\"" "бинарнику хватает библиотек"
check "desktop-file-validate /usr/share/applications/nuncio.desktop" "nuncio.desktop корректен"
check "[ -f /usr/share/icons/hicolor/scalable/apps/nuncio.svg ] && [ -f /usr/share/licenses/nuncio/LICENSE ]" "значок и лицензии на месте"
Xvfb :5 -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
sleep 2
useradd -m tester
su tester -c "DISPLAY=:5 timeout 15 nuncio > /tmp/nuncio.out 2>&1" || true
LOG=/home/tester/.local/state/nuncio/nuncio.log
check "grep -q \"рендерер: software\" $LOG" "запустился, рендерер software"
check "grep -q \"критическое состояние: NoApiCredentials\" $LOG" "без ключей API — экран «не заданы api_id и api_hash»"
check "! grep -q \"panicked\" /tmp/nuncio.out" "без паники"
[ $ok = 1 ] && echo "ИТОГ: пакет на чистой системе в порядке" || { echo "ИТОГ: ОШИБКА"; tail -20 /tmp/nuncio.out; exit 1; }
'
