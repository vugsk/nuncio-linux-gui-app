//! `.slint`-разметка и view-model'и. Каркас (веха В0): одно пустое окно,
//! чтобы конвейер `slint-build` был реальным. Настоящие экраны — по
//! заметкам в Дизайн/ (карта в nuncio - Карта проекта.md), появляются
//! вместе с вехами В3–В9.

slint::include_modules!();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_is_constructible() {
        // Требует доступного бэкенда Slint (winit); на CI без дисплея
        // используйте Xvfb или cage, как в проверках П-1…П-5.
        let _ = App::new();
    }
}
