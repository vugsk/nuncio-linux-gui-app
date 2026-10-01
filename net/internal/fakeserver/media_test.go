package fakeserver

import (
	"testing"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// Вложения чата «Вложения» кодируются (иначе сервер рвёт соединение на getHistory).
func TestMediaChatEncodes(t *testing.T) {
	_, att, files := MediaChat()
	for _, a := range att {
		m := fakeMsg{id: 1, peer: 103, media: a.Media, grouped: a.Grouped}
		if _, err := tlbin.Marshal(m.tl()); err != nil {
			t.Fatalf("вложение %d: %v", a.Index, err)
		}
	}
	a := &Account{Files: files}
	b, ok := a.fileBytes(&tl.InputDocumentFileLocation{Id: 9001}, 512*1024, 512*1024)
	if !ok || len(b) != 512*1024 {
		t.Fatalf("часть файла: %v %d", ok, len(b))
	}
	m := fakeMsg{media: att[6].Media}
	if !m.vote([][]byte{{0}}) {
		t.Fatal("голос не принят")
	}
}
