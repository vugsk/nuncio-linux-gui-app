package api

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"nuncio/net/conv"
	"nuncio/net/files"
	pb "nuncio/net/gen"
	"nuncio/net/portal"
	"nuncio/net/tl"
)

// Веха В7: выбор файлов системным диалогом, сохранение из кэша, отправка вложений.

// testPick — для сквозных проверок вместо портала (иначе диалог откроется на
// настоящем рабочем столе): NUNCIO_TEST_PICK — пути через «:»,
// NUNCIO_TEST_SAVE — каталог, куда «сохраняет» диалог сохранения.
// «@файл» — пути построчно из файла (выбор меняется между шагами проверки).
func testPick() ([]string, bool) {
	v, ok := os.LookupEnv("NUNCIO_TEST_PICK")
	if !ok || os.Getenv("NUNCIO_TEST_DC") == "" {
		return nil, false
	}
	if list, ok := strings.CutPrefix(v, "@"); ok {
		data, err := os.ReadFile(list)
		if err != nil {
			return nil, true
		}
		return strings.Fields(string(data)), true
	}
	return strings.Split(v, ":"), true
}

func (s *Service) pickFiles(ctx context.Context, id uint64, c *pb.PickFiles) *pb.Event {
	if paths, ok := testPick(); ok {
		return payload(id, &pb.FilePaths{Paths: paths})
	}
	paths, err := portal.OpenFiles(ctx, c.GetTitle(), c.GetMultiple())
	if errors.Is(err, portal.ErrCancelled) {
		return payload(id, &pb.FilePaths{})
	}
	if err != nil {
		return resultError(id, err)
	}
	s.log.Printf("выбрано файлов: %d", len(paths))
	return payload(id, &pb.FilePaths{Paths: paths})
}

func (s *Service) saveFileAs(ctx context.Context, id uint64, c *pb.SaveFileAs) *pb.Event {
	var dst string
	var err error
	if dir := os.Getenv("NUNCIO_TEST_SAVE"); dir != "" && os.Getenv("NUNCIO_TEST_DC") != "" {
		dst = filepath.Join(dir, c.GetName())
	} else {
		dst, err = portal.SaveFile(ctx, c.GetTitle(), c.GetName())
	}
	if errors.Is(err, portal.ErrCancelled) {
		return payload(id, &pb.FilePath{})
	}
	if err != nil {
		return resultError(id, err)
	}
	if err := copyFile(c.GetSrc(), dst); err != nil {
		return resultError(id, err)
	}
	return payload(id, &pb.FilePath{Path: dst})
}

func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	tmp := dst + ".part"
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		_ = os.Remove(tmp)
		return err
	}
	if err := out.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, dst)
}

// inputMedia — выгруженный файл как фото или как документ с именем.
func inputMedia(f tl.InputFileClass, it *pb.MediaUpload) tl.InputMediaClass {
	if it.GetAsPhoto() {
		return &tl.InputMediaUploadedPhoto{File: f}
	}
	mime := it.GetMime()
	if mime == "" {
		mime = "application/octet-stream"
	}
	return &tl.InputMediaUploadedDocument{File: f, MimeType: mime, ForceFile: true,
		Attributes: []tl.DocumentAttributeClass{&tl.DocumentAttributeFilename{FileName: it.GetName()}}}
}

// uploaded — медиа, уже лежащее на сервере (messages.uploadMedia), для альбома.
func uploaded(m tl.MessageMediaClass) (tl.InputMediaClass, error) {
	switch v := m.(type) {
	case *tl.MessageMediaPhoto:
		if p, ok := v.Photo.(*tl.Photo); ok {
			return &tl.InputMediaPhoto{Id: &tl.InputPhoto{Id: p.Id, AccessHash: p.AccessHash, FileReference: p.FileReference}}, nil
		}
	case *tl.MessageMediaDocument:
		if d, ok := v.Document.(*tl.Document); ok {
			return &tl.InputMediaDocument{Id: &tl.InputDocument{Id: d.Id, AccessHash: d.AccessHash, FileReference: d.FileReference}}, nil
		}
	}
	return nil, fmt.Errorf("messages.uploadMedia: неожиданный ответ %T", m)
}

