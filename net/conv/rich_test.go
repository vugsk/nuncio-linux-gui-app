package conv

import (
	"testing"
	"unicode/utf16"

	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

func plain(s string) tl.RichTextClass { return &tl.TextPlain{Text: s} }

// Итоги розыгрыша как богатое сообщение: текст, форматирование со смещениями
// в UTF-16, таблица сеткой.
func TestRichMessage(t *testing.T) {
	cell := func(s string, h bool) tl.PageTableCellClass { return &tl.PageTableCell{Header: h, Text: plain(s)} }
	row := func(h bool, cs ...string) tl.PageTableRowClass {
		var out []tl.PageTableCellClass
		for _, c := range cs {
			out = append(out, cell(c, h))
		}
		return &tl.PageTableRow{Cells: out}
	}
	rm := &tl.RichMessage{Blocks: []tl.PageBlockClass{
		&tl.PageBlockParagraph{Text: &tl.TextConcat{Texts: []tl.RichTextClass{
			plain("🎉 "), &tl.TextBold{Text: plain("Результаты")}, plain(" — "),
			&tl.TextUrl{Text: plain("тут"), Url: "https://t.me/x"}}}},
		&tl.PageBlockList{Items: []tl.PageListItemClass{&tl.PageListItemText{Text: plain("раз")},
			&tl.PageListItemText{Checkbox: true, Checked: true, Text: plain("два")}}},
		&tl.PageBlockTable{Title: plain("Победители"), Rows: []tl.PageTableRowClass{
			row(true, "Место", "Имя"), row(false, "1", "Каратель")}},
	}, Photos: []tl.PhotoClass{}, Documents: []tl.DocumentClass{}}
	text, ents, media := RichMessage(rm)
	want := "🎉 Результаты — тут\n\n• раз\n☑ два\n\nПобедители\nМесто │ Имя\n1     │ Каратель"
	if text != want {
		t.Fatalf("текст:\n%q\nждали\n%q", text, want)
	}
	if media != nil {
		t.Fatal("фото нет")
	}
	u16 := func(s string) int32 { return int32(len(utf16.Encode([]rune(s)))) }
	var bold, url *pb.MessageEntity
	for _, e := range ents {
		if e.Kind == pb.MessageEntity_BOLD && bold == nil {
			bold = e
		}
		if e.Kind == pb.MessageEntity_TEXT_URL {
			url = e
		}
	}
	if bold == nil || bold.Offset != u16("🎉 ") || bold.Length != u16("Результаты") {
		t.Fatalf("жирный: %v", bold)
	}
	if url == nil || url.Url != "https://t.me/x" || url.Offset != u16("🎉 Результаты — ") {
		t.Fatalf("ссылка: %v", url)
	}
}
