package tray

import "fmt"

// Значок рисуется здесь же (Нс-4: счётчик — картинкой, шрифтов в SNI нет):
// круг цвета акцента с белым «облачком» и красный кружок со счётчиком.
// Сглаживание — 4×4 выборки на пиксель. Временный: своего значка у
// приложения пока нет.

type rgba struct{ r, g, b, a float64 }

var (
	accent = rgba{0x33, 0x90, 0xec, 1}
	white  = rgba{0xff, 0xff, 0xff, 1}
	badge  = rgba{0xe5, 0x39, 0x35, 1}
)

// Цифры и «+» 3×5.
var glyphs = map[rune][5]string{
	'0': {"###", "#.#", "#.#", "#.#", "###"},
	'1': {".#.", "##.", ".#.", ".#.", "###"},
	'2': {"###", "..#", "###", "#..", "###"},
	'3': {"###", "..#", "###", "..#", "###"},
	'4': {"#.#", "#.#", "###", "..#", "..#"},
	'5': {"###", "#..", "###", "..#", "###"},
	'6': {"###", "#..", "###", "#.#", "###"},
	'7': {"###", "..#", ".#.", ".#.", ".#."},
	'8': {"###", "#.#", "###", "#.#", "###"},
	'9': {"###", "#.#", "###", "..#", "###"},
	'+': {"...", ".#.", "###", ".#.", "..."},
}

func label(unread int32) string {
	if unread > 99 {
		return "99+"
	}
	return fmt.Sprint(unread)
}

// render — значок size×size в формате SNI: ARGB32, порядок байтов сетевой.
func render(size int, unread int32) []byte {
	s := float64(size)
	text := ""
	if unread > 0 {
		text = label(unread)
	}
	// Кружок счётчика — в правом верхнем углу; ширина растёт с числом знаков.
	br := s * 0.27
	cell := br * 2 * 0.62 / 5 // высота цифры — 62 % диаметра
	if len(text) > 2 {
		cell *= 0.75 // «99+» уже, чтобы не закрывать облачко
	}
	tw := float64(len(text))*4*cell - cell
	bw := max(0, tw-br*2+cell*2) // удлинение кружка до «таблетки»
	bcx, bcy := s-br-bw, br
	inBadge := func(x, y float64) bool {
		dx := x - min(max(x, bcx), bcx+bw)
		return dx*dx+(y-bcy)*(y-bcy) <= br*br
	}
	inDigit := func(x, y float64) bool {
		x0 := bcx + bw/2 - tw/2
		y0 := bcy - 2.5*cell
		cx, cy := int((x-x0)/cell), int((y-y0)/cell)
		if x < x0 || y < y0 || cy >= 5 {
			return false
		}
		i, col := cx/4, cx%4
		if i >= len(text) || col == 3 {
			return false
		}
		return glyphs[rune(text[i])][cy][col] == '#'
	}
	inBubble := func(x, y float64) bool {
		// Эллипс облачка и хвостик слева снизу.
		ex, ey := (x-s*0.5)/(s*0.27), (y-s*0.47)/(s*0.21)
		if ex*ex+ey*ey <= 1 {
			return true
		}
		// Треугольник (0.27,0.72) (0.33,0.55) (0.45,0.62).
		return inTri(x/s, y/s, 0.27, 0.74, 0.32, 0.55, 0.46, 0.62)
	}
	out := make([]byte, size*size*4)
	const n = 4
	for py := 0; py < size; py++ {
		for px := 0; px < size; px++ {
			var r, g, b, a float64
			for sy := 0; sy < n; sy++ {
				for sx := 0; sx < n; sx++ {
					x := float64(px) + (float64(sx)+0.5)/n
					y := float64(py) + (float64(sy)+0.5)/n
					var c rgba
					switch {
					case text != "" && inBadge(x, y):
						c = badge
						if inDigit(x, y) {
							c = white
						}
					case text != "" && inBadge2(x, y, bcx, bcy, bw, br*1.18):
						// Прозрачный зазор вокруг счётчика.
					case (x-s/2)*(x-s/2)+(y-s/2)*(y-s/2) <= (s/2)*(s/2):
						c = accent
						if inBubble(x, y) {
							c = white
						}
					}
					r += c.r * c.a
					g += c.g * c.a
					b += c.b * c.a
					a += c.a
				}
			}
			i := (py*size + px) * 4
			if a > 0 {
				out[i] = byte(a / (n * n) * 255)
				out[i+1] = byte(r / a)
				out[i+2] = byte(g / a)
				out[i+3] = byte(b / a)
			}
		}
	}
	return out
}

func inBadge2(x, y, cx, cy, w, r float64) bool {
	dx := x - min(max(x, cx), cx+w)
	return dx*dx+(y-cy)*(y-cy) <= r*r
}

func inTri(x, y, ax, ay, bx, by, cx, cy float64) bool {
	d1 := (x-bx)*(ay-by) - (ax-bx)*(y-by)
	d2 := (x-cx)*(by-cy) - (bx-cx)*(y-cy)
	d3 := (x-ax)*(cy-ay) - (cx-ax)*(y-ay)
	neg := d1 < 0 || d2 < 0 || d3 < 0
	pos := d1 > 0 || d2 > 0 || d3 > 0
	return !(neg && pos)
}
