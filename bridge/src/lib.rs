//! Стык Rust ↔ Go (Архитектура/nuncio - Границы и C ABI.md, стык 1).
//! Go собран как c-archive и слинкован в этот крейт (см. build.rs).
//! Rust вызывает Go; обратных вызовов нет: события забирает поток-насос
//! блокирующим `next_event`.
//!
//! Веха В0: `GoBackend` умеет старт, отправку команды, приём события и
//! остановку; из команд есть только `Ping`.

mod ffi;

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/nuncio.client.v0.rs"));
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use domain::Backend;
use prost::Message;

/// Версия client.proto, которую понимает эта сборка (сверяется с Go).
pub const PROTO_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeError {
    /// Go-рантайм уже запускался в этом процессе (перезапуск невозможен).
    AlreadyStarted,
    /// Версии client.proto у приложения и сети не совпали.
    VersionMismatch,
    /// Сервис не запущен или уже остановлен.
    NotRunning,
    /// Очередь команд переполнена (обратное давление).
    QueueFull,
    /// Неверные аргументы или непонятные байты.
    BadArgument,
    /// Паника или внутренняя ошибка на стороне Go.
    Internal,
    /// Событие от Go не разобралось как protobuf.
    Decode(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyStarted => write!(f, "сетевой слой уже запускался в этом процессе"),
            Self::VersionMismatch => write!(f, "версии client.proto у приложения и сети не совпадают"),
            Self::NotRunning => write!(f, "сетевой слой не запущен"),
            Self::QueueFull => write!(f, "очередь команд переполнена"),
            Self::BadArgument => write!(f, "неверный аргумент стыка"),
            Self::Internal => write!(f, "внутренняя ошибка сетевого слоя"),
            Self::Decode(e) => write!(f, "не удалось разобрать событие: {e}"),
        }
    }
}

impl std::error::Error for BridgeError {}

fn map_rc(rc: i32) -> Result<(), BridgeError> {
    match rc {
        ffi::NN_OK => Ok(()),
        ffi::NN_ERR_STATE => Err(BridgeError::NotRunning),
        ffi::NN_ERR_VERSION => Err(BridgeError::VersionMismatch),
        ffi::NN_ERR_FULL => Err(BridgeError::QueueFull),
        ffi::NN_ERR_ARG => Err(BridgeError::BadArgument),
        ffi::NN_ERR_INTERNAL => Err(BridgeError::Internal),
        _ => Err(BridgeError::Internal),
    }
}

/// Исход ожидания события.
#[derive(Debug)]
pub enum NextEvent {
    Event(proto::Event),
    Timeout,
    Stopped,
}

static STARTED: AtomicBool = AtomicBool::new(false);

/// Реальный сетевой слой (Go, c-archive). Один на процесс.
pub struct GoBackend {
    _private: (),
}

impl GoBackend {
    /// Запускает Go-рантайм и сервис. Можно вызвать один раз за процесс.
    pub fn start() -> Result<Self, BridgeError> {
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err(BridgeError::AlreadyStarted);
        }
        let req = proto::StartRequest { proto_version: PROTO_VERSION }.encode_to_vec();
        // SAFETY: указатель и длина описывают живой срез на время вызова; Go копирует вход.
        let rc = unsafe { ffi::nn_start(req.as_ptr(), req.len()) };
        match rc {
            ffi::NN_ERR_STATE => Err(BridgeError::AlreadyStarted),
            rc => map_rc(rc).map(|()| Self { _private: () }),
        }
    }

    /// Ставит команду в очередь; не блокирует.
    pub fn submit(&self, cmd: &proto::Command) -> Result<(), BridgeError> {
        let data = cmd.encode_to_vec();
        // SAFETY: как в start.
        map_rc(unsafe { ffi::nn_submit(cmd.id, data.as_ptr(), data.len()) })
    }

    /// Ждёт событие не дольше `timeout`. Безопасно вызывать из отдельного потока-насоса.
    pub fn next_event(&self, timeout: Duration) -> Result<NextEvent, BridgeError> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        let mut buf = ffi::NnBuf { ptr: std::ptr::null_mut(), len: 0 };
        // SAFETY: buf — валидный выходной параметр; Go заполняет его памятью из malloc.
        let rc = unsafe { ffi::nn_next_event(ms, &mut buf) };
        match rc {
            ffi::NN_TIMEOUT => Ok(NextEvent::Timeout),
            ffi::NN_STOPPED => Ok(NextEvent::Stopped),
            ffi::NN_OK => {
                let bytes = if buf.ptr.is_null() {
                    &[][..]
                } else {
                    // SAFETY: Go выдал buf.len байт по buf.ptr; память живёт до nn_free ниже.
                    unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }
                };
                let decoded = proto::Event::decode(bytes);
                // SAFETY: буфер выдан Go и освобождается ровно один раз.
                unsafe { ffi::nn_free(buf) };
                decoded.map(NextEvent::Event).map_err(|e| BridgeError::Decode(e.to_string()))
            }
            rc => map_rc(rc).map(|()| NextEvent::Timeout),
        }
    }

    /// Останавливает сервис (Go-рантайм при этом остаётся жить до конца процесса).
    pub fn shutdown(&self, timeout: Duration) -> Result<(), BridgeError> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: без аргументов-указателей.
        map_rc(unsafe { ffi::nn_shutdown(ms) })
    }
}

impl Backend for GoBackend {}

/// Заглушка для тестов домена без Go и ядра (наполняется на вехе В3).
#[derive(Default)]
pub struct FakeBackend;

impl Backend for FakeBackend {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_types_roundtrip() {
        let cmd = proto::Command {
            id: 1,
            account_id: 1,
            body: Some(proto::command::Body::Ping(proto::Ping { message: "привет".into() })),
        };
        let back = proto::Command::decode(cmd.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, cmd);
    }
}
