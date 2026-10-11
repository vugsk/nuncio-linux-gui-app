#!/usr/bin/env bash
# Свой репозиторий pacman [nuncio] (выбрано пользователем 2026-10-11 вместо AUR,
# где закрыта регистрация): база и пакеты — в выпуске GitHub с тегом `repo`,
# сборка и подпись — локально, закрытый ключ с машины не уходит.
#
#   just repo                         собрать (makepkg), подписать, выложить
#   packaging/repo/publish.sh --pkg FILE      выложить уже собранный пакет
#   packaging/repo/publish.sh --to DIR        вместо GitHub — в каталог (проверки)
#
# Ключ подписи — отпечаток в NUNCIO_REPO_KEY или в packaging/repo/key.fingerprint.
# В репозитории держится одна, последняя версия: старый пакет из выпуска удаляется.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
REPO=nuncio
GH=vugsk/nuncio-linux-gui-app
TAG=repo
die() { echo "publish: $*" >&2; exit 1; }

TO= PKG=
while [ $# -gt 0 ]; do
  case $1 in
    --to) TO=$2; shift 2 ;;
    --pkg) PKG=$(realpath "$2"); shift 2 ;;
    *) die "неизвестный параметр $1" ;;
  esac
done

KEY=${NUNCIO_REPO_KEY:-$(cat "$ROOT/packaging/repo/key.fingerprint" 2>/dev/null || true)}
[ -n "$KEY" ] || die "нет ключа: отпечаток в NUNCIO_REPO_KEY или packaging/repo/key.fingerprint"
gpg --list-secret-keys "$KEY" >/dev/null 2>&1 || die "нет закрытого ключа $KEY в связке GPG"

if [ -z "$PKG" ]; then
  [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ] ||
    echo "publish: внимание — есть незакоммиченные правки; PKGBUILD собирает последний коммит" >&2
  (cd "$ROOT/packaging/arch" && makepkg -f)
  PKG=$(ls -t "$ROOT"/packaging/arch/$REPO-*.pkg.tar.zst | head -1)
fi
[ -f "$PKG" ] || die "нет пакета $PKG"
NEW=$(basename "$PKG")

mkdir -p "$ROOT/target"
W=$(mktemp -d "$ROOT/target/repo.XXXXXX")
trap 'rm -rf "$W"' EXIT
cp "$PKG" "$W/"
gpg --batch --yes -u "$KEY" --detach-sign --no-armor "$W/$NEW"

# Текущая база: дополняется, а не создаётся заново.
DBF=("$REPO.db.tar.zst" "$REPO.db.tar.zst.sig" "$REPO.files.tar.zst" "$REPO.files.tar.zst.sig")
if [ -n "$TO" ]; then
  for f in "${DBF[@]}"; do [ -f "$TO/$f" ] && cp "$TO/$f" "$W/"; done
elif gh release view "$TAG" -R "$GH" >/dev/null 2>&1; then
  gh release download "$TAG" -R "$GH" -D "$W" $(printf -- '-p %s ' "${DBF[@]}") 2>/dev/null || true
fi
OLD=
VERIFY=()
if [ -f "$W/$REPO.db.tar.zst" ]; then
  OLD=$(tar -xOf "$W/$REPO.db.tar.zst" --wildcards "$REPO-*/desc" | awk '/^%FILENAME%$/ { getline; print }')
  VERIFY=(--verify)
fi
repo-add -q --sign --key "$KEY" "${VERIFY[@]}" "$W/$REPO.db.tar.zst" "$W/$NEW"
# pacman запрашивает nuncio.db; repo-add делает его ссылкой — в выпуск нужны копии.
for f in db files; do
  cp --remove-destination "$W/$REPO.$f.tar.zst" "$W/$REPO.$f"
  cp --remove-destination "$W/$REPO.$f.tar.zst.sig" "$W/$REPO.$f.sig"
done
gpg --armor --export "$KEY" > "$W/$REPO-repo.asc"

PKGS=("$W/$NEW" "$W/$NEW.sig")
META=("$W/$REPO.db" "$W/$REPO.db.sig" "$W/$REPO.files" "$W/$REPO.files.sig" "$W/$REPO-repo.asc")
for f in "${DBF[@]}"; do META+=("$W/$f"); done
if [ -n "$TO" ]; then
  mkdir -p "$TO"
  cp "${PKGS[@]}" "$TO/"
  cp "${META[@]}" "$TO/"
  [ -n "$OLD" ] && [ "$OLD" != "$NEW" ] && rm -f "$TO/$OLD" "$TO/$OLD.sig"
else
  gh release view "$TAG" -R "$GH" >/dev/null 2>&1 ||
    gh release create "$TAG" -R "$GH" --latest=false --title "Репозиторий pacman" \
      --notes "Служебный выпуск: база и пакеты репозитория pacman [nuncio]. Подключение — в README."
  # Сначала пакет, потом база: база не должна ссылаться на ещё не выложенный файл.
  gh release upload "$TAG" -R "$GH" --clobber "${PKGS[@]}"
  gh release upload "$TAG" -R "$GH" --clobber "${META[@]}"
  if [ -n "$OLD" ] && [ "$OLD" != "$NEW" ]; then
    gh release delete-asset "$TAG" "$OLD" -R "$GH" -y || true
    gh release delete-asset "$TAG" "$OLD.sig" -R "$GH" -y || true
  fi
fi
echo "publish: выложен $NEW${OLD:+ (был $OLD)}"
