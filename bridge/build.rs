// Шаг 5 из Архитектура/nuncio - Сборка и структура репозитория.md:
// генерирует Rust-типы client.proto через prost. Схема для Go-стороны
// (net/gen) генерируется отдельно, шагом `just proto` (protoc-gen-go).
fn main() {
    println!("cargo:rerun-if-changed=../proto/client.proto");
    prost_build::compile_protos(&["../proto/client.proto"], &["../proto"])
        .expect("не удалось скомпилировать proto/client.proto (нужен protoc в PATH)");
}
