// Package api — фасад сетевого слоя: принимает команды client.proto,
// исполняет их и складывает события в ограниченную очередь, из которой
// Rust забирает их через nn_next_event (см. Архитектура/nuncio - Границы и
// C ABI.md и nuncio - Сетевой слой (Go).md).
//
// Веха В0: исполняется одна команда Ping (ответ содержит версию ядра).
// Вход, история, обновления — вехи В3–В5.
package api

import (
	"errors"
	"fmt"
	"sync"
	"time"

	"google.golang.org/protobuf/proto"

	"nuncio/net/core"
	pb "nuncio/net/gen"
)

// ProtoVersion — версия client.proto, которую понимает эта сборка.
// Rust передаёт свою в StartRequest; несовпадение — ошибка старта.
const ProtoVersion = 1

// QueueSize — предел очереди событий (обратное давление, правило 5).
const QueueSize = 1024

var (
	ErrAlreadyStarted = errors.New("сервис уже запущен (Go-рантайм нельзя перезапустить)")
	ErrNotStarted     = errors.New("сервис не запущен")
	ErrStopped        = errors.New("сервис остановлен")
	ErrQueueFull      = errors.New("очередь команд переполнена")
	ErrBadVersion     = errors.New("версия client.proto не совпадает")
)

// NextResult — исход ожидания события.
type NextResult int

const (
	GotEvent NextResult = iota
	Timeout
	Stopped
)

type command struct {
	id  uint64
	cmd *pb.Command
}

// Service — единственный экземпляр на процесс.
type Service struct {
	mu       sync.Mutex
	started  bool
	stopped  bool
	commands chan command
	events   chan []byte
	done     chan struct{}
	wg       sync.WaitGroup
}

// New создаёт незапущенный сервис.
func New() *Service {
	return &Service{}
}

// Start проверяет StartRequest и запускает обработчик команд.
func (s *Service) Start(req []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.started {
		return ErrAlreadyStarted
	}
	var sr pb.StartRequest
	if err := proto.Unmarshal(req, &sr); err != nil {
		return fmt.Errorf("разбор StartRequest: %w", err)
	}
	if sr.GetProtoVersion() != ProtoVersion {
		return fmt.Errorf("%w: у приложения %d, у сети %d", ErrBadVersion, sr.GetProtoVersion(), ProtoVersion)
	}
	s.commands = make(chan command, QueueSize)
	s.events = make(chan []byte, QueueSize)
	s.done = make(chan struct{})
	s.started = true
	s.wg.Add(1)
	go s.loop()
	return nil
}

// Submit копирует команду в очередь и не блокирует.
func (s *Service) Submit(id uint64, data []byte) error {
	s.mu.Lock()
	started, stopped := s.started, s.stopped
	s.mu.Unlock()
	if !started {
		return ErrNotStarted
	}
	if stopped {
		return ErrStopped
	}
	var cmd pb.Command
	if err := proto.Unmarshal(data, &cmd); err != nil {
		return fmt.Errorf("разбор Command: %w", err)
	}
	select {
	case s.commands <- command{id: id, cmd: &cmd}:
		return nil
	default:
		return ErrQueueFull
	}
}

// NextEvent ждёт событие не дольше timeout.
func (s *Service) NextEvent(timeout time.Duration) ([]byte, NextResult) {
	s.mu.Lock()
	started := s.started
	s.mu.Unlock()
	if !started {
		return nil, Stopped
	}
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case ev := <-s.events:
		return ev, GotEvent
	case <-s.done:
		// Отдаём накопленное, прежде чем сообщить об остановке.
		select {
		case ev := <-s.events:
			return ev, GotEvent
		default:
			return nil, Stopped
		}
	case <-timer.C:
		return nil, Timeout
	}
}

// Shutdown останавливает обработчик; повторный вызов безопасен.
func (s *Service) Shutdown(timeout time.Duration) error {
	s.mu.Lock()
	if !s.started || s.stopped {
		s.mu.Unlock()
		return nil
	}
	s.stopped = true
	close(s.done)
	s.mu.Unlock()

	finished := make(chan struct{})
	go func() { s.wg.Wait(); close(finished) }()
	select {
	case <-finished:
		return nil
	case <-time.After(timeout):
		return errors.New("обработчик не остановился за отведённое время")
	}
}

func (s *Service) loop() {
	defer s.wg.Done()
	for {
		select {
		case <-s.done:
			return
		case c := <-s.commands:
			s.emit(s.handle(c))
		}
	}
}

// handle исполняет команду; паника превращается в ошибку INTERNAL.
func (s *Service) handle(c command) (ev *pb.Event) {
	defer func() {
		if r := recover(); r != nil {
			ev = resultError(c.id, pb.Error_INTERNAL, fmt.Sprintf("паника: %v", r))
		}
	}()
	switch body := c.cmd.GetBody().(type) {
	case *pb.Command_Ping:
		return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{
			CmdId: c.id,
			Payload: &pb.CommandResult_Pong{Pong: &pb.Pong{
				Message:     body.Ping.GetMessage(),
				CoreVersion: core.Version(),
			}},
		}}}
	default:
		return resultError(c.id, pb.Error_INTERNAL, "команда не поддерживается этой сборкой")
	}
}

func resultError(id uint64, kind pb.Error_Kind, text string) *pb.Event {
	return &pb.Event{Body: &pb.Event_Result{Result: &pb.CommandResult{
		CmdId: id,
		Error: &pb.Error{Kind: kind, Text: text},
	}}}
}

func (s *Service) emit(ev *pb.Event) {
	data, err := proto.Marshal(ev)
	if err != nil {
		data, _ = proto.Marshal(&pb.Event{Body: &pb.Event_Fatal{Fatal: &pb.FatalError{Text: err.Error()}}})
	}
	// Обратное давление: обработчик ждёт, пока Rust заберёт события.
	select {
	case s.events <- data:
	case <-s.done:
	}
}
