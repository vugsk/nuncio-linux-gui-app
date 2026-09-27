package transport

import (
	"errors"
	"testing"
)

func TestParseSecret(t *testing.T) {
	hex16 := "00112233445566778899aabbccddeeff"
	if b, dd, err := ParseSecret(hex16); err != nil || dd || len(b) != 16 {
		t.Fatalf("обычный: %v %v %d", err, dd, len(b))
	}
	if b, dd, err := ParseSecret("dd" + hex16); err != nil || !dd || len(b) != 17 {
		t.Fatalf("dd: %v %v %d", err, dd, len(b))
	}
	if b, _, err := ParseSecret("ABEiM0RVZneImaq7zN3u_w"); err != nil || len(b) != 16 {
		t.Fatalf("base64url: %v %d", err, len(b))
	}
	if _, _, err := ParseSecret("ee" + hex16 + "676f6f676c652e636f6d"); !errors.Is(err, ErrFakeTLS) {
		t.Fatalf("ee: %v", err)
	}
	for _, bad := range []string{"", "zz", "0011", "ff" + hex16} {
		if _, _, err := ParseSecret(bad); !errors.Is(err, ErrBadSecret) {
			t.Fatalf("%q: %v", bad, err)
		}
	}
}
