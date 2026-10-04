package api

import (
	"context"
	"fmt"

	"nuncio/net/conv"
	"nuncio/net/files"
	pb "nuncio/net/gen"
	"nuncio/net/tl"
)

// Веха В9: профиль и активные сеансы (раздел настроек «Профиль», «Аккаунт»).

func (s *Service) getProfile(ctx context.Context, id uint64) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.UsersGetFullUserRequest{Id: &tl.InputUserSelf{}})
	if err != nil {
		return resultError(id, err)
	}
	full, ok := v.(*tl.UsersUserFull)
	if !ok {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	out := &pb.Profile{}
	if f, ok := full.FullUser.(*tl.UserFull); ok && f.About != nil {
		out.About = *f.About
	}
	for _, u := range full.Users {
		if x, ok := u.(*tl.User); ok && x.Self {
			out.Self = conv.User(x)
			if x.FirstName != nil {
				out.FirstName = *x.FirstName
			}
			if x.LastName != nil {
				out.LastName = *x.LastName
			}
			if x.Username != nil {
				out.Username = *x.Username
			}
			if x.Phone != nil {
				out.Phone = *x.Phone
			}
		}
	}
	return payloadAny(id, out)
}

func (s *Service) updateProfile(ctx context.Context, id uint64, c *pb.UpdateProfile) *pb.Event {
	first, last, about := c.GetFirstName(), c.GetLastName(), c.GetAbout()
	if _, err := s.m.Invoke(ctx, &tl.AccountUpdateProfileRequest{FirstName: &first, LastName: &last, About: &about}); err != nil {
		return resultError(id, err)
	}
	if c.GetSetUsername() {
		if _, err := s.m.Invoke(ctx, &tl.AccountUpdateUsernameRequest{Username: c.GetUsername()}); err != nil {
			return resultError(id, err)
		}
	}
	return s.getProfile(ctx, id)
}

func (s *Service) setProfilePhoto(ctx context.Context, id uint64, c *pb.SetProfilePhoto) *pb.Event {
	f, err := files.Upload(ctx, s.m, c.GetPath(), nil)
	if err != nil {
		return resultError(id, err)
	}
	if _, err := s.m.Invoke(ctx, &tl.PhotosUploadProfilePhotoRequest{File: f}); err != nil {
		return resultError(id, err)
	}
	return s.getProfile(ctx, id)
}

func (s *Service) getSessions(ctx context.Context, id uint64) *pb.Event {
	v, err := s.m.Invoke(ctx, &tl.AccountGetAuthorizationsRequest{})
	if err != nil {
		return resultError(id, err)
	}
	list, ok := v.(*tl.AccountAuthorizations)
	if !ok {
		return resultErrorKind(id, pb.Error_INTERNAL, fmt.Sprintf("неожиданный ответ %T", v))
	}
	out := &pb.SessionList{}
	for _, a := range list.Authorizations {
		x, ok := a.(*tl.Authorization)
		if !ok {
			continue
		}
		out.Sessions = append(out.Sessions, &pb.Session{Hash: x.Hash, Current: x.Current, Device: x.DeviceModel,
			Platform: x.Platform + " " + x.SystemVersion, App: x.AppName + " " + x.AppVersion, Ip: x.Ip,
			Country: x.Country, DateActive: int64(x.DateActive)})
	}
	return payloadAny(id, out)
}

func (s *Service) terminateSession(ctx context.Context, id uint64, c *pb.TerminateSession) *pb.Event {
	var err error
	if c.GetHash() == 0 {
		_, err = s.m.Invoke(ctx, &tl.AuthResetAuthorizationsRequest{})
	} else {
		_, err = s.m.Invoke(ctx, &tl.AccountResetAuthorizationRequest{Hash: c.GetHash()})
	}
	if err != nil {
		return resultError(id, err)
	}
	return s.getSessions(ctx, id)
}

// payloadAny — ответы В9 (профиль, сеансы).
func payloadAny(id uint64, p any) *pb.Event {
	r := &pb.CommandResult{CmdId: id}
	switch v := p.(type) {
	case *pb.Profile:
		r.Payload = &pb.CommandResult_Profile{Profile: v}
	case *pb.SessionList:
		r.Payload = &pb.CommandResult_Sessions{Sessions: v}
	}
	return &pb.Event{Body: &pb.Event_Result{Result: r}}
}
