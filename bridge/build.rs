// 1. Генерирует Rust-типы client.proto через prost (Go-сторона — `just proto`).
// 2. Линкует готовые архивы из target/native: libnuncio_net.a (Go, c-archive),
//    libtgcrypto.a (ядро C++) и libcrypto.a (урезанный OpenSSL). `cargo build` сам их не собирает (О-11):
//    если их нет — понятное сообщение, а не ошибка линковщика.
//    Порядок важен: nuncio_net раньше tgcrypto (Go-архив ссылается на ядро).
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=../proto/client.proto");
    prost_build::compile_protos(&["../proto/client.proto"], &["../proto"])
        .expect("не удалось скомпилировать proto/client.proto (нужен protoc в PATH)");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let native = manifest.join("../target/native").canonicalize().unwrap_or_else(|_| {
        panic!("нет каталога target/native: выполните `just build` в корне репозитория")
    });
    for lib in ["libnuncio_net.a", "libtgcrypto.a", "libcrypto.a"] {
        let path = native.join(lib);
        if !path.exists() {
            panic!("нет {}: выполните `just build` в корне репозитория", path.display());
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-link-search=native={}", native.display());
    println!("cargo:rustc-link-lib=static=nuncio_net");
    println!("cargo:rustc-link-lib=static=tgcrypto");
    println!("cargo:rustc-link-lib=static=crypto"); // урезанный OpenSSL (Р-11), после ядра

    // Рантайм C++ статически (Архитектура/nuncio - Крипто-ядро (C++).md, раздел 7):
    // бинарник не зависит от версии libstdc++ в системе.
    let libstdcxx = std::process::Command::new("g++")
        .arg("-print-file-name=libstdc++.a")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_absolute() && p.exists());
    match libstdcxx {
        Some(path) => {
            println!("cargo:rustc-link-search=native={}", path.parent().unwrap().display());
            println!("cargo:rustc-link-lib=static=stdc++");
        }
        None => {
            println!("cargo:warning=libstdc++.a не найдена — рантайм C++ линкуется динамически");
            println!("cargo:rustc-link-lib=dylib=stdc++");
        }
    }
    for lib in ["pthread", "dl", "m", "resolv"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
