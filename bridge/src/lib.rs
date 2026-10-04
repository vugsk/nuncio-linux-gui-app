//! Стык Rust ↔ Go (Архитектура/nuncio - Границы и C ABI.md, стык 1).
//! Go собран как c-archive и слинкован в этот крейт (см. build.rs).
//! Rust вызывает Go; обратных вызовов нет: события забирает поток-насос
//! блокирующим `next_event`.
//!
//! `GoBackend` и `FakeBackend` реализуют `domain::Backend`; отображение
//! доменных типов в protobuf — в `map`.

mod ffi;
pub mod map;

// Сгенерированный prost код: размеры вариантов oneof задаёт схема, не мы.
#[allow(clippy::large_enum_variant)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/nuncio.client.v0.rs"));
}

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use domain::model::{Command, Event, ProxySettings, UpdateState};
use domain::{Backend, BackendError, Next};
use prost::Message;

/// Версия client.proto, которую понимает эта сборка (сверяется с Go).
pub const PROTO_VERSION: u32 = 10;

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

impl From<BridgeError> for BackendError {
    fn from(e: BridgeError) -> Self {
        match e {
            BridgeError::NotRunning => BackendError::NotRunning,
            BridgeError::QueueFull => BackendError::QueueFull,
            e => BackendError::Other(e.to_string()),
        }
    }
}

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

/// Исход ожидания события (сырые protobuf-события). Живёт одно мгновение,
/// поэтому разница размеров вариантов не важна.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum NextEvent {
    Event(proto::Event),
    Timeout,
    Stopped,
}

/// Параметры старта сетевого слоя (`StartRequest`).
#[derive(Clone, Default)]
pub struct StartParams {
    pub api_id: i32,
    pub api_hash: String,
    pub data_dir: String,
    pub log_dir: String,
    pub device_model: String,
    pub system_version: String,
    pub app_version: String,
    pub lang_code: String,
    pub system_lang_code: String,
    pub test_servers: bool,
    pub proxy: ProxySettings,
    pub keyring_app: String,
    pub cache_dir: String,
    /// Состояние обновлений из базы (О-5); `None` — новый вход.
    pub update_state: Option<UpdateState>,
    pub channel_states: Vec<(i64, i32)>,
}

static STARTED: AtomicBool = AtomicBool::new(false);

/// Реальный сетевой слой (Go, c-archive). Один на процесс.
pub struct GoBackend {
    _private: (),
}

impl GoBackend {
    /// Запускает Go-рантайм и сервис. Можно вызвать один раз за процесс.
    pub fn start(p: &StartParams) -> Result<Self, BridgeError> {
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err(BridgeError::AlreadyStarted);
        }
        let req = proto::StartRequest {
            proto_version: PROTO_VERSION,
            api_id: p.api_id,
            api_hash: p.api_hash.clone(),
            data_dir: p.data_dir.clone(),
            device_model: p.device_model.clone(),
            system_version: p.system_version.clone(),
            app_version: p.app_version.clone(),
            lang_code: p.lang_code.clone(),
            system_lang_code: p.system_lang_code.clone(),
            test_servers: p.test_servers,
            proxy: Some(map::proxy_to(&p.proxy)),
            log_dir: p.log_dir.clone(),
            keyring_app: p.keyring_app.clone(),
            update_state: p.update_state.map(map::state_to),
            cache_dir: p.cache_dir.clone(),
            channel_states: p.channel_states.iter().map(|&(peer_id, pts)| proto::ChannelState { peer_id, pts }).collect(),
        }
        .encode_to_vec();
        // SAFETY: указатель и длина описывают живой срез на время вызова; Go копирует вход.
        let rc = unsafe { ffi::nn_start(req.as_ptr(), req.len()) };
        match rc {
            ffi::NN_ERR_STATE => Err(BridgeError::AlreadyStarted),
            rc => map_rc(rc).map(|()| Self { _private: () }),
        }
    }

    /// Ставит protobuf-команду в очередь; не блокирует.
    pub fn submit_raw(&self, cmd: &proto::Command) -> Result<(), BridgeError> {
        let data = cmd.encode_to_vec();
        // SAFETY: как в start.
        map_rc(unsafe { ffi::nn_submit(cmd.id, data.as_ptr(), data.len()) })
    }

    /// Ждёт protobuf-событие не дольше `timeout`.
    pub fn next_raw(&self, timeout: Duration) -> Result<NextEvent, BridgeError> {
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
    pub fn stop(&self, timeout: Duration) -> Result<(), BridgeError> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: без аргументов-указателей.
        map_rc(unsafe { ffi::nn_shutdown(ms) })
    }
}