func (s *Service) sendMedia(ctx context.Context, id uint64, c *pb.SendMedia) *pb.Event {
	items := c.GetItems()
	if len(items) == 0 {
		return resultErrorKind(id, pb.Error_INTERNAL, "нет вложений")
	}
	peer, err := conv.InputPeer(c.GetPeer())
	if err != nil {
		return resultError(id, err)
	}
	ctx, cancel := context.WithCancel(ctx)
	transfers.Store(id, cancel)
	defer func() {
		transfers.Delete(id)
		cancel()
	}()
	// Общий ход по всем файлам. Пропавший файл — постоянная ошибка (не повторять).
	var total, before int64
	for _, it := range items {
		st, err := os.Stat(it.GetPath())
		if err != nil {
			return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{CmdId: id,
				Error: &pb.Error{Kind: pb.Error_RPC, Code: 400, Text: "FILE_NOT_FOUND"}}}}
		}
		total += st.Size()
	}
	progress := func(done, _ int64) {
		s.emit(&pb.Event{Body: &pb.Event_Transfer{Transfer: &pb.TransferProgress{CmdId: id, Done: before + done, Total: total}}})
	}
	var replyTo tl.InputReplyToClass
	if r := c.GetReplyTo(); r != 0 {
		replyTo = &tl.InputReplyToMessage{ReplyToMsgId: r}
	}
	fail := func(err error) *pb.Event {
		if errors.Is(err, context.Canceled) {
			return resultErrorKind(id, pb.Error_CANCELLED, "отменено")
		}
		s.auth.CheckAuthError(err)
		return resultError(id, err)
	}
	medias := make([]tl.InputMediaClass, 0, len(items))
	for _, it := range items {
		f, err := files.Upload(ctx, s.m, it.GetPath(), progress)
		if err != nil {
			return fail(err)
		}
		if st, err := os.Stat(it.GetPath()); err == nil {
			before += st.Size()
		}
		medias = append(medias, inputMedia(f, it))
	}
	var v any
	if len(items) == 1 {
		it := items[0]
		v, err = s.m.Invoke(ctx, &tl.MessagesSendMediaRequest{Peer: peer, ReplyTo: replyTo, Media: medias[0],
			Message: it.GetCaption(), RandomId: it.GetRandomId(), Entities: conv.InputEntities(it.GetEntities())})
	} else {
		multi := make([]tl.InputSingleMediaClass, 0, len(items))
		for i, it := range items {
			// Альбом: каждое вложение сначала на сервер (uploadMedia), потом ссылкой.
			m, err := s.m.Invoke(ctx, &tl.MessagesUploadMediaRequest{Peer: peer, Media: medias[i]})
			if err != nil {
				return fail(err)
			}
			mc, ok := m.(tl.MessageMediaClass)
			if !ok {
				return fail(fmt.Errorf("messages.uploadMedia: неожиданный ответ %T", m))
			}
			in, err := uploaded(mc)
			if err != nil {
				return fail(err)
			}
			multi = append(multi, &tl.InputSingleMedia{Media: in, RandomId: it.GetRandomId(), Message: it.GetCaption(),
				Entities: conv.InputEntities(it.GetEntities())})
		}
		v, err = s.m.Invoke(ctx, &tl.MessagesSendMultiMediaRequest{Peer: peer, ReplyTo: replyTo, MultiMedia: multi})
	}
	if err != nil {
		return fail(err)
	}
	u, ok := v.(tl.UpdatesClass)
	if !ok {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	s.upd.Process(ctx, u, nil)
	out := &pb.SentList{}
	for _, it := range items {
		out.Messages = append(out.Messages, &pb.Message{PeerId: c.GetPeer().GetId(), Id: s.upd.SentID(it.GetRandomId()),
			RandomId: it.GetRandomId(), FromId: s.auth.UserID(), Out: true, Text: it.GetCaption(), ReplyTo: c.GetReplyTo()})
	}
	s.log.Printf("отправлено вложений: %d", len(items))
	return payload(id, out)
}
