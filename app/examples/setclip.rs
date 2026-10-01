//! Для сквозных проверок: положить картинку (JPEG/PNG) в буфер обмена X11 и
//! держать его, пока процесс жив (`tests/mediatest.sh`, вставка Ctrl+V).
//!
//!   DISPLAY=:99 cargo run -p app --example setclip -- картинка.png
use arboard::SetExtLinux;

fn main() {
    let path = std::env::args().nth(1).expect("путь к картинке");
    let data = std::fs::read(&path).expect("картинка не читается");
    let (w, h, rgb) = ui::photo::decode(&data).expect("не JPEG/PNG");
    let rgba: Vec<u8> = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let img = arboard::ImageData { width: w, height: h, bytes: rgba.into() };
    let mut cb = arboard::Clipboard::new().expect("буфер обмена недоступен");
    println!("в буфере");
    cb.set().wait().image(img).expect("картинка не положена");
}
