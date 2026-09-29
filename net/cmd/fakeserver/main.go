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
