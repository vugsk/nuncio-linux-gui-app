package portal

import (
	"reflect"
	"testing"
)

func TestPaths(t *testing.T) {
	got := paths([]string{"file:///home/u/%D0%A4%D0%BE%D1%82%D0%BE%201.jpg", "https://x/y", "file:///tmp/a.pdf"})
	want := []string{"/home/u/Фото 1.jpg", "/tmp/a.pdf"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("%q", got)
	}
}
