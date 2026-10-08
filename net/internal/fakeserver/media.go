package fakeserver

import (
	"bytes"
	"fmt"
	"image"
	"image/color"
	"image/jpeg"
	_ "image/png" // выгруженные PNG
	"math/rand"
	"strings"
	"time"

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
	table := "Трек 2: компиляторы\n" + strings.Repeat("─", 120) + "\n│ 1 │ Tree-walking интерпретатор (Lox) │ ★★☆☆☆ │ 1–2 недели │ лексер, парсер, AST │\n" +
		strings.Repeat("─", 120) + "\n│ 2 │ Мини-ассемблер │ ★★☆☆☆ │ 1 неделя │ текст ↔ байты инструкций │"
	caption := "Небольшой процесс\nУ меня пока 2 новости, классная и не очень. " +
		"Я простудился чутка, пока без температуры, но по общим ощущениям немного не очень. " +
		"Ну и ещё готовится открытие одной большой штуки, буквально на днях. Наверное для этого понадобится модерация."
	texts := []string{caption, table, "фото", "", "", "подпись альбома", "отчёт", "опрос", "ссылка https://example.com", "после вложений"}
	doc := make([]byte, 1536*1024+123) // больше части (512 КБ) — скачивание несколькими частями
	rand.New(rand.NewSource(1)).Read(doc)
	files[9001] = doc
	voters := func(n int32) *int32 { return &n }
	site, title, desc := "Example", "Пример сайта", "Описание страницы для превью ссылки."
	att := []Attach{
		{Index: 0, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8006, 900, 600, 160)}},
		{Index: 2, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8001, 800, 600, 40)}},
		{Index: 3, Grouped: 77, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8002, 600, 600, 120)}},
		{Index: 4, Grouped: 77, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8003, 600, 800, 200)}},
		{Index: 5, Grouped: 77, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8004, 640, 480, 250)}},
		{Index: 6, Media: &tl.MessageMediaDocument{Document: &tl.Document{Id: 9001, AccessHash: 5, FileReference: []byte{4},
			Date: 1, MimeType: "application/pdf", Size: int64(len(doc)), DcId: 2,
			Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeFilename{FileName: "отчёт.pdf"}}}}},
		{Index: 7, Media: &tl.MessageMediaPoll{
			Poll: &tl.Poll{Id: 555, Question: textWE("Когда релиз?"), Answers: []tl.PollAnswerClass{
				&tl.PollAnswer{Text: textWE("В пятницу"), Option: []byte{0}},
				&tl.PollAnswer{Text: textWE("В понедельник"), Option: []byte{1}},
			}},
			Results: &tl.PollResults{TotalVoters: voters(3), Results: []tl.PollAnswerVotersClass{
				&tl.PollAnswerVoters{Option: []byte{0}, Voters: voters(2), RecentVoters: []tl.PeerClass{}},
				&tl.PollAnswerVoters{Option: []byte{1}, Voters: voters(1), RecentVoters: []tl.PeerClass{}},
			}},
		}},
		{Index: 8, Media: &tl.MessageMediaWebPage{Webpage: &tl.WebPage{Id: 1, Url: "https://example.com",
			DisplayUrl: "example.com", SiteName: &site, Title: &title, Description: &desc,
			Photo: fakePhoto(files, 8005, 480, 270, 90)}}},
	}
	return texts, att, files
}

// StickerSet — набор стикеров поддельного аккаунта (панель стикеров).
type StickerSet struct {
	ID, Hash int64
	Title    string
	Docs     []*tl.Document
}

// stickerDoc — стикер: статичный WebP (сам файл) или «анимированный» TGS
// (файл — заглушка, показывается превью m из thumb).
func stickerDoc(id int64, webp []byte, tgs bool, set *tl.InputStickerSetID, files, thumbs map[int64][]byte) *tl.Document {
	attrs := []tl.DocumentAttributeClass{&tl.DocumentAttributeImageSize{W: 512, H: 512},
		&tl.DocumentAttributeSticker{Alt: "😀", Stickerset: set}}
	d := &tl.Document{Id: id, AccessHash: id * 3, FileReference: []byte{7}, Date: 1, DcId: 2, Attributes: attrs}
	if tgs {
		files[id] = []byte("tgs")
		thumbs[id] = webp
		d.MimeType, d.Size = "application/x-tgsticker", 3
		d.Thumbs = []tl.PhotoSizeClass{&tl.PhotoSize{Type: "m", W: 128, H: 128, Size: int32(len(webp))}}
	} else {
		files[id] = webp
		d.MimeType, d.Size = "image/webp", int64(len(webp))
	}
	return d
}

