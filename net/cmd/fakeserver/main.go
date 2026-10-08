// Command fakeserver — поддельный Telegram отдельным процессом для сквозных
// проверок приложения (тест kill -9, Р-8): QR подтверждается сам, входящие
// сообщения идут непрерывно, при SIGTERM история сохраняется в JSON.
//
//	go run ./cmd/fakeserver -listen 127.0.0.1:44321 -key key.hex -push 300 -every 15ms -dump out.json
//
// Приложение подключается к нему переменными NUNCIO_TEST_DC и NUNCIO_TEST_RSA
// (только для проверок, см. api/testhooks.go).
package main

import (
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"nuncio/net/internal/fakeserver"
)

func main() {
	listen := flag.String("listen", "127.0.0.1:44321", "адрес")
	keyFile := flag.String("key", "", "куда записать открытый ключ RSA (hex модуля, потом e)")
	push := flag.Int("push", 0, "сколько входящих прислать")
	every := flag.Duration("every", 20*time.Millisecond, "интервал входящих")
	start := flag.Duration("start", 3*time.Second, "задержка перед первым входящим после входа")
	dump := flag.String("dump", "", "куда сохранить историю при остановке")
	history := flag.Int("history", 0, "сколько сообщений заранее в чате (длинная история для прокрутки)")
	unread := flag.Int("unread", 0, "непрочитанных в диалоге (0 — одно, −1 — всё прочитано)")
	extras := flag.Bool("extras", false, "ещё собеседники и папка (поиск, папки, архив — В6)")
	media := flag.Bool("media", false, "чат «Вложения»: фото, альбом, файл, опрос, превью ссылки (В7)")
	partDelay := flag.Duration("part-delay", 0, "задержка на часть файла при скачивании")
	photos := flag.Int("photos", 0, "чат «Фото» из стольких фото, самый новый (замер памяти при прокрутке)")
	video := flag.String("video", "", "чат «Видео»: этот ролик и голосовое из -voice (проверка плеера)")
	voice := flag.String("voice", "", "голосовое (ogg/opus) для чата «Видео»")
	stickers := flag.String("stickers", "", "каталог с s0.webp, s1.webp…: чат «Стикеры» и наборы для панели")
	ttl := flag.Bool("ttl", false, "чат «Секрет»: самоуничтожающееся фото и истёкшее")
	ads := flag.Bool("ads", false, "чат с ботом «Новости» и рекламой в нём (условия API)")
	revoke := flag.Duration("revoke", 0, "через столько после входа завершить все сеансы (как с другого устройства)")
	flag.Parse()

	msgs := []string{"начало"}
	for i := 1; i <= *history; i++ {
		t := fmt.Sprintf("сообщение %d", i)
		switch {
		case i%7 == 0:
			t += "\nвторая строка, чтобы высоты строк были разными"
		case i%11 == 0:
			t += " 🎉👍"
		}
		msgs = append(msgs, t)
	}

	acc := &fakeserver.Account{Phone: "1", Code: "1", Registered: true, UserID: 5, FirstName: "Тест", AutoQR: true,
		Contacts: []fakeserver.Contact{{ID: 100, Name: "Отправитель", AccessHash: 11, Messages: msgs, OutEvery: 3, Unread: *unread}}}
	if *extras {
		// Раньше основного чата (старше по датам): первым в списке остаётся «Отправитель».
		acc.Contacts = append([]fakeserver.Contact{
			{ID: 101, Name: "Мария", AccessHash: 12, About: "дизайнер", Unread: -1,
				Messages: []string{"привет", "релиз в пятницу", "ок, договорились"}},
			{ID: 102, Name: "Архивный", AccessHash: 13, Archived: true, Unread: -1, Messages: []string{"старое"}},
		}, acc.Contacts...)
		acc.Folders = []fakeserver.FakeFolder{{ID: 2, Title: "Работа", Include: []int64{101}}}
	}
	if *stickers != "" {
		var webp [][]byte
		for i := 0; ; i++ {
			b, err := os.ReadFile(fmt.Sprintf("%s/s%d.webp", *stickers, i))
			if err != nil {
				break
			}
			webp = append(webp, b)
		}
		if acc.Files == nil {
			acc.Files = map[int64][]byte{}
		}
		if acc.Thumbs == nil {
			acc.Thumbs = map[int64][]byte{}
		}
		video, _ := os.ReadFile(*stickers + "/v0.webm")
		videoThumb, _ := os.ReadFile(*stickers + "/v0.webp")
		tgs, _ := os.ReadFile(*stickers + "/t0.tgs")
		texts, att, sets := fakeserver.StickerChat(webp, video, videoThumb, tgs, acc.Files, acc.Thumbs)
		acc.Stickers = sets
		acc.Contacts = append(acc.Contacts, fakeserver.Contact{ID: 107, Name: "Стикеры", AccessHash: 18, Unread: -1,
			Messages: texts, Attach: att})
	}
	if *ttl {
		if acc.Files == nil {
			acc.Files = map[int64][]byte{}
		}
		texts, att := fakeserver.TtlChat(acc.Files)
		acc.Contacts = append(acc.Contacts, fakeserver.Contact{ID: 106, Name: "Секрет", AccessHash: 17, Unread: -1,
			Messages: texts, Attach: att})
	}
	if *ads {
		acc.Ads = true
		acc.Contacts = append(acc.Contacts, fakeserver.Contact{ID: 105, Name: "Новости", AccessHash: 16, Unread: -1,
			Bot: true, Messages: []string{"новость 1", "новость 2", "новость 3"}})
	}
	if *media {
		// Новее всех: чат «Вложения» — первый в списке.
		texts, att, files := fakeserver.MediaChat()
		acc.Contacts = append(acc.Contacts, fakeserver.Contact{ID: 103, Name: "Вложения", AccessHash: 14, Unread: -1,
			Messages: texts, Attach: att})
		acc.Files = files
		acc.PartDelay = *partDelay
	}
	if *photos > 0 {
		if acc.Files == nil {
			acc.Files = map[int64][]byte{}
		}
		texts, att := fakeserver.PhotoChat(*photos, acc.Files)
		acc.Contacts = append(acc.Contacts, fakeserver.Contact{ID: 104, Name: "Фото", AccessHash: 15, Unread: -1,
			Messages: texts, Attach: att})
	}
	if *video != "" {
		// Рядом с -video лежат gif.mp4, gif.jpg, round.mp4, round.jpg.
		dir := filepath.Dir(*video)
		read := func(name string) []byte {
			b, err := os.ReadFile(name)
			if err != nil {
				fmt.Fprintln(os.Stderr, "файл для чата «Видео»:", err)
				os.Exit(1)
			}
			return b
		}
		if acc.Files == nil {
			acc.Files = map[int64][]byte{}
		}
		acc.Thumbs = map[int64][]byte{}
		texts, att := fakeserver.VideoChat(read(filepath.Join(dir, "gif.mp4")), read(filepath.Join(dir, "gif.jpg")),
			read(filepath.Join(dir, "round.mp4")), read(filepath.Join(dir, "round.jpg")), read(*video), read(*voice),
			acc.Files, acc.Thumbs)
		acc.Contacts = append(acc.Contacts, fakeserver.Contact{ID: 105, Name: "Видео", AccessHash: 16, Unread: -1,
			Messages: texts, Attach: att})
	}
	srv, err := fakeserver.StartAt(*listen, fakeserver.Options{DC: 2, Account: acc})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	n, e := srv.PublicKey()
	if *keyFile != "" {
		_ = os.WriteFile(*keyFile, []byte(fmt.Sprintf("%s %d\n", hex.EncodeToString(n), e)), 0o600)
	}
	fmt.Println("готов", srv.Addr())

	go func() {
		for acc.SignedIn() == 0 {
			srv.ConfirmQR() // «телефон отсканировал» — updateLoginToken будит QR-цикл клиента
			time.Sleep(300 * time.Millisecond)
		}
		if *revoke > 0 {
			go func() {
				time.Sleep(*revoke)
				acc.Revoke()
				// Клиент узнаёт об этом на следующем запросе: разорвать
				// соединения, чтобы он переподключился и спросил обновления.
				srv.Stop()
				time.Sleep(500 * time.Millisecond)
				if err := srv.Restart(); err != nil {
					fmt.Println("перезапуск:", err)
				}
				fmt.Println("сеансы завершены")
			}()
		}
		time.Sleep(*start)
		for i := 1; i <= *push; i++ {
			id := srv.PushMessage(100, fmt.Sprintf("входящее %d", i), false)
			fmt.Println("push", id)
			time.Sleep(*every)
		}
		fmt.Println("push done")
	}()

	// Прочитанное клиентом — в журнал (проверка «прочитано по мере просмотра»).
	go func() {
		var last int32
		for {
			if r := acc.ReadMax(100); r != last {
				last = r
				fmt.Println("read", r)
			}
			time.Sleep(200 * time.Millisecond)
		}
	}()

	// SIGUSR1 — «выключить сеть», SIGUSR2 — включить.
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGTERM, syscall.SIGINT, syscall.SIGUSR1, syscall.SIGUSR2)
	for s := range sig {
		if s == syscall.SIGUSR1 {
			srv.Stop()
			fmt.Println("сеть выключена")
			continue
		}
		if s == syscall.SIGUSR2 {
			if err := srv.Restart(); err != nil {
				fmt.Println("перезапуск:", err)
			}
			fmt.Println("сеть включена")
			continue
		}
		break
	}
	if *dump != "" {
		data, _ := json.Marshal(acc.Dump())
		_ = os.WriteFile(*dump, data, 0o600)
	}
}
