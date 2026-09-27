// Command nettest — ручная интеграционная проверка сети против настоящего DC
// Telegram (правило 6 плана: реальная сеть только вручную, не на каждом прогоне).
//
// Что делает: соединение (по умолчанию обфусцированный intermediate), обмен
// ключами, ping; если есть ~/.config/nuncio/api.env (API_ID=...), ещё
// help.getConfig и help.getNearestDc. Вход в аккаунт не выполняется, ключ на диск
// не пишется. Журнал без секретов: печатается только auth_key_id (он и так
// передаётся открыто в каждом пакете), время и сводка config.
//
//	go run ./cmd/nettest [-dc 2] [-framing intermediate] [-obfs=true] [-test]
package main

import (
	"bufio"
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"nuncio/net/dc"
	"nuncio/net/tl"
	"nuncio/net/transport"
)

func apiID() (int32, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return 0, err
	}
	f, err := os.Open(filepath.Join(home, ".config", "nuncio", "api.env"))
	if err != nil {
		return 0, err
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		k, v, ok := strings.Cut(strings.TrimSpace(sc.Text()), "=")
		if ok && strings.TrimSpace(k) == "API_ID" {
			n, err := strconv.ParseInt(strings.Trim(strings.TrimSpace(v), `"'`), 10, 32)
			if err != nil {
				return 0, fmt.Errorf("API_ID не число")
			}
			return int32(n), nil
		}
	}
	return 0, fmt.Errorf("в api.env нет API_ID")
}

func main() {
	dcID := flag.Int("dc", 2, "номер DC")
	framing := flag.String("framing", "intermediate", "abridged | intermediate | padded")
	obfs := flag.Bool("obfs", true, "обфускация obfuscated2")
	test := flag.Bool("test", false, "тестовые серверы Telegram")
	flag.Parse()

	fr := map[string]transport.Framing{"abridged": transport.Abridged, "intermediate": transport.Intermediate,
		"padded": transport.PaddedIntermediate}[*framing]
	id, idErr := apiID()
	m := dc.New(dc.Options{
		App: dc.App{APIID: id, DeviceModel: "nuncio nettest", SystemVersion: "Linux", AppVersion: "0.1.0",
			SystemLangCode: "ru", LangCode: "ru"},
		TestDCs: *test, Home: *dcID, Transport: transport.Options{Framing: fr, Obfuscate: *obfs},
		DisablePing: true,
	})
	defer m.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	fmt.Printf("DC %d, транспорт %v, обфускация %v, тестовые серверы %v\n", *dcID, fr, *obfs, *test)
	start := time.Now()
	rtt, err := m.Ping(ctx, *dcID)
	if err != nil {
		fmt.Println("ОШИБКА: соединение / обмен ключами / ping:", err)
		os.Exit(1)
	}
	k := m.Key(*dcID)
	fmt.Printf("обмен ключами и ping: %v (ping %v), auth_key_id %016x, поправка времени %v\n",
		time.Since(start).Round(time.Millisecond), rtt.Round(time.Millisecond), k.Key.ID(), k.TimeOffset.Round(time.Millisecond))

	if idErr != nil {
		fmt.Println("help.getConfig пропущен: нет api_id (", idErr, ")")
		return
	}
	v, err := m.Invoke(ctx, &tl.HelpGetConfigRequest{})
	if err != nil {
		fmt.Println("ОШИБКА: help.getConfig:", err)
		os.Exit(1)
	}
	cfg := v.(*tl.Config)
	fmt.Printf("help.getConfig: слой %d, this_dc %d, test_mode %v, dc_options %d, срок до %v\n",
		tl.Layer, cfg.ThisDc, cfg.TestMode, len(cfg.DcOptions), time.Unix(int64(cfg.Expires), 0).Format(time.RFC3339))
	if v, err := m.Invoke(ctx, &tl.HelpGetNearestDcRequest{}); err == nil {
		fmt.Printf("help.getNearestDc: ближайший DC %d\n", v.(*tl.NearestDc).NearestDc)
	} else {
		fmt.Println("ОШИБКА: help.getNearestDc:", err)
	}
}