// StickerChat — чат «Стикеры» (статичный и «анимированный») и два набора
// для панели: «Котики» (статичные) и «Анимации» (TGS). webp — картинки 512×512.
// video (необязательно) — видеостикер WebM с прозрачностью, videoThumb — его первый кадр WebP.
func StickerChat(webp [][]byte, video, videoThumb []byte, files, thumbs map[int64][]byte) ([]string, []Attach, []StickerSet) {
	setA := &tl.InputStickerSetID{Id: 7001, AccessHash: 71}
	setB := &tl.InputStickerSetID{Id: 7002, AccessHash: 72}
	pick := func(i int) []byte { return webp[i%len(webp)] }
	a := StickerSet{ID: 7001, Hash: 71, Title: "Котики"}
	for i := 0; i < 4; i++ {
		a.Docs = append(a.Docs, stickerDoc(int64(9301+i), pick(i), false, setA, files, thumbs))
	}
	b := StickerSet{ID: 7002, Hash: 72, Title: "Анимации"}
	for i := 0; i < 3; i++ {
		b.Docs = append(b.Docs, stickerDoc(int64(9401+i), pick(i+1), true, setB, files, thumbs))
	}
	att := []Attach{
		{Index: 0, Media: &tl.MessageMediaDocument{Document: a.Docs[0]}},
		{Index: 1, Media: &tl.MessageMediaDocument{Document: b.Docs[0]}},
	}
	texts := []string{"", "", "после стикеров"}
	if len(video) > 0 {
		id := int64(9501)
		files[id], thumbs[id] = video, videoThumb
		d := &tl.Document{Id: id, AccessHash: id * 3, FileReference: []byte{7}, Date: 1, DcId: 2, MimeType: "video/webm",
			Size: int64(len(video)), Attributes: []tl.DocumentAttributeClass{
				&tl.DocumentAttributeVideo{W: 256, H: 256, Duration: 1},
				&tl.DocumentAttributeSticker{Alt: "😀", Stickerset: setA}},
			Thumbs: []tl.PhotoSizeClass{&tl.PhotoSize{Type: "m", W: 128, H: 128, Size: int32(len(videoThumb))}}}
		texts = []string{"", "", "", "после стикеров"}
		att = append(att, Attach{Index: 2, Media: &tl.MessageMediaDocument{Document: d}})
	}
	return texts, att, []StickerSet{a, b}
}

// TtlChat — чат «Секрет»: самоуничтожающееся фото (10 с) и уже истёкшее.
func TtlChat(files map[int64][]byte) ([]string, []Attach) {
	ttl := int32(10)
	return []string{"", "", "после секретного"}, []Attach{
		{Index: 0, Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, 8101, 640, 480, 30), TtlSeconds: &ttl}},
		{Index: 1, Media: &tl.MessageMediaPhoto{TtlSeconds: &ttl}},
	}
}

// fileBytes — часть файла для upload.getFile.
func (a *Account) fileBytes(loc tl.InputFileLocationClass, off int64, limit int32) ([]byte, bool) {
	var id int64
	switch l := loc.(type) {
	case *tl.InputPhotoFileLocation:
		id = l.Id
	case *tl.InputPeerPhotoFileLocation:
		id = l.PhotoId
	case *tl.InputDocumentFileLocation:
		id = l.Id
		if l.ThumbSize != "" {
			if t, ok := a.Thumbs[id]; ok {
				return slice(t, off, limit), true
			}
		}
	default:
		return nil, false
	}
	data, ok := a.Files[id]
	if !ok {
		return nil, false
	}
	return slice(data, off, limit), true
}

