package conv

import (
	"fmt"
	"reflect"
	"strings"
	"unicode/utf16"
	"unicode/utf8"

	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// «Богатое сообщение» (message.rich_message): блоки статьи (PageBlock), как
// в Instant View, — итоги розыгрышей, таблицы, списки. Первое приближение:
// текст с форматированием, который умеет рисовать лента; таблица —
// выровненной сеткой моноширинным, если влезает, иначе строками через «│».

type richBuilder struct {
	b    strings.Builder
	off  int32 // длина в единицах UTF-16
	ents []*pb.MessageEntity
}

func (r *richBuilder) write(s string) {
	r.b.WriteString(s)
	for _, c := range s {
		r.off += int32(utf16.RuneLen(c))
	}
}

// styled — записать содержимое f и отметить его сущностью.
func (r *richBuilder) styled(k pb.MessageEntity_Kind, url string, f func()) {
	start := r.off
	f()
	if n := r.off - start; n > 0 {
		r.ents = append(r.ents, &pb.MessageEntity{Kind: k, Offset: start, Length: n, Url: url})
	}
}

// newline — перевод строки, если строка не пустая (без пустых строк подряд).
func (r *richBuilder) newline() {
	if s := r.b.String(); s != "" && !strings.HasSuffix(s, "\n") {
		r.write("\n")
	}
}

// paragraph — пустая строка между блоками.
func (r *richBuilder) paragraph() {
	r.newline()
	if s := r.b.String(); s != "" && !strings.HasSuffix(s, "\n\n") {
		r.write("\n")
	}
}

func (r *richBuilder) text(t tl.RichTextClass) {
	switch v := t.(type) {
	case nil, *tl.TextEmpty, *tl.TextImage:
	case *tl.TextPlain:
		r.write(v.Text)
	case *tl.TextConcat:
		for _, x := range v.Texts {
			r.text(x)
		}
	case *tl.TextBold:
		r.styled(pb.MessageEntity_BOLD, "", func() { r.text(v.Text) })
	case *tl.TextItalic:
		r.styled(pb.MessageEntity_ITALIC, "", func() { r.text(v.Text) })
	case *tl.TextUnderline:
		r.styled(pb.MessageEntity_UNDERLINE, "", func() { r.text(v.Text) })
	case *tl.TextStrike:
		r.styled(pb.MessageEntity_STRIKE, "", func() { r.text(v.Text) })
	case *tl.TextFixed:
		r.styled(pb.MessageEntity_CODE, "", func() { r.text(v.Text) })
	case *tl.TextSpoiler:
		r.styled(pb.MessageEntity_SPOILER, "", func() { r.text(v.Text) })
	case *tl.TextUrl:
		r.styled(pb.MessageEntity_TEXT_URL, v.Url, func() { r.text(v.Text) })
	case *tl.TextAutoUrl:
		r.styled(pb.MessageEntity_URL, "", func() { r.text(v.Text) })
	case *tl.TextMention:
		r.styled(pb.MessageEntity_MENTION, "", func() { r.text(v.Text) })
	case *tl.TextCustomEmoji:
		r.write(v.Alt)
	case *tl.TextMath:
		r.styled(pb.MessageEntity_CODE, "", func() { r.write(v.Source) })
	case *tl.TextDiff:
		r.text(v.Text)
	default:
		// Обёртки с полем Text (почта, телефон, хэштег, дата, кнопка и т. п.) — их текст.
		if f := reflect.ValueOf(t).Elem().FieldByName("Text"); f.IsValid() {
			if inner, ok := f.Interface().(tl.RichTextClass); ok {
				r.text(inner)
			}
		}
	}
}

// plain — текст без форматирования (ячейки таблицы).
func plainText(t tl.RichTextClass) string {
	var r richBuilder
	r.text(t)
	return strings.ReplaceAll(r.b.String(), "\n", " ")
}

func (r *richBuilder) caption(c tl.PageCaptionClass) {
	if v, ok := c.(*tl.PageCaption); ok {
		r.styled(pb.MessageEntity_ITALIC, "", func() { r.text(v.Text) })
	}
}

func (r *richBuilder) blocks(bs []tl.PageBlockClass) {
	for _, b := range bs {
		r.block(b)
	}
}

func (r *richBuilder) block(b tl.PageBlockClass) {
	switch v := b.(type) {
	case *tl.PageBlockTitle, *tl.PageBlockHeader, *tl.PageBlockHeading1, *tl.PageBlockHeading2, *tl.PageBlockHeading3:
		r.paragraph()
		r.styled(pb.MessageEntity_BOLD, "", func() { r.text(textField(v)) })
		r.newline()
	case *tl.PageBlockSubtitle, *tl.PageBlockSubheader, *tl.PageBlockHeading4, *tl.PageBlockHeading5,
		*tl.PageBlockHeading6, *tl.PageBlockKicker:
		r.newline()
		r.styled(pb.MessageEntity_BOLD, "", func() { r.text(textField(v)) })
		r.newline()
	case *tl.PageBlockParagraph:
		r.paragraph()
		r.text(v.Text)
	case *tl.PageBlockFooter, *tl.PageBlockThinking:
		r.paragraph()
		r.styled(pb.MessageEntity_ITALIC, "", func() { r.text(textField(v)) })
	case *tl.PageBlockPreformatted:
		r.paragraph()
		r.styled(pb.MessageEntity_PRE, "", func() { r.text(v.Text) })
	case *tl.PageBlockMath:
		r.paragraph()
		r.styled(pb.MessageEntity_CODE, "", func() { r.write(v.Source) })
	case *tl.PageBlockDivider:
		r.paragraph()
		r.write("———")
	case *tl.PageBlockBlockquote:
		r.paragraph()
		r.styled(pb.MessageEntity_BLOCKQUOTE, "", func() { r.text(v.Text) })
		if plainText(v.Caption) != "" {
			r.write("\n— ")
			r.text(v.Caption)
		}
	case *tl.PageBlockPullquote:
		r.paragraph()
		r.styled(pb.MessageEntity_ITALIC, "", func() { r.text(v.Text) })
	case *tl.PageBlockBlockquoteBlocks:
		r.paragraph()
		r.styled(pb.MessageEntity_BLOCKQUOTE, "", func() { r.blocks(v.Blocks) })
	case *tl.PageBlockList:
		r.paragraph()
		for _, it := range v.Items {
			r.newline()
			switch x := it.(type) {
			case *tl.PageListItemText:
				r.write(mark(x.Checkbox, x.Checked, "• "))
				r.text(x.Text)
			case *tl.PageListItemBlocks:
				r.write(mark(x.Checkbox, x.Checked, "• "))
				r.blocks(x.Blocks)
			}
		}
	case *tl.PageBlockOrderedList:
		r.paragraph()
		n := int32(1)
		if v.Start != nil {
			n = *v.Start
		}
		for _, it := range v.Items {
			r.newline()
			switch x := it.(type) {
			case *tl.PageListOrderedItemText:
				r.write(mark(x.Checkbox, x.Checked, number(x.Num, n)))
				r.text(x.Text)
			case *tl.PageListOrderedItemBlocks:
				r.write(mark(x.Checkbox, x.Checked, number(x.Num, n)))
				r.blocks(x.Blocks)
			}
			n++
		}
	case *tl.PageBlockTable:
		r.paragraph()
		if plainText(v.Title) != "" {
			r.styled(pb.MessageEntity_BOLD, "", func() { r.text(v.Title) })
			r.newline()
		}
		r.table(v.Rows)
	case *tl.PageBlockDetails:
		r.paragraph()
		r.styled(pb.MessageEntity_BOLD, "", func() { r.text(v.Title) })
		r.newline()
		r.blocks(v.Blocks)
	case *tl.PageBlockButtonRow:
		r.paragraph()
		for i, x := range v.Buttons {
			if btn, ok := x.(*tl.PageButton); ok {
				if i > 0 {
					r.write("   ")
				}
				r.write("[")
				r.text(btn.Text)
				r.write("]")
			}
		}
	case *tl.PageBlockPhoto:
		// Само фото — вложением сообщения (первое); здесь подпись.
		if c, ok := v.Caption.(*tl.PageCaption); ok && plainText(c.Text) != "" {
			r.paragraph()
			r.caption(v.Caption)
		}
	case *tl.PageBlockVideo, *tl.PageBlockAudio, *tl.PageBlockDocument, *tl.PageBlockEmbed, *tl.PageBlockMap,
		*tl.PageBlockCollage, *tl.PageBlockSlideshow:
		r.paragraph()
		r.styled(pb.MessageEntity_ITALIC, "", func() { r.write(mediaWord(v)) })
	case *tl.PageBlockCover:
		r.block(v.Cover)
	case *tl.PageBlockEmbedPost:
		r.paragraph()
		r.styled(pb.MessageEntity_BOLD, "", func() { r.write(v.Author) })
		r.newline()
		r.blocks(v.Blocks)
	case *tl.PageBlockAuthorDate:
		r.newline()
		r.styled(pb.MessageEntity_ITALIC, "", func() { r.text(v.Author) })
	}
}

func textField(b tl.PageBlockClass) tl.RichTextClass {
	if f := reflect.ValueOf(b).Elem().FieldByName("Text"); f.IsValid() {
		if t, ok := f.Interface().(tl.RichTextClass); ok {
			return t
		}
	}
	return nil
}

func mark(checkbox, checked bool, bullet string) string {
	if !checkbox {
		return bullet
	}
	if checked {
		return "☑ "
	}
	return "☐ "
}

func number(num *string, n int32) string {
	if num != nil && *num != "" {
		return *num + ". "
	}
	return fmt.Sprintf("%d. ", n)
}

func mediaWord(b tl.PageBlockClass) string {
	switch b.(type) {
	case *tl.PageBlockVideo:
		return "[видео]"
	case *tl.PageBlockAudio:
		return "[аудио]"
	case *tl.PageBlockDocument:
		return "[файл]"
	case *tl.PageBlockMap:
		return "[карта]"
	case *tl.PageBlockCollage, *tl.PageBlockSlideshow:
		return "[фото]"
	}
	return "[вложение]"
}

// tableWidth — сетка влезает в пузырь моноширинным (знаков в строке).
const tableWidth = 56

func (r *richBuilder) table(rows []tl.PageTableRowClass) {
	var cells [][]string
	var header []bool
	cols := 0
	for _, row := range rows {
		tr, ok := row.(*tl.PageTableRow)
		if !ok {
			continue
		}
		var line []string
		h := false
		for _, c := range tr.Cells {
			if cell, ok := c.(*tl.PageTableCell); ok {
				line = append(line, plainText(cell.Text))
				h = h || cell.Header
			}
		}
		cells = append(cells, line)
		header = append(header, h)
		cols = max(cols, len(line))
	}
	widths := make([]int, cols)
	for _, line := range cells {
		for i, c := range line {
			widths[i] = max(widths[i], utf8.RuneCountInString(c))
		}
	}
	total := 3 * (cols - 1)
	for _, w := range widths {
		total += w
	}
	if total <= tableWidth {
		// Сетка моноширинным: столбцы выровнены пробелами.
		for i, line := range cells {
			r.newline()
			var s strings.Builder
			for j := 0; j < cols; j++ {
				c := ""
				if j < len(line) {
					c = line[j]
				}
				if j > 0 {
					s.WriteString(" │ ")
				}
				s.WriteString(c)
				if j < cols-1 {
					s.WriteString(strings.Repeat(" ", widths[j]-utf8.RuneCountInString(c)))
				}
			}
			k := pb.MessageEntity_CODE
			r.styled(k, "", func() {
				if header[i] {
					r.styled(pb.MessageEntity_BOLD, "", func() { r.write(s.String()) })
				} else {
					r.write(s.String())
				}
			})
		}
		return
	}
	for i, line := range cells {
		r.newline()
		s := strings.Join(line, " │ ")
		if header[i] {
			r.styled(pb.MessageEntity_BOLD, "", func() { r.write(s) })
		} else {
			r.write(s)
		}
	}
}

// RichMessage — текст с форматированием из блоков богатого сообщения и его
// первое фото (вложение для ленты, если своего нет).
func RichMessage(rm tl.RichMessageClass) (string, []*pb.MessageEntity, *pb.Media) {
	v, ok := rm.(*tl.RichMessage)
	if !ok {
		return "", nil, nil
	}
	var r richBuilder
	r.blocks(v.Blocks)
	var media *pb.Media
	if len(v.Photos) > 0 {
		media = photoMedia(v.Photos[0])
	}
	// Только справа: обрезка слева сдвинула бы смещения сущностей.
	return strings.TrimRight(r.b.String(), " \n"), r.ents, media
}
