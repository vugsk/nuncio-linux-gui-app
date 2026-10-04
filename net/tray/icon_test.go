package tray

import (
	"image"
	"image/color"
	"image/png"
	"os"
	"testing"
)

func TestRender(t *testing.T) {
	// TRAY_PNG=путь — сохранить значки для просмотра.
	if p := os.Getenv("TRAY_PNG"); p != "" {
		const s = 64
		img := image.NewNRGBA(image.Rect(0, 0, s*4+12, s))
		for k, n := range []int32{0, 7, 42, 1000} {
			b := render(s, n)
			for i := 0; i < s*s; i++ {
				img.Set(k*(s+4)+i%s, i/s, color.NRGBA{b[i*4+1], b[i*4+2], b[i*4+3], b[i*4]})
			}
		}
		f, _ := os.Create(p)
		defer f.Close()
		_ = png.Encode(f, img)
	}
	for _, n := range []int32{0, 7, 42, 1000} {
		b := render(32, n)
		if len(b) != 32*32*4 {
			t.Fatal(len(b))
		}
		// Центр — белое облачко, угол — прозрачный.
		c := (20*32 + 12) * 4
		if b[c] != 255 || b[c+1] != 255 {
			t.Fatalf("%d: центр %v", n, b[c:c+4])
		}
		if b[(31*32)*4] != 0 {
			t.Fatalf("%d: угол не прозрачный", n)
		}
		// Счётчик — красный в правом верхнем углу.
		r := (9*32 + 30) * 4
		if red := b[r+1] > 200 && b[r+2] < 150; red != (n > 0) {
			t.Fatalf("%d: угол счётчика %v", n, b[r:r+4])
		}
	}
}
