package fakeserver

import (
	"bytes"
	"image"
	"image/color"
	"image/jpeg"
	"math/rand"

	"nuncio/net/tl"
)

// Вложения поддельного аккаунта (В7): фото, альбом, файл, опрос, превью
// ссылки; файлы отдаются upload.getFile частями.

// Attach — вложение сообщения контакта: номер в Messages, медиа, альбом.
type Attach struct {
	Index   int
	Media   tl.MessageMediaClass
	Grouped int64
}

// photoJPEG — картинка w×h с градиентом (разные цвета — разные фото).
func photoJPEG(w, h int, hue uint8) []byte {
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for y := 0; y < h; y++ {
		for x := 0; x < w; x++ {
			img.Set(x, y, color.RGBA{uint8(x * 255 / w), uint8(y * 255 / h), hue, 255})
		}
	}
	var b bytes.Buffer
	_ = jpeg.Encode(&b, img, &jpeg.Options{Quality: 80})
	return b.Bytes()
}

func fakePhoto(files map[int64][]byte, id int64, w, h int, hue uint8) *tl.Photo {
	data := photoJPEG(w, h, hue)
	files[id] = data
	return &tl.Photo{Id: id, AccessHash: id * 7, FileReference: []byte{1, 2, 3}, Date: 1, DcId: 2,
		Sizes: []tl.PhotoSizeClass{&tl.PhotoSize{Type: "x", W: int32(w), H: int32(h), Size: int32(len(data))}}}
}

func textWE(s string) *tl.TextWithEntities {
	return &tl.TextWithEntities{Text: s, Entities: []tl.MessageEntityClass{}}
}

// MediaChat — чат «Вложения»: подписи сообщений и вложения к ним, плюс
// содержимое файлов по id (для upload.getFile).
func MediaChat() ([]string, []Attach, map[int64][]byte) {
	files := map[int64][]byte{}
	texts := []string{"фото", "", "", "подпись альбома", "отчёт", "опрос", "ссылка https://example.com", "после вложений"}
	doc := make([]byte, 1536*1024+123) // больше части (512 КБ) — скачивание несколькими частями
	rand.New(rand.NewSource(1)).Read(doc)
	files[9001] = doc
	voters := func(n int32) *int32 { return &n }
	site, title, desc := "Example", "Пример сайта", "Описание страницы для превью ссылки."
	att := []Attach{
		{Index: 0, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8001, 800, 600, 40)}},
		{Index: 1, Grouped: 77, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8002, 600, 600, 120)}},
		{Index: 2, Grouped: 77, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8003, 600, 800, 200)}},
		{Index: 3, Grouped: 77, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8004, 640, 480, 250)}},
		{Index: 4, Media: &tl.MessageMediaDocument{Document: &tl.Document{Id: 9001, AccessHash: 5, FileReference: []byte{4},
			Date: 1, MimeType: "application/pdf", Size: int64(len(doc)), DcId: 2,
			Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeFilename{FileName: "отчёт.pdf"}}}}},
		{Index: 5, Media: &tl.MessageMediaPoll{
			Poll: &tl.Poll{Id: 555, Question: textWE("Когда релиз?"), Answers: []tl.PollAnswerClass{
				&tl.PollAnswer{Text: textWE("В пятницу"), Option: []byte{0}},
				&tl.PollAnswer{Text: textWE("В понедельник"), Option: []byte{1}},
			}},
			Results: &tl.PollResults{TotalVoters: voters(3), Results: []tl.PollAnswerVotersClass{
				&tl.PollAnswerVoters{Option: []byte{0}, Voters: voters(2), RecentVoters: []tl.PeerClass{}},
				&tl.PollAnswerVoters{Option: []byte{1}, Voters: voters(1), RecentVoters: []tl.PeerClass{}},
			}},
		}},
		{Index: 6, Media: &tl.MessageMediaWebPage{Webpage: &tl.WebPage{Id: 1, Url: "https://example.com",
			DisplayUrl: "example.com", SiteName: &site, Title: &title, Description: &desc,
			Photo: fakePhoto(files, 8005, 480, 270, 90)}}},
	}
	return texts, att, files
}

// fileBytes — часть файла для upload.getFile.
func (a *Account) fileBytes(loc tl.InputFileLocationClass, off int64, limit int32) ([]byte, bool) {
	var id int64
	switch l := loc.(type) {
	case *tl.InputPhotoFileLocation:
		id = l.Id
	case *tl.InputDocumentFileLocation:
		id = l.Id
	default:
		return nil, false
	}
	data, ok := a.Files[id]
	if !ok {
		return nil, false
	}
	if off >= int64(len(data)) {
		return []byte{}, true
	}
	end := min(off+int64(limit), int64(len(data)))
	return data[off:end], true
}

// vote — голос в опросе сообщения: отметить выбранное, прибавить голоса.
func (m *fakeMsg) vote(options [][]byte) bool {
	mp, ok := m.media.(*tl.MessageMediaPoll)
	if !ok {
		return false
	}
	r, ok := mp.Results.(*tl.PollResults)
	if !ok {
		return false
	}
	for _, o := range options {
		for _, x := range r.Results {
			v := x.(*tl.PollAnswerVoters)
			if bytes.Equal(v.Option, o) {
				v.Chosen = true
				n := *v.Voters + 1
				v.Voters = &n
			}
		}
	}
	t := *r.TotalVoters + 1
	r.TotalVoters = &t
	return true
}
