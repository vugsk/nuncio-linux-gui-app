// Package files — передача файлов частями (Архитектура/nuncio - Сетевой слой
// (Go).md, раздел 5): скачивание upload.getFile и выгрузка saveFilePart,
// прогресс, отмена, продолжение после обрыва.
package files

import (
	"context"
	"encoding/binary"
	"fmt"
	"os"
	"sync"
	"time"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// Part — размер части: делит 1 МБ, кратен 4 КБ (правила upload.getFile).
const Part = 512 * 1024

// Workers — частей параллельно.
const Workers = 4

// Invoker — запрос в нужный DC (dc.Manager.InvokeOn: перенос авторизации, FILE_MIGRATE).
type Invoker interface {
	InvokeOn(ctx context.Context, dc int, req tlbin.Request) (any, error)
}

// Progress — сколько байт записано и всего (0 — неизвестно).
type Progress func(done, total int64)

// Download скачивает файл в path: во временный path+".part", по завершении —
// переименование. Непрерывное начало записанного хранится в path+".part.done",
// повторная загрузка продолжает с него. size 0 — размер неизвестен (по частям
// подряд, пока сервер не отдаст неполную).
func Download(ctx context.Context, inv Invoker, dc int, loc tl.InputFileLocationClass, size int64, path string,
	progress Progress) error {
	if st, err := os.Stat(path); err == nil && (size == 0 || st.Size() == size) {
		return nil // уже в кэше
	}
	part, donePath := path+".part", path+".part.done"
	f, err := os.OpenFile(part, os.O_CREATE|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer f.Close()
	start := readDone(donePath)
	if size > 0 && start > size {
		start = 0
	}
	start -= start % Part

	tr := &transfer{f: f, donePath: donePath, size: size, progress: progress, prefix: start, finished: map[int64]int{}}
	tr.report(true)
	if size == 0 {
		err = tr.sequential(ctx, inv, dc, loc, start)
	} else {
		err = tr.parallel(ctx, inv, dc, loc, start)
	}
	if err != nil {
		tr.saveDone()
		return err
	}
	if err := f.Sync(); err != nil {
		return err
	}
	if err := f.Close(); err != nil {
		return err
	}
	_ = os.Remove(donePath)
	return os.Rename(part, path)
}

type transfer struct {
	f        *os.File
	donePath string
	size     int64
	progress Progress

	mu       sync.Mutex
	prefix   int64         // непрерывно записанное начало
	finished map[int64]int // смещение → длина готовых частей за префиксом
	written  int64         // всего записано (для прогресса)
	last     time.Time
}

func readDone(p string) int64 {
	b, err := os.ReadFile(p)
	if err != nil || len(b) != 8 {
		return 0
	}
	return int64(binary.LittleEndian.Uint64(b))
}

func (t *transfer) saveDone() {
	t.mu.Lock()
	n := t.prefix
	t.mu.Unlock()
	var b [8]byte
	binary.LittleEndian.PutUint64(b[:], uint64(n))
	_ = os.WriteFile(t.donePath, b[:], 0o600)
}

func (t *transfer) report(force bool) {
	if t.progress == nil {
		return
	}
	t.mu.Lock()
	now := time.Now()
	if !force && now.Sub(t.last) < 200*time.Millisecond {
		t.mu.Unlock()
		return
	}
	t.last = now
	done := t.prefix + t.written
	t.mu.Unlock()
	if t.size > 0 && done > t.size {
		done = t.size
	}
	t.progress(done, t.size)
}

// chunk — одна часть: запрос и запись по смещению.
func (t *transfer) chunk(ctx context.Context, inv Invoker, dc int, loc tl.InputFileLocationClass, off int64) (int, error) {
	v, err := inv.InvokeOn(ctx, dc, &tl.UploadGetFileRequest{Location: loc, Offset: off, Limit: Part})
	if err != nil {
		return 0, err
	}
	uf, ok := v.(*tl.UploadFile)
	if !ok {
		return 0, fmt.Errorf("files: неожиданный ответ %T (CDN не поддерживается)", v)
	}
	if _, err := t.f.WriteAt(uf.Bytes, off); err != nil {
		return 0, err
	}
	t.mu.Lock()
	if off == t.prefix {
		t.prefix += int64(len(uf.Bytes))
		for {
			n, ok := t.finished[t.prefix]
			if !ok {
				break
			}
			delete(t.finished, t.prefix)
			t.written -= int64(n)
			t.prefix += int64(n)
		}
	} else {
		t.finished[off] = len(uf.Bytes)
		t.written += int64(len(uf.Bytes))
	}
	t.mu.Unlock()
	t.report(false)
	return len(uf.Bytes), nil
}

func (t *transfer) sequential(ctx context.Context, inv Invoker, dc int, loc tl.InputFileLocationClass, off int64) error {
	for {
		n, err := t.chunk(ctx, inv, dc, loc, off)
		if err != nil {
			return err
		}
		off += int64(n)
		if n < Part {
			t.report(true)
			return nil
		}
	}
}

func (t *transfer) parallel(parent context.Context, inv Invoker, dc int, loc tl.InputFileLocationClass, start int64) error {
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	offsets := make(chan int64)
	errs := make(chan error, Workers)
	var wg sync.WaitGroup
	for w := 0; w < Workers; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for off := range offsets {
				if _, err := t.chunk(ctx, inv, dc, loc, off); err != nil {
					errs <- err
					cancel()
					return
				}
			}
		}()
	}
	// Раз в секунду — непрерывное начало на диск (продолжение после обрыва).
	stopSave := make(chan struct{})
	go func() {
		tk := time.NewTicker(time.Second)
		defer tk.Stop()
		for {
			select {
			case <-tk.C:
				t.saveDone()
			case <-stopSave:
				return
			}
		}
	}()
feed:
	for off := start; off < t.size; off += Part {
		select {
		case offsets <- off:
		case <-ctx.Done():
			break feed
		}
	}
	close(offsets)
	wg.Wait()
	close(stopSave)
	select {
	case err := <-errs:
		return err
	default:
	}
	if err := parent.Err(); err != nil {
		return err // отмена снаружи
	}
	t.mu.Lock()
	complete := t.prefix >= t.size
	t.mu.Unlock()
	if !complete {
		return fmt.Errorf("files: получено меньше размера файла")
	}
	t.report(true)
	return nil
}
