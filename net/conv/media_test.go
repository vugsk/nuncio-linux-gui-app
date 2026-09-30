package conv

import (
	"bytes"
	"testing"

	"nuncio/net/tl"
)

func TestStrippedJPEGGeometry(t *testing.T) {
	if len(strippedHeader) != 623 {
		t.Fatalf("длина заголовка %d, ждали 623", len(strippedHeader))
	}
	// SOF0: ffc0 0011 08 <высота 2 байта> <ширина 2 байта>.
	if !bytes.Equal(strippedHeader[158:163], []byte{0xff, 0xc0, 0x00, 0x11, 0x08}) {
		t.Fatalf("SOF0 не на месте: % x", strippedHeader[158:163])
	}
	j := StrippedJPEG([]byte{1, 30, 40, 0xAA, 0xBB})
	if j[164] != 30 || j[166] != 40 || !bytes.HasSuffix(j, []byte{0xAA, 0xBB, 0xff, 0xd9}) {
		t.Fatal("высота, ширина или данные скана не на месте")
	}
	if StrippedJPEG([]byte{2, 1, 1}) != nil {
		t.Fatal("неизвестная версия должна отбрасываться")
	}
}

func TestPhotoSizes(t *testing.T) {
	p := &tl.Photo{Id: 1, DcId: 2, Sizes: []tl.PhotoSizeClass{
		&tl.PhotoStrippedSize{Type: "i", Bytes: []byte{1, 8, 8, 0}},
		&tl.PhotoSize{Type: "m", W: 320, H: 240, Size: 10},
		&tl.PhotoSize{Type: "x", W: 800, H: 600, Size: 50},
		&tl.PhotoSizeProgressive{Type: "y", W: 1280, H: 960, Sizes: []int32{10, 90}},
		&tl.PhotoSizeProgressive{Type: "w", W: 2560, H: 1920, Sizes: []int32{10, 400}},
	}}
	m := MediaInfo(&tl.MessageMediaPhoto{Photo: p})
	if m.GetFile().GetThumbSize() != "y" || m.GetFull().GetThumbSize() != "w" || m.GetFull().GetSize() != 400 {
		t.Fatalf("размеры выбраны неверно: лента %q, полное %q", m.GetFile().GetThumbSize(), m.GetFull().GetThumbSize())
	}
	if m.GetWidth() != 2560 || len(m.GetStripped()) == 0 {
		t.Fatal("нет размеров или мини-превью")
	}
}

func TestVoiceAndFile(t *testing.T) {
	d := &tl.Document{Id: 5, Size: 1234, MimeType: "audio/ogg", Attributes: []tl.DocumentAttributeClass{
		&tl.DocumentAttributeAudio{Voice: true, Duration: 7, Waveform: []byte{1, 2}},
	}}
	m := MediaInfo(&tl.MessageMediaDocument{Document: d})
	if m.GetKind().String() != "VOICE" || m.GetDuration() != 7 || m.GetFile().GetSize() != 1234 {
		t.Fatalf("голосовое: %v", m)
	}
	f := &tl.Document{Id: 6, Size: 99, MimeType: "application/pdf", Attributes: []tl.DocumentAttributeClass{
		&tl.DocumentAttributeFilename{FileName: "отчёт.pdf"},
	}}
	m = MediaInfo(&tl.MessageMediaDocument{Document: f})
	if m.GetKind().String() != "DOCUMENT" || m.GetName() != "отчёт.pdf" {
		t.Fatalf("файл: %v", m)
	}
}
