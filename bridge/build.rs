// 1. Генерирует Rust-типы client.proto через prost (Go-сторона — `just proto`).
// 2. Линкует готовые архивы из target/native: libnuncio_net.a (Go, c-archive)
//    и libtgcrypto.a (ядро C++). `cargo build` сам их не собирает (О-11):
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
    for lib in ["libnuncio_net.a", "libtgcrypto.a"] {
        let path = native.join(lib);
        if !path.exists() {
            panic!("нет {}: выполните `just build` в корне репозитория", path.display());
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-link-search=native={}", native.display());
    println!("cargo:rustc-link-lib=static=nuncio_net");
    println!("cargo:rustc-link-lib=static=tgcrypto");
    for lib in ["stdc++", "pthread", "dl", "m", "resolv"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
