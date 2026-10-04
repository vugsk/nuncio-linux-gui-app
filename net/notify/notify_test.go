package notify

import "testing"

func TestEscape(t *testing.T) {
	if got := escape("a<b> & ё"); got != "a&lt;b&gt; &amp; ё" {
		t.Fatal(got)
	}
}
