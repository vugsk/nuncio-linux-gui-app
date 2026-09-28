package api

import (
	"encoding/hex"
	"fmt"
	"os"
	"strings"

	"nuncio/net/core"
)

// Переменные только для сквозных проверок с поддельным сервером (net/cmd/fakeserver):
// NUNCIO_TEST_DC — адрес вместо всех DC Telegram; NUNCIO_TEST_RSA — файл с
// открытым ключом сервера («hex-модуль e»). Без обеих переменных не действуют.
// Подменить сервер может лишь тот, кто уже управляет окружением процесса.
func testServer() (map[int]string, error) {
	addr, keyFile := os.Getenv("NUNCIO_TEST_DC"), os.Getenv("NUNCIO_TEST_RSA")
	if addr == "" || keyFile == "" {
		return nil, nil
	}
	data, err := os.ReadFile(keyFile)
	if err != nil {
		return nil, err
	}
	var nHex string
	var e uint32
	if _, err := fmt.Sscan(strings.TrimSpace(string(data)), &nHex, &e); err != nil {
		return nil, err
	}
	n, err := hex.DecodeString(nHex)
	if err != nil {
		return nil, err
	}
	if _, err := core.AddRSAKey(n, e); err != nil {
		return nil, err
	}
	return map[int]string{1: addr, 2: addr, 3: addr, 4: addr, 5: addr}, nil
}