impl Backend for GoBackend {
    fn submit(&self, id: u64, cmd: Command) -> Result<(), BackendError> {
        Ok(self.submit_raw(&map::command_to(id, cmd))?)
    }

    fn next_event(&self, timeout: Duration) -> Result<Next, BackendError> {
        loop {
            return Ok(match self.next_raw(timeout)? {
                NextEvent::Event(ev) => match map::event_from(ev) {
                    Some(e) => Next::Event(e),
                    None => continue, // пустое событие новой версии — пропускаем
                },
                NextEvent::Timeout => Next::Timeout,
                NextEvent::Stopped => Next::Stopped,
            });
        }
    }

    fn shutdown(&self, timeout: Duration) -> Result<(), BackendError> {
        Ok(self.stop(timeout)?)
    }
}

/// Поддельный сетевой слой для тестов домена и интерфейса: запоминает
/// команды, отдаёт заранее положенные события, может отвечать сценарием.
#[derive(Default)]
pub struct FakeBackend {
    state: Mutex<FakeState>,
    cv: Condvar,
}

type Script = Box<dyn FnMut(u64, &Command) -> Vec<Event> + Send>;

#[derive(Default)]
struct FakeState {
    commands: Vec<(u64, Command)>,
    events: VecDeque<Event>,
    stopped: bool,
    script: Option<Script>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Сценарий: на каждую команду возвращает события, которые встанут в очередь.
    pub fn with_script(f: impl FnMut(u64, &Command) -> Vec<Event> + Send + 'static) -> Self {
        let b = Self::default();
        b.state.lock().unwrap().script = Some(Box::new(f));
        b
    }

    /// Кладёт событие в очередь.
    pub fn push(&self, e: Event) {
        self.state.lock().unwrap().events.push_back(e);
        self.cv.notify_all();
    }

    /// Команды, полученные к этому моменту.
    pub fn commands(&self) -> Vec<(u64, Command)> {
        self.state.lock().unwrap().commands.clone()
    }
}

impl Backend for FakeBackend {
    fn submit(&self, id: u64, cmd: Command) -> Result<(), BackendError> {
        let mut st = self.state.lock().unwrap();
        if st.stopped {
            return Err(BackendError::NotRunning);
        }
        let evs = st.script.as_mut().map(|s| s(id, &cmd)).unwrap_or_default();
        st.commands.push((id, cmd));
        st.events.extend(evs);
        drop(st);
        self.cv.notify_all();
        Ok(())
    }

    fn next_event(&self, timeout: Duration) -> Result<Next, BackendError> {
        let st = self.state.lock().unwrap();
        let (mut st, _) = self.cv.wait_timeout_while(st, timeout, |s| s.events.is_empty() && !s.stopped).unwrap();
        if let Some(e) = st.events.pop_front() {
            return Ok(Next::Event(e));
        }
        Ok(if st.stopped { Next::Stopped } else { Next::Timeout })
    }

    fn shutdown(&self, _timeout: Duration) -> Result<(), BackendError> {
        self.state.lock().unwrap().stopped = true;
        self.cv.notify_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::model::{CommandResult, Payload};

    #[test]
    fn proto_types_roundtrip() {
        let cmd = map::command_to(1, Command::Ping("привет".into()));
        let back = proto::Command::decode(cmd.encode_to_vec().as_slice()).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn fake_backend_scripts_and_orders() {
        let b = FakeBackend::with_script(|id, _| {
            vec![Event::Result(CommandResult { cmd_id: id, error: None, payload: Payload::None })]
        });
        for i in 1..=30 {
            b.submit(i, Command::RequestQr).unwrap();
        }
        for i in 1..=30 {
            match b.next_event(Duration::from_millis(10)).unwrap() {
                Next::Event(Event::Result(r)) => assert_eq!(r.cmd_id, i),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(b.next_event(Duration::from_millis(5)).unwrap(), Next::Timeout);
        b.shutdown(Duration::ZERO).unwrap();
        assert_eq!(b.next_event(Duration::from_millis(5)).unwrap(), Next::Stopped);
        assert_eq!(b.submit(99, Command::LogOut), Err(BackendError::NotRunning));
        assert_eq!(b.commands().len(), 30);
    }
}
