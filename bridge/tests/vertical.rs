//! Вертикаль Rust -> Go (c-archive) -> ядро C++ -> Go -> Rust.
//! Отдельный файл = отдельный процесс, поэтому Go-рантайм стартует здесь
//! ровно один раз; все проверки — в одном тесте.

use std::time::Duration;

use bridge::proto::{self, command, command_result, event};
use bridge::{BridgeError, GoBackend, NextEvent, StartParams};
use domain::model::{Command, ErrorKind, Event};
use domain::{Backend, Next};

fn ping(id: u64, text: &str) -> proto::Command {
    proto::Command {
        id,
        account_id: 1,
        body: Some(command::Body::Ping(proto::Ping { message: text.into() })),
    }
}

fn next_result(backend: &GoBackend) -> proto::CommandResult {
    match backend.next_raw(Duration::from_secs(5)).expect("next_event") {
        NextEvent::Event(proto::Event { body: Some(event::Body::Result(r)) }) => r,
        other => panic!("ожидался CommandResult, получено {other:?}"),
    }
}

#[test]
fn rust_go_core_roundtrip() {
    // Без api_id сеть не поднимается, но стык и ядро работают.
    let backend = GoBackend::start(&StartParams::default()).expect("старт сетевого слоя");
    assert_eq!(GoBackend::start(&StartParams::default()).err(), Some(BridgeError::AlreadyStarted));

    // Пустая очередь: таймаут, а не зависание.
    assert!(matches!(backend.next_raw(Duration::from_millis(20)).unwrap(), NextEvent::Timeout));

    // Порядок ответов сохраняется на серии команд.
    for id in 1..=50u64 {
        backend.submit_raw(&ping(id, &format!("ping {id}"))).unwrap();
    }
    for id in 1..=50u64 {
        let r = next_result(&backend);
        assert_eq!(r.cmd_id, id);
        assert!(r.error.is_none(), "ошибка: {:?}", r.error);
        match r.payload {
            Some(command_result::Payload::Pong(p)) => {
                assert_eq!(p.message, format!("ping {id}"));
                assert!(p.core_version.starts_with("tgcrypto "), "версия ядра: {}", p.core_version);
            }
            other => panic!("ожидался Pong, получено {other:?}"),
        }
    }

    // Неизвестная команда даёт ошибку INTERNAL, а не падение.
    backend.submit_raw(&proto::Command { id: 99, account_id: 1, body: None }).unwrap();
    let r = next_result(&backend);
    assert_eq!(r.cmd_id, 99);
    assert_eq!(r.error.unwrap().kind, proto::error::Kind::Internal as i32);

    // Через трейт домена: команда входа без api_id — понятная ошибка.
    Backend::submit(&backend, 100, Command::SendCode { phone: "+1".into() }).unwrap();
    match Backend::next_event(&backend, Duration::from_secs(5)).unwrap() {
        Next::Event(Event::Result(r)) => {
            assert_eq!(r.cmd_id, 100);
            let e = r.error.expect("ошибка");
            assert_eq!(e.kind, ErrorKind::Internal);
            assert!(e.text.contains("api_id"), "{}", e.text);
        }
        other => panic!("{other:?}"),
    }

    Backend::shutdown(&backend, Duration::from_secs(2)).expect("остановка");
    assert!(matches!(backend.next_raw(Duration::from_millis(100)).unwrap(), NextEvent::Stopped));
    assert_eq!(backend.submit_raw(&ping(101, "после остановки")), Err(BridgeError::NotRunning));
}
