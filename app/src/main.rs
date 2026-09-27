//! Точка входа. Поток UI (Slint) остаётся главным; сетевой слой (Go)
//! запускается из отдельного потока-насоса, который забирает события
//! через `next_event` и передаёт их в UI через `invoke_from_event_loop`
//! (Архитектура/nuncio - Приложение (Rust и Slint).md, раздел 2).
//!
//! Веха В0: насос отправляет Ping и показывает версию ядра, полученную
//! по цепочке Rust -> Go -> ядро C++ -> Go -> Rust.

use std::sync::mpsc;
use std::time::Duration;

use bridge::proto::{self, command, command_result, event};
use bridge::{GoBackend, NextEvent};
use slint::ComponentHandle;

enum Control {
    Stop,
}

fn set_status(ui: &slint::Weak<ui::App>, text: String, ok: bool) {
    let ui = ui.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui.upgrade() {
            ui.set_status(text.into());
            ui.set_ok(ok);
        }
    });
}

fn pump(ui: slint::Weak<ui::App>, control: mpsc::Receiver<Control>) {
    let backend = match GoBackend::start() {
        Ok(b) => b,
        Err(e) => return set_status(&ui, format!("Сетевой слой не запустился: {e}"), false),
    };
    let ping = proto::Command {
        id: 1,
        account_id: 1,
        body: Some(command::Body::Ping(proto::Ping { message: "проверка связки".into() })),
    };
    if let Err(e) = backend.submit(&ping) {
        set_status(&ui, format!("Команда не отправлена: {e}"), false);
    }
    loop {
        if let Ok(Control::Stop) = control.try_recv() {
            let _ = backend.shutdown(Duration::from_secs(2));
            return;
        }
        match backend.next_event(Duration::from_millis(200)) {
            Ok(NextEvent::Event(proto::Event { body: Some(event::Body::Result(r)) })) => {
                match (r.error, r.payload) {
                    (None, Some(command_result::Payload::Pong(p))) => set_status(
                        &ui,
                        format!("Связка Rust → Go → ядро работает: {}", p.core_version),
                        true,
                    ),
                    (Some(e), _) => set_status(&ui, format!("Ошибка команды: {}", e.text), false),
                    _ => {}
                }
            }
            Ok(NextEvent::Event(proto::Event { body: Some(event::Body::Fatal(f)) })) => {
                set_status(&ui, format!("Сбой сетевого слоя: {}", f.text), false);
            }
            Ok(NextEvent::Stopped) => return,
            Ok(_) => {}
            Err(e) => set_status(&ui, format!("Ошибка стыка: {e}"), false),
        }
    }
}

fn main() {
    let app = ui::App::new().expect("не удалось создать окно (нужен доступный бэкенд Slint)");
    let (tx, rx) = mpsc::channel();
    let weak = app.as_weak();
    let pump = std::thread::Builder::new()
        .name("nuncio-pump".into())
        .spawn(move || pump(weak, rx))
        .expect("не удалось запустить поток-насос");

    app.run().expect("окно завершилось с ошибкой");

    // Закрытие: остановить сетевой слой и дождаться насоса.
    let _ = tx.send(Control::Stop);
    let _ = pump.join();
}
