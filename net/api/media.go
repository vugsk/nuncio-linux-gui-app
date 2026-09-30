package api

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"nuncio/net/conv"
	"nuncio/net/files"
	pb "nuncio/net/gen"
	"nuncio/net/session"
	"nuncio/net/tl"
)

// Веха В7: скачивание вложений в кэш, отмена, голосование в опросе.

// transfers — отмена передач по id команды, которая их начала.
var transfers sync.Map // uint64 → context.CancelFunc

func location(f *pb.FileRef) tl.InputFileLocationClass {
	if f.GetKind() == pb.FileRef_PHOTO {
		return &tl.InputPhotoFileLocation{Id: f.GetId(), AccessHash: f.GetAccessHash(),
			FileReference: f.GetFileReference(), ThumbSize: f.GetThumbSize()}
	}
	return &tl.InputDocumentFileLocation{Id: f.GetId(), AccessHash: f.GetAccessHash(),
		FileReference: f.GetFileReference(), ThumbSize: f.GetThumbSize()}
}

// cacheName — имя файла в кэше: вид, id, размер превью и расширение.
func cacheName(f *pb.FileRef, name string) string {
	kind := "d"
	ext := strings.ToLower(filepath.Ext(name))
	if f.GetKind() == pb.FileRef_PHOTO || f.GetThumbSize() != "" {
		kind, ext = "p", ".jpg"
	}
	if len(ext) > 8 || strings.ContainsAny(ext, "/\\") {
		ext = ""
	}
	thumb := f.GetThumbSize()
	if thumb == "" {
		thumb = "0"
	}
	return fmt.Sprintf("%s_%d_%s%s", kind, f.GetId(), thumb, ext)
}

func referenceExpired(err error) bool {
	var re *session.RPCError
	return errors.As(err, &re) && strings.HasPrefix(re.Message, "FILE_REFERENCE_")
}

// freshReference — перечитать сообщение и взять новую ссылку на тот же файл.
func (s *Service) freshReference(ctx context.Context, peer *pb.PeerRef, msgID int32, f *pb.FileRef) (*pb.FileRef, error) {
	ev := s.loadMessages(ctx, 0, &pb.LoadMessages{Peer: peer, Ids: []int32{msgID}})
	res := ev.GetResult()
	if res.GetError() != nil {
		return nil, fmt.Errorf("обновить ссылку на файл: %s", res.GetError().GetText())
	}
	for _, m := range res.GetHistory().GetMessages() {
		mi := m.GetMediaInfo()
		cands := []*pb.FileRef{mi.GetFile(), mi.GetFull(), mi.GetThumb(), mi.GetWebpage().GetPhoto()}
		for _, c := range cands {
			if c.GetId() == f.GetId() {
				return &pb.FileRef{Kind: f.GetKind(), Id: f.GetId(), AccessHash: f.GetAccessHash(),
					FileReference: c.GetFileReference(), Dc: f.GetDc(), ThumbSize: f.GetThumbSize(), Size: f.GetSize()}, nil
			}
		}
	}
	return nil, errors.New("файл в сообщении не найден")
}

func (s *Service) downloadFile(ctx context.Context, id uint64, c *pb.DownloadFile) *pb.Event {
	f := c.GetFile()
	if f.GetId() == 0 {
		return resultErrorKind(id, pb.Error_INTERNAL, "нет файла")
	}
	dir := filepath.Join(s.req.GetCacheDir(), "media")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return resultError(id, err)
	}
	path := filepath.Join(dir, cacheName(f, c.GetName()))
	ctx, cancel := context.WithCancel(ctx)
	transfers.Store(id, cancel)
	defer func() {
		transfers.Delete(id)
		cancel()
	}()
	progress := func(done, total int64) {
		s.emit(&pb.Event{Body: &pb.Event_Transfer{Transfer: &pb.TransferProgress{CmdId: id, Done: done, Total: total}}})
	}
	err := files.Download(ctx, s.m, int(f.GetDc()), location(f), f.GetSize(), path, progress)
	if referenceExpired(err) && c.GetMsgId() != 0 {
		// Ссылка на файл устарела: новая — из того же сообщения, одна попытка.
		nf, rerr := s.freshReference(ctx, c.GetPeer(), c.GetMsgId(), f)
		if rerr != nil {
			return resultError(id, rerr)
		}
		err = files.Download(ctx, s.m, int(nf.GetDc()), location(nf), nf.GetSize(), path, progress)
	}
	if err != nil {
		if errors.Is(err, context.Canceled) {
			return resultErrorKind(id, pb.Error_CANCELLED, "отменено")
		}
		return resultError(id, err)
	}
	return payload(id, &pb.FilePath{Path: path})
}

func (s *Service) cancelTransfer(id uint64, c *pb.CancelTransfer) *pb.Event {
	if v, ok := transfers.Load(c.GetCmdId()); ok {
		v.(context.CancelFunc)()
	}
	return payload(id, nil)
}

func (s *Service) sendVote(ctx context.Context, id uint64, c *pb.SendVote) *pb.Event {
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	v, err := s.m.Invoke(ctx, &tl.MessagesSendVoteRequest{Peer: peer, MsgId: c.GetMsgId(), Options: c.GetOptions()})
	if err != nil {
		return resultError(id, err)
	}
	s.processUpdates(ctx, v)
	return payload(id, nil)
}
