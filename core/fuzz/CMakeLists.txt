# Цели libFuzzer (веха В1): всё, что разбирает вход из сети.
# Сборка: cmake -S core -B core/build-fuzz -DTG_FUZZ=ON -DCMAKE_CXX_COMPILER=clang++
# Запуск: `just fuzz <секунд>` (урок П-4: разбор открытого текста — отдельная цель,
# иначе фаззер упирается в проверку msg_key).
if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
    message(FATAL_ERROR "фаззеры собираются только clang (-DCMAKE_CXX_COMPILER=clang++)")
endif()
target_compile_options(tgcrypto PRIVATE -fsanitize=fuzzer-no-link,address,undefined -fno-omit-frame-pointer -g)
foreach(name env_parse env_decrypt authgen_decrypt_dh authgen_dh srp key_unseal pq obfs)
    add_executable(fuzz_${name} fuzz_${name}.cpp)
    target_include_directories(fuzz_${name} PRIVATE ../src)
    target_compile_options(fuzz_${name} PRIVATE -fsanitize=fuzzer,address,undefined -g)
    target_link_options(fuzz_${name} PRIVATE -fsanitize=fuzzer,address,undefined)
    target_link_libraries(fuzz_${name} PRIVATE tgcrypto)
endforeach()
