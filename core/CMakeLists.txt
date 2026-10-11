cmake_minimum_required(VERSION 3.20)
project(tgcrypto VERSION 0.2.0 LANGUAGES CXX)

# Крипто-ядро nuncio: только криптография MTProto (О-1, О-2), C ABI в
# include/tgcrypto.h. Архитектура/nuncio - Крипто-ядро (C++).md.
#
# Зависит от урезанного статического OpenSSL из third_party/deps.toml (Р-11):
# target/native/libcrypto.a и target/native/include/openssl (`just deps openssl`).
#
# Опции:
#   TG_SANITIZE=ON — ASan + UBSan для тестов (сборка core/build-asan);
#   TG_FUZZ=ON     — цели libFuzzer (только clang, сборка core/build-fuzz).

option(TG_SANITIZE "ASan + UBSan" OFF)
option(TG_FUZZ "цели libFuzzer (clang)" OFF)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

set(TG_NATIVE "${CMAKE_SOURCE_DIR}/../target/native")
if(NOT EXISTS "${TG_NATIVE}/libcrypto.a" OR NOT EXISTS "${TG_NATIVE}/include/openssl/evp.h")
    message(FATAL_ERROR "нет статического OpenSSL в target/native: выполните `just deps openssl`")
endif()
add_library(openssl_crypto STATIC IMPORTED)
set_target_properties(openssl_crypto PROPERTIES
    IMPORTED_LOCATION "${TG_NATIVE}/libcrypto.a"
    INTERFACE_INCLUDE_DIRECTORIES "${TG_NATIVE}/include")
find_package(Threads REQUIRED)
target_link_libraries(openssl_crypto INTERFACE Threads::Threads ${CMAKE_DL_LIBS})

add_compile_options(-Wall -Wextra -Werror -fstack-protector-strong)
# Свой уровень — только если сборка не задала его сама (makepkg Arch передаёт
# -Wp,-D_FORTIFY_SOURCE=3 в CXXFLAGS: повторное определение при -Werror — ошибка).
if(NOT TG_SANITIZE AND NOT TG_FUZZ AND NOT CMAKE_CXX_FLAGS MATCHES "_FORTIFY_SOURCE")
    add_compile_definitions(_FORTIFY_SOURCE=2)
endif()
if(TG_SANITIZE)
    add_compile_options(-fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer -O1 -g)
    add_link_options(-fsanitize=address,undefined)
endif()

set(TG_SOURCES
    src/common.cpp
    src/aes_ige.cpp
    src/key.cpp
    src/envelope.cpp
    src/rsa_pad.cpp
    src/dh.cpp
    src/authgen.cpp
    src/srp.cpp
    src/obfs.cpp
)

add_library(tgcrypto STATIC ${TG_SOURCES})
target_include_directories(tgcrypto PUBLIC include PRIVATE src)
target_link_libraries(tgcrypto PUBLIC openssl_crypto)
target_compile_definitions(tgcrypto PRIVATE TGCRYPTO_VERSION="${PROJECT_VERSION}")
set_target_properties(tgcrypto PROPERTIES CXX_VISIBILITY_PRESET hidden)
# Только обычная сборка кладёт библиотеку в target/native (её линкуют Go и Rust).
if(NOT TG_SANITIZE AND NOT TG_FUZZ)
    set_target_properties(tgcrypto PROPERTIES ARCHIVE_OUTPUT_DIRECTORY "${TG_NATIVE}")
endif()

if(TG_FUZZ)
    add_subdirectory(fuzz)
else()
    enable_testing()
    add_subdirectory(tests)
endif()
