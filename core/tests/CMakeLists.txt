# Каркас (веха В0): один тривиальный тест, чтобы `ctest` было что
# запускать. Настоящие тесты (векторы IGE/SHA/PBKDF2/SRP, свойства,
# сверка с эталоном) появляются на веху В1 — см.
# Архитектура/nuncio - Крипто-ядро (C++).md, раздел 6.

add_executable(smoke_test smoke_test.cpp)
target_link_libraries(smoke_test PRIVATE tgcrypto)
add_test(NAME smoke_test COMMAND smoke_test)
