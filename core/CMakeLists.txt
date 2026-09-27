cmake_minimum_required(VERSION 3.20)
project(tgcrypto VERSION 0.1.0 LANGUAGES CXX)

# Веха В0: статическая библиотека с минимумом ABI (tg_version, tg_buf_free)
# для проверки связки Rust -> Go -> ядро. Примитивы
# (AES-IGE, конверт MTProto, RSA_PAD, DH, SRP, обфускация) появляются
# на веху В1, см. Архитектура/nuncio - Крипто-ядро (C++).md и
# nuncio - План реализации.md. С этой вехи же подключается урезанный
# статический OpenSSL из third_party/deps.toml (Р-11).

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

add_compile_options(-Wall -Wextra -Werror -fstack-protector-strong)
add_compile_definitions(_FORTIFY_SOURCE=2)

add_library(tgcrypto STATIC
    src/common.cpp
)
target_include_directories(tgcrypto PUBLIC include)
target_compile_definitions(tgcrypto PRIVATE TGCRYPTO_VERSION="${PROJECT_VERSION}")
set_target_properties(tgcrypto PROPERTIES
    CXX_VISIBILITY_PRESET hidden
    ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_SOURCE_DIR}/../target/native"
)

enable_testing()
add_subdirectory(tests)

