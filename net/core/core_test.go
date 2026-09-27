package core

import (
	"strings"
	"testing"
)

func TestVersion(t *testing.T) {
	v := Version()
	if !strings.HasPrefix(v, "tgcrypto ") || len(v) <= len("tgcrypto ") {
		t.Fatalf("неожиданная версия ядра: %q", v)
	}
}
