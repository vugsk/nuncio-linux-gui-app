package conv

import (
	"encoding/hex"

	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// Вложения (В7): описание для ленты, скачивания и окна просмотра.

// strippedHeader — заголовок JPEG мини-превью Telegram (photoStrippedSize):
// сервер отдаёт только данные скана, высоту и ширину.
var strippedHeader, _ = hex.DecodeString("ffd8ffe000104a46494600010100000100010000ffdb004300281c1e231e19282321232d2b28303c64413c37373c7b585d4964918099968f808c8aa0b4e6c3a0aadaad8a8cc8ffcbdaeef5ffffff9bc1fffffffaffe6fdfff8ffdb0043012b2d2d3c353c76414176f8a58ca5f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8f8ffc00011080000000003012200021101031101ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f0156272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a636465666768696a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00")

// StrippedJPEG — мини-превью в полноценный JPEG (nil — не то, что ожидали).
func StrippedJPEG(b []byte) []byte {
	if len(b) < 3 || b[0] != 1 || len(strippedHeader) < 167 {
		return nil
	}
	out := make([]byte, 0, len(strippedHeader)+len(b)+2)
	out = append(out, strippedHeader...)
	out[164] = b[1] // высота
	out[166] = b[2] // ширина
	out = append(out, b[3:]...)
	return append(out, 0xff, 0xd9)
}

func sizeBox(s tl.PhotoSizeClass) (typ string, w, h int32, size int64) {
	switch v := s.(type) {
	case *tl.PhotoSize:
		return v.Type, v.W, v.H, int64(v.Size)
	case *tl.PhotoSizeProgressive:
		var n int32
		if len(v.Sizes) > 0 {
			n = v.Sizes[len(v.Sizes)-1]
		}
		return v.Type, v.W, v.H, int64(n)
	}
	return "", 0, 0, 0
}

func stripped(sizes []tl.PhotoSizeClass) []byte {
	for _, s := range sizes {
		if v, ok := s.(*tl.PhotoStrippedSize); ok {
			return StrippedJPEG(v.Bytes)
		}
	}
	return nil
}

// pickSizes — размер для ленты (самый большой не больше 1280 px) и самый большой.
func pickSizes(sizes []tl.PhotoSizeClass) (feed, full tl.PhotoSizeClass) {
	var bestFeed, bestFull int32
	for _, s := range sizes {
		_, w, h, _ := sizeBox(s)
		m := max(w, h)
		if m == 0 {
			continue
		}
		if m > bestFull {
			bestFull, full = m, s
		}
		if m <= 1280 && m > bestFeed {
			bestFeed, feed = m, s
		}
	}
	if feed == nil {
		feed = full
	}
	return feed, full
}

func photoRef(p *tl.Photo, s tl.PhotoSizeClass) *pb.FileRef {
	if s == nil {
		return nil
	}
	typ, _, _, size := sizeBox(s)
	return &pb.FileRef{Kind: pb.FileRef_PHOTO, Id: p.Id, AccessHash: p.AccessHash, FileReference: p.FileReference,
		Dc: p.DcId, ThumbSize: typ, Size: size}
}

func photoMedia(ph tl.PhotoClass) *pb.Media {
	p, ok := ph.(*tl.Photo)
	if !ok {
		return nil
	}
	feed, full := pickSizes(p.Sizes)
	out := &pb.Media{Kind: pb.Media_PHOTO, File: photoRef(p, feed), Full: photoRef(p, full), Stripped: stripped(p.Sizes)}
	if full != nil {
		_, out.Width, out.Height, _ = sizeBox(full)
	}
	return out
}

func documentMedia(dc tl.DocumentClass, video, round, voice bool) *pb.Media {
	d, ok := dc.(*tl.Document)
	if !ok {
		return nil
	}
	out := &pb.Media{Kind: pb.Media_DOCUMENT, Mime: d.MimeType, Size: d.Size,
		File: &pb.FileRef{Kind: pb.FileRef_DOCUMENT, Id: d.Id, AccessHash: d.AccessHash, FileReference: d.FileReference,
			Dc: d.DcId, Size: d.Size}}
	for _, a := range d.Attributes {
		switch v := a.(type) {
		case *tl.DocumentAttributeFilename:
			out.Name = v.FileName
		case *tl.DocumentAttributeAudio:
			out.Duration = v.Duration
			out.Waveform = v.Waveform
			out.Title = str(v.Title)
			out.Performer = str(v.Performer)
			if v.Voice || voice {
				out.Kind = pb.Media_VOICE
			} else {
				out.Kind = pb.Media_AUDIO
			}
		case *tl.DocumentAttributeVideo:
			out.Duration = int32(v.Duration)
			out.Width, out.Height = v.W, v.H
			if v.RoundMessage || round {
				out.Kind = pb.Media_VIDEO_NOTE
			} else if out.Kind == pb.Media_DOCUMENT {
				out.Kind = pb.Media_VIDEO
			}
		case *tl.DocumentAttributeImageSize:
			out.Width, out.Height = v.W, v.H
		case *tl.DocumentAttributeSticker:
			out.Kind = pb.Media_STICKER
			out.Emoji = v.Alt
		case *tl.DocumentAttributeAnimated:
			out.Kind = pb.Media_GIF
		}
	}
	if video && out.Kind == pb.Media_DOCUMENT {
		out.Kind = pb.Media_VIDEO
	}
	// Превью документа: самый большой обычный размер.
	var best int32
	for _, t := range d.Thumbs {
		typ, w, h, size := sizeBox(t)
		if m := max(w, h); m > best {
			best = m
			out.Thumb = &pb.FileRef{Kind: pb.FileRef_DOCUMENT, Id: d.Id, AccessHash: d.AccessHash,
				FileReference: d.FileReference, Dc: d.DcId, ThumbSize: typ, Size: size}
		}
	}
	out.Stripped = stripped(d.Thumbs)
	return out
}

func text(t tl.TextWithEntitiesClass) string {
	if v, ok := t.(*tl.TextWithEntities); ok {
		return v.Text
	}
	return ""
}

// MediaInfo — вложение сообщения подробно (nil — нет вложения).
func MediaInfo(m tl.MessageMediaClass) *pb.Media {
	switch v := m.(type) {
	case *tl.MessageMediaPhoto:
		m := photoMedia(v.Photo)
		if v.TtlSeconds != nil {
			// Самоуничтожающееся; без фото — срок уже истёк.
			if m == nil {
				m = &pb.Media{Kind: pb.Media_PHOTO}
			}
			m.TtlSeconds = *v.TtlSeconds
		}
		return m
	case *tl.MessageMediaDocument:
		m := documentMedia(v.Document, v.Video, v.Round, v.Voice)
		if v.TtlSeconds != nil {
			if m == nil {
				m = &pb.Media{Kind: pb.Media_VIDEO}
			}
			m.TtlSeconds = *v.TtlSeconds
		}
		return m
	case *tl.MessageMediaWebPage:
		w, ok := v.Webpage.(*tl.WebPage)
		if !ok {
			return nil
		}
		wp := &pb.WebPage{Url: w.Url, Site: str(w.SiteName), Title: str(w.Title), Description: str(w.Description)}
		if ph := photoMedia(w.Photo); ph != nil {
			wp.Photo, wp.PhotoStripped, wp.PhotoWidth, wp.PhotoHeight = ph.File, ph.Stripped, ph.Width, ph.Height
		}
		return &pb.Media{Kind: pb.Media_WEBPAGE, Webpage: wp}
	case *tl.MessageMediaPoll:
		p, ok := v.Poll.(*tl.Poll)
		if !ok {
			return nil
		}
		poll := &pb.Poll{Id: p.Id, Question: text(p.Question), Closed: p.Closed, Multiple: p.MultipleChoice, Quiz: p.Quiz}
		votes := map[string]*tl.PollAnswerVoters{}
		if r, ok := v.Results.(*tl.PollResults); ok {
			if r.TotalVoters != nil {
				poll.TotalVoters = *r.TotalVoters
			}
			for _, a := range r.Results {
				if x, ok := a.(*tl.PollAnswerVoters); ok {
					votes[string(x.Option)] = x
				}
			}
		}
		for _, a := range p.Answers {
			ans, ok := a.(*tl.PollAnswer)
			if !ok {
				continue
			}
			o := &pb.PollOption{Text: text(ans.Text), Option: ans.Option}
			if x := votes[string(ans.Option)]; x != nil {
				o.Chosen, o.Correct = x.Chosen, x.Correct
				if x.Voters != nil {
					o.Voters = *x.Voters
				}
			}
			poll.Options = append(poll.Options, o)
		}
		return &pb.Media{Kind: pb.Media_POLL, Poll: poll}
	case nil, *tl.MessageMediaEmpty:
		return nil
	}
	return &pb.Media{Kind: pb.Media_OTHER}
}
