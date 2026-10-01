package files

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"

	"nuncio/net/tl"
	"nuncio/net/tlbin"
)

// sink — сервер выгрузки: собирает части по номерам.
type sink struct {
	mu     sync.Mutex
	parts  map[int32][]byte
	total  int32
	big    bool
	failAt int32 // номер части, на котором запрос падает (−1 — нет)
}

func (s *sink) InvokeOn(_ context.Context, _ int, req tlbin.Request) (any, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var p int32
	var b []byte
	switch r := req.(type) {
	case *tl.UploadSaveFilePartRequest:
		p, b = r.FilePart, r.Bytes
	case *tl.UploadSaveBigFilePartRequest:
		p, b, s.total, s.big = r.FilePart, r.Bytes, r.FileTotalParts, true
	}
	if p == s.failAt {
		return nil, errors.New("обрыв")
	}
	if len(b) > Part {
		return nil, errors.New("FILE_PART_SIZE_INVALID")
	}
	s.parts[p] = append([]byte(nil), b...)
	return true, nil
}

// joined — файл из частей; все части, кроме последней, ровно по Part.
func (s *sink) joined() []byte {
	var out []byte
	n := int32(len(s.parts))
	for i := int32(0); i < n; i++ {
		if i < n-1 && len(s.parts[i]) != Part {
			return nil
		}
		out = append(out, s.parts[i]...)
	}
	return out
}

func TestUploadSmallAndProgress(t *testing.T) {
	path := filepath.Join(t.TempDir(), "doc.pdf")
	data := blob(3*Part + 1000)
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	s := &sink{parts: map[int32][]byte{}, failAt: -1}
	var last int64
	f, err := Upload(context.Background(), s, path, func(done, total int64) { last = done })
	if err != nil {
		t.Fatal(err)
	}
	in, ok := f.(*tl.InputFile)
	if !ok || in.Parts != 4 || in.Name != "doc.pdf" {
		t.Fatalf("InputFile: %#v", f)
	}
	if !bytes.Equal(s.joined(), data) || s.big {
		t.Fatal("собрано не то")
	}
	if last != int64(len(data)) {
		t.Fatalf("прогресс %d", last)
	}
}

func TestUploadBigAndFailure(t *testing.T) {
	path := filepath.Join(t.TempDir(), "big.bin")
	data := blob(BigFile + 5)
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatal(err)
	}
	s := &sink{parts: map[int32][]byte{}, failAt: -1}
	f, err := Upload(context.Background(), s, path, nil)
	if err != nil {
		t.Fatal(err)
	}
	if b, ok := f.(*tl.InputFileBig); !ok || b.Parts != 21 || s.total != 21 || !s.big {
		t.Fatalf("InputFileBig: %#v, всего %d", f, s.total)
	}
	if !bytes.Equal(s.joined(), data) {
		t.Fatal("собрано не то")
	}
	s = &sink{parts: map[int32][]byte{}, failAt: 3}
	if _, err := Upload(context.Background(), s, path, nil); err == nil {
		t.Fatal("ошибка части не дошла")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := Upload(ctx, &sink{parts: map[int32][]byte{}, failAt: -1}, path, nil); !errors.Is(err, context.Canceled) {
		t.Fatalf("отмена: %v", err)
	}
}
