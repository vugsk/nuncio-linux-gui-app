package files

import (
	"context"
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"nuncio/net/tl"
)

// BigFile — с этого размера файл выгружается как «большой» (saveBigFilePart).
const BigFile = 10 * 1024 * 1024

// MaxParts — предел частей по 512 КБ у Telegram (4000 × 512 КБ ≈ 2 ГБ).
const MaxParts = 4000

// Upload выгружает файл частями в домашний DC, по Workers частей сразу.
// Возвращает InputFile (до BigFile) или InputFileBig. Прогресс — байты
// подтверждённых сервером частей.
func Upload(ctx context.Context, inv Invoker, path string, progress Progress) (tl.InputFileClass, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil {
		return nil, err
	}
	size := st.Size()
	if size == 0 {
		return nil, fmt.Errorf("files: пустой файл")
	}
	parts := int32((size + Part - 1) / Part)
	if parts > MaxParts {
		return nil, fmt.Errorf("files: файл больше 2 ГБ")
	}
	var idb [8]byte
	if _, err := rand.Read(idb[:]); err != nil {
		return nil, err
	}
	fileID := int64(binary.LittleEndian.Uint64(idb[:]))
	big := size >= BigFile

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	next := make(chan int32)
	errs := make(chan error, Workers)
	var done atomic.Int64
	var mu sync.Mutex
	last := time.Time{}
	report := func(force bool) {
		if progress == nil {
			return
		}
		mu.Lock()
		now := time.Now()
		if !force && now.Sub(last) < 200*time.Millisecond {
			mu.Unlock()
			return
		}
		last = now
		mu.Unlock()
		progress(done.Load(), size)
	}
	report(true)
	var wg sync.WaitGroup
	for w := 0; w < Workers; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			buf := make([]byte, Part)
			for p := range next {
				n, err := f.ReadAt(buf, int64(p)*Part)
				if err != nil && err != io.EOF {
					errs <- err
					cancel()
					return
				}
				var req any
				if big {
					req = &tl.UploadSaveBigFilePartRequest{FileId: fileID, FilePart: p, FileTotalParts: parts, Bytes: buf[:n]}
				} else {
					req = &tl.UploadSaveFilePartRequest{FileId: fileID, FilePart: p, Bytes: buf[:n]}
				}
				if err := savePart(ctx, inv, req); err != nil {
					errs <- err
					cancel()
					return
				}
				done.Add(int64(n))
				report(false)
			}
		}()
	}
feed:
	for p := int32(0); p < parts; p++ {
		select {
		case next <- p:
		case <-ctx.Done():
			break feed
		}
	}
	close(next)
	wg.Wait()
	select {
	case err := <-errs:
		return nil, err
	default:
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	report(true)
	name := filepath.Base(path)
	if big {
		return &tl.InputFileBig{Id: fileID, Parts: parts, Name: name}, nil
	}
	return &tl.InputFile{Id: fileID, Parts: parts, Name: name}, nil
}

func savePart(ctx context.Context, inv Invoker, req any) error {
	var v any
	var err error
	switch r := req.(type) {
	case *tl.UploadSaveBigFilePartRequest:
		v, err = inv.InvokeOn(ctx, 0, r)
	case *tl.UploadSaveFilePartRequest:
		v, err = inv.InvokeOn(ctx, 0, r)
	}
	if err != nil {
		return err
	}
	if ok, _ := v.(bool); !ok {
		return fmt.Errorf("files: часть не принята (%v)", v)
	}
	return nil
}
