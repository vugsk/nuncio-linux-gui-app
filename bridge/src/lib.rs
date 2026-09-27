//! Стык Rust ↔ Go. Каркас (веха В0): только сгенерированные типы `client.proto`
//! и обёртки-заглушки `Backend`. Насос `nn_next_event`, реальный `GoBackend`
//! с FFI к `libnuncio_net.a` и разбор событий появляются на вехе В3
//! (см. Архитектура/nuncio - План реализации.md).

pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/nuncio.client.v0.rs"));
}

use domain::Backend;

/// Заглушка на месте будущего `GoBackend` (FFI к `net`, c-archive).
/// Пока не звонит никуда: методы появятся вместе со стыком `nn_*`
/// (Архитектура/nuncio - Границы и C ABI.md).
#[derive(Default)]
pub struct GoBackend;

impl Backend for GoBackend {}

/// Заглушка на месте будущего `FakeBackend` для тестов домена без Go и ядра.
#[derive(Default)]
pub struct FakeBackend;

impl Backend for FakeBackend {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong_types_generated() {
        // Смок-тест: типы из client.proto действительно сгенерированы и годны.
        let ping = proto::Ping { message: "привет".into() };
        assert_eq!(ping.message, "привет");
    }

    #[test]
    fn backends_are_constructible() {
        let _ = GoBackend;
        let _ = FakeBackend;
    }
}
