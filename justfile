# Шаги сборки — см. Архитектура/nuncio - Сборка и структура репозитория.md.
# Порядок важен: core раньше net (cgo линкует libtgcrypto.a), net раньше
# cargo build (bridge линкует libnuncio_net.a).

set shell := ["bash", "-uc"]

# protoc-gen-go живёт в $(go env GOBIN)/$(go env GOPATH)/bin, что не всегда
# в PATH по умолчанию (go install кладёт его туда, не в /usr/bin).
export PATH := env_var('HOME') / "go/bin:" + env_var('PATH')

# Полная сборка: шаги 1–5 (без deps — она тяжёлая и отдельная).
build: tl core proto net
    cargo build

# Все тесты по слоям.
test:
    cd core/build && ctest --output-on-failure
    cd net && go vet ./... && go test -race ./...
    cd tools/tlgen && go vet ./... && go test ./...
    cargo test

# Шаг 0: скачивает и собирает OpenSSL и FFmpeg (third_party/deps.toml).
# Не входит в `build` по умолчанию — тяжёлая и нужна не на каждой сборке.
deps:
    python3 third_party/fetch_deps.py

# Шаг 1: генератор TL-кодека (пока читает пустые схемы, см. tools/tlgen).
tl:
    cd tools/tlgen && go run . -schema ../../schema -out ../../net/tl

# Шаг 2: крипто-ядро (пока пустая заготовка, см. core/).
core:
    cmake -S core -B core/build -DCMAKE_BUILD_TYPE=Release
    cmake --build core/build
    cd core/build && ctest --output-on-failure

# Шаг 3: типы client.proto для Go-стороны (net/gen).
proto:
    mkdir -p net/gen
    protoc -I proto --go_out=net/gen --go_opt=paths=source_relative proto/client.proto

# Шаг 4: сеть Go -> target/native/libnuncio_net.a (+ .h). Нужна libtgcrypto.a
# (шаг 2): cgo пробно линкует C-часть уже при сборке архива.
net:
    cd net && go vet ./... && go build -buildmode=c-archive -o ../target/native/libnuncio_net.a ./ffi

# Проверки качества (раздел 4 заметки о сборке).
lint:
    cargo clippy --all-targets -- -D warnings
    cargo deny check
    cd net && go vet ./... && staticcheck ./...
    cd tools/tlgen && go vet ./... && staticcheck ./...
    cd core && clang-tidy -p build src/*.cpp

clean:
    rm -rf target core/build net/tl net/gen
