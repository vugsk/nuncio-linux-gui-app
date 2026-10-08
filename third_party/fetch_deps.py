#!/usr/bin/env python3
"""Шаг 0 сборки (`just deps`): скачивает и собирает OpenSSL и FFmpeg по
рецепту из deps.toml (версия, URL, SHA-256, флаги configure — единственный
источник правды, см. комментарии в этом файле). Каркас (веха В0): скрипт
рабочий и уже проверялся вручную при исследовании Р-11 и П-6, но здесь
запускается на новом репозитории впервые — если что-то в окружении
отличается, сообщение об ошибке должно быть понятным, а не голым traceback.

Использование:
    python3 third_party/fetch_deps.py [openssl] [ffmpeg] [noto-emoji]
(без аргументов — все).
"""
from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CACHE = ROOT / ".deps-cache"
NATIVE = ROOT / "target" / "native"


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url: str, dest: Path, expected_sha256: str) -> None:
    if dest.exists() and sha256_of(dest) == expected_sha256:
        print(f"  уже скачано и совпадает по SHA-256: {dest.name}")
        return
    print(f"  скачиваю {url}")
    dest.parent.mkdir(parents=True, exist_ok=True)
    tmp = dest.with_suffix(dest.suffix + ".part")
    urllib.request.urlretrieve(url, tmp)
    got = sha256_of(tmp)
    if got != expected_sha256:
        tmp.unlink(missing_ok=True)
        raise SystemExit(
            f"SHA-256 не совпал для {url}\n  ожидалось {expected_sha256}\n  получено {got}\n"
            "Обновите third_party/deps.toml, если версия сменилась намеренно."
        )
    tmp.rename(dest)


def extract(archive: Path, into: Path) -> Path:
    if into.exists():
        shutil.rmtree(into)
    into.mkdir(parents=True)
    with tarfile.open(archive) as tar:
        tar.extractall(into, filter="data")
    entries = list(into.iterdir())
    if len(entries) != 1:
        raise SystemExit(f"неожиданное содержимое архива {archive}: {entries}")
    return entries[0]


def run(cmd: list[str], cwd: Path) -> None:
    print("  $", " ".join(cmd))
    subprocess.run(cmd, cwd=cwd, check=True)


def build_openssl(dep: dict) -> None:
    print("OpenSSL", dep["version"])
    archive = CACHE / f"openssl-{dep['version']}.tar.gz"
    download(dep["url"], archive, dep["sha256"])
    src = extract(archive, CACHE / "build-openssl")
    run(["./Configure", *dep["configure_args"]], cwd=src)
    run(["make", f"-j{os.cpu_count() or 1}", *dep["build_target"].split()], cwd=src)
    NATIVE.mkdir(parents=True, exist_ok=True)
    for artifact in dep["artifacts"]:
        shutil.copy2(src / artifact, NATIVE / Path(artifact).name)
        print(f"  -> target/native/{Path(artifact).name}")
    # Заголовки (включая сгенерированные configuration.h и opensslv.h) нужны ядру.
    headers = NATIVE / "include" / "openssl"
    if headers.exists():
        shutil.rmtree(headers)
    shutil.copytree(src / "include" / "openssl", headers)
    print("  -> target/native/include/openssl/")
    shutil.copy2(src / "LICENSE.txt", NATIVE / "OpenSSL.LICENSE.txt")


def verify_signature(archive: Path, dep: dict) -> None:
    """Подпись .asc архива ключом выпусков FFmpeg; отпечаток ключа закреплён в
    deps.toml. Временное хранилище ключей — личное gpg не трогается."""
    sig = archive.with_name(archive.name + ".asc")
    key = CACHE / "ffmpeg-release-key.asc"
    # Подпись и ключ уже положены (сборка пакета скачивает их сама) — без сети;
    # отпечаток ключа всё равно сверяется ниже.
    if not (sig.exists() and key.exists()):
        print(f"  скачиваю подпись {dep['url']}.asc и ключ {dep['signing_key_url']}")
        urllib.request.urlretrieve(dep["url"] + ".asc", sig)
        urllib.request.urlretrieve(dep["signing_key_url"], key)
    home = CACHE / "gnupg"
    shutil.rmtree(home, ignore_errors=True)
    home.mkdir(mode=0o700)
    subprocess.run(["gpg", "--homedir", str(home), "--quiet", "--import", str(key)], check=True)
    out = subprocess.run(["gpg", "--homedir", str(home), "--status-fd", "1", "--verify", str(sig), str(archive)],
                         capture_output=True, text=True)
    shutil.rmtree(home, ignore_errors=True)
    valid = [line.split()[2] for line in out.stdout.splitlines() if line.startswith("[GNUPG:] VALIDSIG")]
    want = dep["signing_key_fingerprint"].replace(" ", "").upper()
    # VALIDSIG: отпечаток подключа подписи и (последним полем) основного ключа.
    prim = [line.split()[-1] for line in out.stdout.splitlines() if line.startswith("[GNUPG:] VALIDSIG")]
    if not valid or want not in (valid + prim):
        raise SystemExit(f"подпись FFmpeg не прошла проверку (ожидался ключ {want}):\n{out.stdout}{out.stderr}")
    print(f"  подпись верна, ключ {want}")


