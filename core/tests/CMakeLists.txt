# Тесты ядра (веха В1): модульные, сверка с эталоном на Python.
# Под ASan/UBSan — та же цель в сборке с TG_SANITIZE=ON (`just core-asan`).

add_executable(unit_tests unit_tests.cpp)
target_include_directories(unit_tests PRIVATE ../src)
target_link_libraries(unit_tests PRIVATE tgcrypto)
add_test(NAME unit_tests COMMAND unit_tests)

add_executable(refcheck refcheck.cpp)
target_include_directories(refcheck PRIVATE ../src)
target_link_libraries(refcheck PRIVATE tgcrypto)

find_package(Python3 COMPONENTS Interpreter REQUIRED)
add_test(NAME reference COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_SOURCE_DIR}/reference.py $<TARGET_FILE:refcheck> 200)

add_executable(bench bench.cpp)
target_include_directories(bench PRIVATE ../src)
target_link_libraries(bench PRIVATE tgcrypto)
