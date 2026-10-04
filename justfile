# Шаги сборки — см. Архитектура/nuncio - Сборка и структура репозитория.md.
# Порядок важен: core раньше net (cgo линкует libtgcrypto.a), net раньше
# cargo build (bridge линкует libnuncio_net.a).

set shell := ["bash", "-uc"]

# protoc-gen-go живёт в $(go env GOBIN)/$(go env GOPATH)/bin, что не всегда
# в PATH по умолчанию (go install кладёт его туда, не в /usr/bin).
export PATH := env_var('HOME') / "go/bin:" + env_var('PATH')

# Полная сборка: шаги 1–5 (без deps — она тяжёлая и отдельная).
build: tl core proto net emoji
    cargo build

# Все тесты по слоям (ядро — ещё и под ASan/UBSan).
test: core-asan
    cd core/build && ctest --output-on-failure
    cd net && go vet ./... && go test -race ./...
    cd tools/tlgen && go vet ./... && go test ./...
    cargo test

# Шаг 0: скачивает и собирает OpenSSL и FFmpeg (third_party/deps.toml).
# Не входит в `build` по умолчанию — тяжёлая и нужна не на каждой сборке.
deps *what:
    python3 third_party/fetch_deps.py {{what}}

# Статический OpenSSL нужен ядру: собирается, только если его ещё нет.
openssl:
    [ -f target/native/libcrypto.a ] && [ -f target/native/include/openssl/evp.h ] || python3 third_party/fetch_deps.py openssl

# Шрифт Noto Color Emoji для флагов и эмодзи (О-14): скачивается, только если его нет.
emoji:
    [ -f target/native/NotoColorEmoji.ttf ] || python3 third_party/fetch_deps.py noto-emoji

# Шаг 1: TL-кодек net/tl из schema/*.tl (замыкание от корней schema/used.txt).
tl:
    cd tools/tlgen && go run . -schema ../../schema -out ../../net/tl

# Шаг 2: крипто-ядро -> target/native/libtgcrypto.a; модульные тесты и сверка с эталоном.
core: openssl
    cmake -S core -B core/build -DCMAKE_BUILD_TYPE=Release
    cmake --build core/build -j
    cd core/build && ctest --output-on-failure

# Ядро под ASan + UBSan (отдельная сборка, в target/native не попадает).
core-asan: openssl
    cmake -S core -B core/build-asan -DCMAKE_BUILD_TYPE=Debug -DTG_SANITIZE=ON
    cmake --build core/build-asan -j
    cd core/build-asan && ctest --output-on-failure

# Фаззинг всех целей параллельно, по `seconds` секунд каждая (корпуса копятся в core/build-fuzz/corpus).
fuzz seconds="600": openssl
    cmake -S core -B core/build-fuzz -DTG_FUZZ=ON -DCMAKE_CXX_COMPILER=clang++ -DCMAKE_BUILD_TYPE=RelWithDebInfo
    cmake --build core/build-fuzz -j
    cd core/build-fuzz && mkdir -p corpus logs && for t in env_parse env_decrypt authgen_decrypt_dh authgen_dh srp key_unseal pq obfs; do mkdir -p corpus/$t; ./fuzz/fuzz_$t corpus/$t -max_total_time={{seconds}} -timeout=10 -rss_limit_mb=2048 -print_final_stats=1 > logs/$t.log 2>&1 & done; wait; ! grep -l "ERROR: \|==ERROR\|deadly signal\|Timeout" logs/*.log

# Скорость AES-IGE (сравнение с П-3).
core-bench: core
    core/build/tests/bench

# Шаг 3: типы client.proto для Go-стороны (net/gen).
proto:
    mkdir -p net/gen
    protoc -I proto --go_out=net/gen --go_opt=paths=source_relative proto/client.proto

# Шаг 4: сеть Go -> target/native/libnuncio_net.a (+ .h). Нужна libtgcrypto.a
# (шаг 2): cgo пробно линкует C-часть уже при сборке архива.
net:
    cd net && go vet ./... && go build -buildmode=c-archive -o ../target/native/libnuncio_net.a ./ffi

# Ручная проверка против настоящего DC Telegram (не входит в test): обмен ключами,
# ping; с ~/.config/nuncio/api.env ещё help.getConfig. Пример: just nettest -dc 4
nettest *args: tl core
    cd net && go run ./cmd/nettest {{args}}

# Сквозные проверки против поддельного Telegram (Xvfb на DISPLAY_NUM, по умолчанию :99):
# kill -9 посреди записи (Р-8) и очередь отправки без сети. Не входят в test: долгие.
e2e:
    cargo build --release
    tests/killtest.sh 5 300 40
    tests/offlinetest.sh 10 15
    tests/scrolltest.sh 3000
    tests/composertest.sh
    tests/listtest.sh
    tests/mediatest.sh
    tests/playtest.sh
    tests/settingstest.sh
    tests/notifytest.sh
    tests/traytest.sh
    tests/logouttest.sh
    tests/critstest.sh

# Проверки качества (раздел 4 заметки о сборке).
lint:
    cargo clippy --all-targets -- -D warnings
    cargo deny check
    tools/go_licenses.sh
    cd net && test -z "$(gofmt -l .)" && go vet ./... && staticcheck ./...
    cd tools/tlgen && test -z "$(gofmt -l .)" && go vet ./... && staticcheck ./...
    cd core && clang-tidy -p build src/*.cpp tests/*.cpp

clean:
    rm -rf target core/build core/build-asan core/build-fuzz net/tl net/gen