def build_ffmpeg(dep: dict) -> None:
    print("FFmpeg", dep["version"])
    archive = CACHE / f"ffmpeg-{dep['version']}.tar.xz"
    download(dep["url"], archive, dep["sha256"])
    verify_signature(archive, dep)
    src = extract(archive, CACHE / "build-ffmpeg")
    prefix = NATIVE / "ffmpeg"
    run(["./configure", f"--prefix={prefix}", *dep["configure_args"]], cwd=src)
    run(["make", f"-j{os.cpu_count() or 1}"], cwd=src)
    # lib/ и include/ — так их ищет ffmpeg-sys-next (FFMPEG_DIR, .cargo/config.toml).
    shutil.rmtree(prefix, ignore_errors=True)
    run(["make", "install-libs", "install-headers"], cwd=src)
    for stale in dep["artifacts"]:
        (NATIVE / Path(stale).name).unlink(missing_ok=True)
    # Текст лицензии рядом с библиотеками — для выпуска (third_party/NOTICES.md).
    shutil.copy2(src / "COPYING.LGPLv2.1", prefix / "COPYING.LGPLv2.1")
    print("  -> target/native/ffmpeg/{lib,include}")


def fetch_noto_emoji(dep: dict) -> None:
    print("Noto Color Emoji", dep["version"])
    NATIVE.mkdir(parents=True, exist_ok=True)
    download(dep["url"], CACHE / "NotoColorEmoji.ttf", dep["sha256"])
    download(dep["license_url"], CACHE / "NotoColorEmoji.LICENSE", dep["license_sha256"])
    for name in dep["artifacts"]:
        shutil.copy2(CACHE / name, NATIVE / name)


def build_thorvg(dep: dict) -> None:
    print("ThorVG", dep["version"])
    archive = CACHE / f"thorvg-{dep['version']}.tar.gz"
    if not dep["sha256"]:
        # Первая загрузка: скачать, распаковать для просмотра, показать сумму.
        if not archive.exists():
            print(f"  скачиваю {dep['url']}")
            urllib.request.urlretrieve(dep["url"], archive)
        src = extract(archive, CACHE / "build-thorvg")
        raise SystemExit(f"  SHA-256 {sha256_of(archive)}\n  исходники: {src}\n"
                         "  Сумма ещё не закреплена в deps.toml — сборка не начата.")
    download(dep["url"], archive, dep["sha256"])
    src = extract(archive, CACHE / "build-thorvg")
    build = src / "builddir"
    shutil.rmtree(build, ignore_errors=True)
    run(["meson", "setup", str(build), *dep["meson_args"]], cwd=src)
    run(["ninja", "-C", str(build)], cwd=src)
    prefix = NATIVE / "thorvg"
    shutil.rmtree(prefix, ignore_errors=True)
    (prefix / "lib").mkdir(parents=True)
    (prefix / "include").mkdir(parents=True)
    for artifact in dep["artifacts"]:
        shutil.copy2(build / artifact, prefix / "lib" / Path(artifact).name)
    for header in dep["headers"]:
        shutil.copy2(src / header, prefix / "include" / Path(header).name)
    shutil.copy2(src / "LICENSE", prefix / "LICENSE")
    print("  -> target/native/thorvg/{lib,include}")


def main() -> None:
    deps = tomllib.loads((ROOT / "third_party" / "deps.toml").read_text())
    wanted = sys.argv[1:] or ["openssl", "ffmpeg", "noto-emoji", "thorvg"]
    CACHE.mkdir(exist_ok=True)
    if "openssl" in wanted:
        build_openssl(deps["openssl"])
    if "ffmpeg" in wanted:
        build_ffmpeg(deps["ffmpeg"])
    if "noto-emoji" in wanted:
        fetch_noto_emoji(deps["noto-emoji"])
    if "thorvg" in wanted:
        build_thorvg(deps["thorvg"])
    print("Готово:", NATIVE)


if __name__ == "__main__":
    main()
