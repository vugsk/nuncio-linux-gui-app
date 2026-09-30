package files

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// server — файл в памяти, отдаёт части по правилам upload.getFile.
type server struct {
	data    []byte
	calls   atomic.Int32
	failAt  int64 // смещение, на котором запрос падает (−1 — нет)
	mu      sync.Mutex
	offsets []int64
}

func (s *server) InvokeOn(_ context.Context, _ int, req tlbin.Request) (any, error) {
	r := req.(*tl.UploadGetFileRequest)
	s.calls.Add(1)
	s.mu.Lock()
	s.offsets = append(s.offsets, r.Offset)
	s.mu.Unlock()
	if r.Offset%4096 != 0 || Part%4096 != 0 || (1<<20)%r.Limit != 0 {
		return nil, errors.New("LIMIT_INVALID")
	}
	if r.Offset == s.failAt {
		return nil, errors.New("обрыв")
	}
	end := min(r.Offset+int64(r.Limit), int64(len(s.data)))
	if r.Offset >= int64(len(s.data)) {
		return &tl.UploadFile{Bytes: []byte{}}, nil
	}
	return &tl.UploadFile{Bytes: s.data[r.Offset:end]}, nil
}

func blob(n int) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte(i * 7)
	}
	return b
}

func TestParallelDownloadAndProgress(t *testing.T) {
	dir := t.TempDir()
	srv := &server{data: blob(5*Part + 1234), failAt: -1}
	path := filepath.Join(dir, "f.bin")
	var last int64
	err := Download(context.Background(), srv, 2, &tl.InputDocumentFileLocation{}, int64(len(srv.data)), path,
		func(done, total int64) { last = done })
	if err != nil {
		t.Fatal(err)
	}
	got, _ := os.ReadFile(path)
	if !bytes.Equal(got, srv.data) {
		t.Fatal("содержимое не совпало")
	}
	if last != int64(len(srv.data)) {
		t.Fatalf("последний прогресс %d из %d", last, len(srv.data))
	}
	if _, err := os.Stat(path + ".part"); !os.IsNotExist(err) {
		t.Fatal("временный файл остался")
	}
	// Уже в кэше — без запросов.
	n := srv.calls.Load()
	if err := Download(context.Background(), srv, 2, &tl.InputDocumentFileLocation{}, int64(len(srv.data)), path, nil); err != nil || srv.calls.Load() != n {
		t.Fatal("повторная загрузка того же файла")
	}
}

func TestResumeAfterFailure(t *testing.T) {
	dir := t.TempDir()
	srv := &server{data: blob(6 * Part), failAt: 3 * Part}
	path := filepath.Join(dir, "f.bin")
	// Одна часть за раз, чтобы непрерывное начало было предсказуемым.
	err := Download(context.Background(), srv, 2, &tl.InputDocumentFileLocation{}, int64(len(srv.data)), path, nil)
	if err == nil {
		t.Fatal("ожидался обрыв")
	}
	srv.failAt = -1
	srv.offsets = nil
	if err := Download(context.Background(), srv, 2, &tl.InputDocumentFileLocation{}, int64(len(srv.data)), path, nil); err != nil {
		t.Fatal(err)
	}
	got, _ := os.ReadFile(path)
	if !bytes.Equal(got, srv.data) {
		t.Fatal("после продолжения содержимое не совпало")
	}
	for _, off := range srv.offsets {
		if off < 3*Part-int64(Workers)*Part {
			t.Fatalf("продолжение запросило давно записанное смещение %d", off)
		}
	}
}

func TestUnknownSizeAndCancel(t *testing.T) {
	dir := t.TempDir()
	srv := &server{data: blob(2*Part + 10), failAt: -1}
	path := filepath.Join(dir, "a.jpg")
	if err := Download(context.Background(), srv, 2, &tl.InputPhotoFileLocation{}, 0, path, nil); err != nil {
		t.Fatal(err)
	}
	got, _ := os.ReadFile(path)
	if !bytes.Equal(got, srv.data) {
		t.Fatal("размер неизвестен — содержимое не совпало")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	srv2 := &server{data: blob(8 * Part), failAt: -1}
	err := Download(ctx, srv2, 2, &tl.InputDocumentFileLocation{}, int64(len(srv2.data)), filepath.Join(dir, "b"), nil)
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("отмена: %v", err)
	}
}
