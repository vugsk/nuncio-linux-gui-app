package auth

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Файл сессии session.bin (Архитектура/nuncio - Данные и хранилище.md, раздел 4).
// Владелец — Go. auth_key лежит только в запечатанном ядром виде (NKS1,
// AES-256-GCM ключом обёртки из Secret Service); остальное — открыто:
//
//	"NSES" | u32 версия=1 | u32 основной DC | i64 id пользователя (0 — не вошли)
//	| u32 число записей | записи: u32 DC | i64 соль | i64 поправка времени, мс
//	| u32 длина | запечатанный ключ
//
// Все числа little-endian. Запись атомарная: временный файл, fsync, rename; права 0600.

const (
	fileMagic   = "NSES"
	fileVersion = 1
	maxEntries  = 32
	maxSealed   = 4096
)

// ErrBadFile — файл сессии повреждён или другой версии.
var ErrBadFile = errors.New("auth: файл сессии повреждён")

type fileEntry struct {
	DC         int
	Salt       int64
	TimeOffset time.Duration
	Sealed     []byte
}

type sessionFile struct {
	Home    int
	UserID  int64
	Entries []fileEntry
}

func (f *sessionFile) marshal() []byte {
	var b bytes.Buffer
	b.WriteString(fileMagic)
	w := func(v any) { _ = binary.Write(&b, binary.LittleEndian, v) }
	w(uint32(fileVersion))
	w(uint32(f.Home))
	w(f.UserID)
	w(uint32(len(f.Entries)))
	for _, e := range f.Entries {
		w(uint32(e.DC))
		w(e.Salt)
		w(e.TimeOffset.Milliseconds())
		w(uint32(len(e.Sealed)))
		b.Write(e.Sealed)
	}
	return b.Bytes()
}

func parseSessionFile(data []byte) (*sessionFile, error) {
	r := bytes.NewReader(data)
	magic := make([]byte, 4)
	if _, err := r.Read(magic); err != nil || string(magic) != fileMagic {
		return nil, ErrBadFile
	}
	var ver, home, n uint32
	var f sessionFile
	rd := func(v any) error { return binary.Read(r, binary.LittleEndian, v) }
	if rd(&ver) != nil || ver != fileVersion || rd(&home) != nil || rd(&f.UserID) != nil || rd(&n) != nil ||
		n > maxEntries {
		return nil, ErrBadFile
	}
	f.Home = int(home)
	for i := uint32(0); i < n; i++ {
		var dc, ln uint32
		var off int64
		var e fileEntry
		if rd(&dc) != nil || rd(&e.Salt) != nil || rd(&off) != nil || rd(&ln) != nil || ln > maxSealed ||
			int(ln) > r.Len() {
			return nil, ErrBadFile
		}
		e.DC, e.TimeOffset = int(dc), time.Duration(off)*time.Millisecond
		e.Sealed = make([]byte, ln)
		if _, err := r.Read(e.Sealed); err != nil {
			return nil, ErrBadFile
		}
		f.Entries = append(f.Entries, e)
	}
	if r.Len() != 0 {
		return nil, ErrBadFile
	}
	return &f, nil
}

// writeAtomic пишет файл с правами 0600 через временный файл и rename.
func writeAtomic(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	tmp, err := os.CreateTemp(filepath.Dir(path), ".session-*.tmp")
	if err != nil {
		return err
	}
	name := tmp.Name()
	defer os.Remove(name) // после успешного rename файла уже нет — ошибка игнорируется
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	if err := os.Rename(name, path); err != nil {
		return fmt.Errorf("auth: запись файла сессии: %w", err)
	}
	if d, err := os.Open(filepath.Dir(path)); err == nil {
		_ = d.Sync()
		d.Close()
	}
	return nil
}
