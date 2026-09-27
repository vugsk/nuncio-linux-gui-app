# Шаги сборки — см. Архитектура/nuncio - Сборка и структура репозитория.md.
# Каркас (веха В0): все шаги реальны (ничего не заглушено в justfile), но
# то, что они собирают, — по большей части ещё пустые заготовки без
# логики (см. комментарии в исходниках каждого шага).

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
    cd net && go vet ./... && go test ./...
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

# Шаг 4: сеть Go (пока без -buildmode=c-archive: вертикаль-заглушка ещё
# не написана, см. Архитектура/nuncio - План реализации.md, веха В0/В3).
net:
    cd net && go vet ./... && go build ./...

# Проверки качества (раздел 4 заметки о сборке).
lint:
    cargo clippy --all-targets -- -D warnings
    cargo deny check
    cd net && go vet ./... && staticcheck ./... || true
    cd core && clang-tidy src/*.cpp -- -std=c++20 -Iinclude || true

clean:
    rm -rf target core/build net/tl net/gen