func slice(data []byte, off int64, limit int32) []byte {
	if off >= int64(len(data)) {
		return []byte{}
	}
	return data[off:min(off+int64(limit), int64(len(data)))]
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

// Выгрузка и отправка вложений (приём частей, sendMedia, uploadMedia, sendMultiMedia).

func (a *Account) savePart(id int64, part int32, b []byte) {
	if a.uploads == nil {
		a.uploads = map[int64]map[int32][]byte{}
	}
	if a.uploads[id] == nil {
		a.uploads[id] = map[int32][]byte{}
	}
	a.uploads[id][part] = append([]byte(nil), b...)
}

// assemble — выгруженный файл целиком по InputFile / InputFileBig.
func (a *Account) assemble(f tl.InputFileClass) ([]byte, string, error) {
	var id int64
	var parts int32
	var name string
	switch v := f.(type) {
	case *tl.InputFile:
		id, parts, name = v.Id, v.Parts, v.Name
	case *tl.InputFileBig:
		id, parts, name = v.Id, v.Parts, v.Name
	default:
		return nil, "", &rpcErr{400, "FILE_PARTS_INVALID"}
	}
	var out []byte
	for p := int32(0); p < parts; p++ {
		b, ok := a.uploads[id][p]
		if !ok {
			return nil, "", &rpcErr{400, fmt.Sprintf("FILE_PART_%d_MISSING", p)}
		}
		out = append(out, b...)
	}
	delete(a.uploads, id)
	return out, name, nil
}

func (a *Account) newFileID() int64 {
	if a.Files == nil {
		a.Files = map[int64][]byte{}
	}
	a.nextFile++
	return 20000 + a.nextFile
}

// mediaOf — сообщение-вложение из InputMedia запроса.
func (a *Account) mediaOf(in tl.InputMediaClass) (tl.MessageMediaClass, error) {
	switch v := in.(type) {
	case *tl.InputMediaUploadedPhoto:
		data, _, err := a.assemble(v.File)
		if err != nil {
			return nil, err
		}
		cfg, _, err := image.DecodeConfig(bytes.NewReader(data))
		if err != nil {
			return nil, &rpcErr{400, "PHOTO_INVALID"}
		}
		id := a.newFileID()
		a.Files[id] = data
		return &tl.MessageMediaPhoto{Photo: &tl.Photo{Id: id, AccessHash: id * 7, FileReference: []byte{1}, Date: 1, DcId: 2,
			Sizes: []tl.PhotoSizeClass{&tl.PhotoSize{Type: "x", W: int32(cfg.Width), H: int32(cfg.Height), Size: int32(len(data))}}}}, nil
	case *tl.InputMediaUploadedDocument:
		data, _, err := a.assemble(v.File)
		if err != nil {
			return nil, err
		}
		id := a.newFileID()
		a.Files[id] = data
		return &tl.MessageMediaDocument{Document: &tl.Document{Id: id, AccessHash: id * 7, FileReference: []byte{1}, Date: 1,
			MimeType: v.MimeType, Size: int64(len(data)), DcId: 2, Attributes: v.Attributes}}, nil
	case *tl.InputMediaPhoto, *tl.InputMediaDocument:
		var id int64
		if p, ok := v.(*tl.InputMediaPhoto); ok {
			id = p.Id.(*tl.InputPhoto).Id
		} else {
			id = v.(*tl.InputMediaDocument).Id.(*tl.InputDocument).Id
		}
		if m, ok := a.uploadedMedia[id]; ok {
			return m, nil
		}
		for _, st := range a.Stickers {
			for _, d := range st.Docs {
				if d.Id == id {
					return &tl.MessageMediaDocument{Document: d}, nil
				}
			}
		}
		return nil, &rpcErr{400, "MEDIA_INVALID"}
	}
	return nil, &rpcErr{400, "MEDIA_INVALID"}
}

// remember — вложение после messages.uploadMedia (для альбома по ссылке).
func (a *Account) remember(m tl.MessageMediaClass) {
	if a.uploadedMedia == nil {
		a.uploadedMedia = map[int64]tl.MessageMediaClass{}
	}
	switch v := m.(type) {
	case *tl.MessageMediaPhoto:
		a.uploadedMedia[v.Photo.(*tl.Photo).Id] = m
	case *tl.MessageMediaDocument:
		a.uploadedMedia[v.Document.(*tl.Document).Id] = m
	}
}

// postMedia — своё сообщение с вложением: в историю и в ответ Updates.
func (a *Account) postMedia(peer int64, text string, media tl.MessageMediaClass, grouped, random int64,
	u *tl.Updates) error {
	if a.randoms == nil {
		a.randoms = map[int64]int32{}
	}
	if _, dup := a.randoms[random]; dup {
		return &rpcErr{400, "RANDOM_ID_DUPLICATE"}
	}
	a.pts++
	m := fakeMsg{id: int32(len(a.msgs) + 1), peer: peer, out: true, text: text, date: int32(time.Now().Unix()),
		media: media, grouped: grouped}
	a.msgs = append(a.msgs, m)
	a.randoms[random] = m.id
	u.Updates = append(u.Updates, &tl.UpdateMessageID{Id: m.id, RandomId: random},
		&tl.UpdateNewMessage{Message: m.tl(), Pts: a.pts, PtsCount: 1})
	return nil
}

func (a *Account) peerOf(p tl.InputPeerClass) (int64, error) {
	switch v := p.(type) {
	case *tl.InputPeerUser:
		return v.UserId, nil
	case *tl.InputPeerSelf:
		return a.UserID, nil
	}
	return 0, &rpcErr{400, "PEER_ID_INVALID"}
}

func (a *Account) newUpdates() *tl.Updates {
	return &tl.Updates{Updates: []tl.UpdateClass{}, Users: a.contactUsers(), Chats: []tl.ChatClass{},
		Date: int32(time.Now().Unix())}
}

// PhotoChat — чат из n фото (каждое своё, каждые десять — три альбомом) для
// замера памяти при долгой прокрутке; файлы — в files.
func PhotoChat(n int, files map[int64][]byte) ([]string, []Attach) {
	texts := make([]string, n)
	att := make([]Attach, n)
	for i := 0; i < n; i++ {
		w, h := 800, 600
		if i%3 == 1 {
			w, h = 600, 800
		}
		var g int64
		if i%10 < 3 {
			g = int64(1000 + i/10)
		}
		texts[i] = fmt.Sprintf("фото %d", i+1)
		att[i] = Attach{Index: i, Grouped: g,
			Media: &tl.MessageMediaPhoto{Photo: fakePhoto(files, int64(30000+i), w, h, uint8(i*37))}}
	}
	return texts, att
}

// VideoChat — чат с GIF, кружком, видео и голосовым из готовых файлов
// (проверка плеера, В8). У GIF и кружка — превью (thumb_size "m").
func VideoChat(gif, gifThumb, round, roundThumb, video, voice []byte, files, thumbs map[int64][]byte) ([]string, []Attach) {
	files[9101], files[9102], files[9103], files[9104] = video, voice, gif, round
	thumbs[9103], thumbs[9104] = gifThumb, roundThumb
	thumb := func(b []byte, w, h int32) []tl.PhotoSizeClass {
		return []tl.PhotoSizeClass{&tl.PhotoSize{Type: "m", W: w, H: h, Size: int32(len(b))}}
	}
	return []string{"", "", "ролик", "голосовое"}, []Attach{
		{Index: 0, Media: &tl.MessageMediaDocument{Document: &tl.Document{Id: 9103, AccessHash: 5,
			FileReference: []byte{3}, Date: 1, MimeType: "video/mp4", Size: int64(len(gif)), DcId: 2,
			Thumbs: thumb(gifThumb, 320, 240),
			Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeAnimated{},
				&tl.DocumentAttributeVideo{Duration: 3, W: 320, H: 240}}}}},
		{Index: 1, Media: &tl.MessageMediaDocument{Round: true, Document: &tl.Document{Id: 9104, AccessHash: 6,
			FileReference: []byte{4}, Date: 1, MimeType: "video/mp4", Size: int64(len(round)), DcId: 2,
			Thumbs:     thumb(roundThumb, 384, 384),
			Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeVideo{RoundMessage: true, Duration: 4, W: 384, H: 384}}}}},
		{Index: 2, Media: &tl.MessageMediaDocument{Video: true, Document: &tl.Document{Id: 9101, AccessHash: 3,
			FileReference: []byte{1}, Date: 1, MimeType: "video/mp4", Size: int64(len(video)), DcId: 2,
			Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeVideo{Duration: 6, W: 640, H: 360},
				&tl.DocumentAttributeFilename{FileName: "ролик.mp4"}}}}},
		{Index: 3, Media: &tl.MessageMediaDocument{Voice: true, Document: &tl.Document{Id: 9102, AccessHash: 4,
			FileReference: []byte{2}, Date: 1, MimeType: "audio/ogg", Size: int64(len(voice)), DcId: 2,
			Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeAudio{Voice: true, Duration: 3}}}}},
	}
}

// authorizations — сеансы аккаунта: этот и два других (для раздела «Аккаунт»).
func (a *Account) authorizations() []tl.AuthorizationClass {
	if a.sessions == nil {
		a.sessions = []*tl.Authorization{
			{Current: true, Hash: 0, DeviceModel: "nuncio", Platform: "Linux", AppName: "nuncio", AppVersion: "0.1.0",
				Ip: "127.0.0.1", Country: "RU", DateActive: int32(time.Now().Unix())},
			{Hash: 101, DeviceModel: "Pixel 8", Platform: "Android", AppName: "Telegram Android", AppVersion: "12.0",
				Ip: "10.0.0.2", Country: "RU", DateActive: int32(time.Now().Unix()) - 3600},
			{Hash: 102, DeviceModel: "MacBook", Platform: "macOS", AppName: "Telegram Desktop", AppVersion: "6.1",
				Ip: "10.0.0.3", Country: "DE", DateActive: int32(time.Now().Unix()) - 86400},
		}
	}
	out := make([]tl.AuthorizationClass, len(a.sessions))
	for i, s := range a.sessions {
		out[i] = s
	}
	return out
}
