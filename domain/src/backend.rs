//! Трейт `Backend`: всё, что домен требует от сетевого слоя. Реализации —
//! `bridge::GoBackend` (Go через c-archive) и `bridge::FakeBackend` (тесты).

use std::time::Duration;

use crate::model::{Command, Event};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    NotRunning,
    QueueFull,
    Other(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => write!(f, "сетевой слой не запущен"),
            Self::QueueFull => write!(f, "очередь команд переполнена"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// Исход ожидания события. Живёт одно мгновение (насос сразу раскладывает
/// событие по каналам), поэтому разница размеров вариантов не важна.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    Event(Event),
    Timeout,
    Stopped,
}

pub trait Backend: Send + Sync {
    /// Ставит команду в очередь; не блокирует. `id` вернётся в `CommandResult::cmd_id`.
    fn submit(&self, id: u64, cmd: Command) -> Result<(), BackendError>;
    /// Ждёт событие не дольше `timeout` (вызывается из потока-насоса).
    fn next_event(&self, timeout: Duration) -> Result<Next, BackendError>;
    /// Останавливает сетевой слой.
    fn shutdown(&self, timeout: Duration) -> Result<(), BackendError>;
}
