//! Точка входа. Каркас (веха В0): открывает пустое окно из `ui`, ничего
//! не вызывает в `bridge`/`domain` — это следующий шаг той же вехи
//! («вертикаль-заглушка», см. Архитектура/nuncio - План реализации.md).

use slint::ComponentHandle;

fn main() {
    let _fake = bridge::FakeBackend;

    let app = ui::App::new().expect("не удалось создать окно (нужен доступный бэкенд Slint)");
    app.run().expect("окно завершилось с ошибкой");
}
