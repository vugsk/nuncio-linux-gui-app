package keyring

import (
	"bytes"
	"context"
	"errors"
	"os"
	"testing"
	"time"
)

// Против настоящей связки ключей пользователя: создаёт отдельную запись
// «nuncio-test» и удаляет её. Нет Secret Service — тест пропускается.
func TestRealSecretService(t *testing.T) {
	if os.Getenv("DBUS_SESSION_BUS_ADDRESS") == "" {
		t.Skip("нет сессионной шины")
	}
	k, err := Open("nuncio-test")
	if errors.Is(err, ErrUnavailable) {
		t.Skip("Secret Service недоступен:", err)
	}
	if err != nil {
		t.Fatal(err)
	}
	defer k.Close()
	// Коллекция «session» живёт в памяти службы и не заблокирована: тест не
	// спрашивает пароль и не трогает основную связку пользователя.
	k.Collection = "/org/freedesktop/secrets/collection/session"
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	_ = k.Delete(ctx)

	if _, found, err := k.WrapKey(ctx, false); err != nil || found {
		t.Fatalf("после удаления ключ найден: %v %v", found, err)
	}
	k1, found, err := k.WrapKey(ctx, true)
	if err != nil || !found || len(k1) != 32 {
		t.Fatalf("создание: %v %v %d", found, err, len(k1))
	}
	k2, found, err := k.WrapKey(ctx, true)
	if err != nil || !found || !bytes.Equal(k1, k2) {
		t.Fatal("повторное чтение вернуло другой ключ")
	}
	if err := k.Delete(ctx); err != nil {
		t.Fatal(err)
	}
	if _, found, _ := k.WrapKey(ctx, false); found {
		t.Fatal("ключ остался после Delete")
	}
}

func TestNoBus(t *testing.T) {
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/nuncio-test-bus")
	if _, err := Open("nuncio-test"); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("без шины ожидалась ErrUnavailable: %v", err)
	}
}
