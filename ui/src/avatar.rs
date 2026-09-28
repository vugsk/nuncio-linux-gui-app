//! Аватары: круглая маска в Rust (Slint в software-рендерере картинку по кругу
//! не обрезает, Дизайн - Список диалогов, раздел 5), заглушка — инициалы и
//! цвет по id. Декодирование и маска выполняются вне потока UI: наружу идёт
//! `SharedPixelBuffer` (его можно передавать между потоками).

use slint::{Rgba8Pixel, SharedPixelBuffer};
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Цвета заглушек (приглушённые, под тёмную тему; первый — из макета).
const PALETTE: [(u8, u8, u8); 7] = [
    (0x3a, 0x6b, 0x8a),
    (0x8a, 0x5a, 0x2b),
    (0x3a, 0x6b, 0x4a),
    (0x6b, 0x4a, 0x8a),
    (0x8a, 0x3a, 0x4f),
    (0x2f, 0x6f, 0xbd),
    (0x6b, 0x6b, 0x3a),
];

/// Цвет заглушки по id собеседника.
pub fn color_for(id: i64) -> (u8, u8, u8) {
    PALETTE[(id.unsigned_abs() % PALETTE.len() as u64) as usize]
}

/// Инициалы: первые буквы двух первых слов («Рабочая группа» → «РГ»).
pub fn initials(name: &str) -> String {
    let mut out: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// JPEG → RGB.
pub fn decode_jpeg(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(data), opts);
    let px = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    (px.len() == w * h * 3).then_some((w, h, px))
}

/// Круглый аватар стороны `side` px из RGB-картинки: центральный квадрат,
/// уменьшение усреднением, край сглажен (4×4 подвыборки на пиксель края).
pub fn circle(w: usize, h: usize, rgb: &[u8], side: u32) -> SharedPixelBuffer<Rgba8Pixel> {
    let s = w.min(h);
    let (ox, oy) = ((w - s) / 2, (h - s) / 2);
    let n = side as usize;
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(side, side);
    let buf = out.make_mut_slice();
    let scale = s as f32 / n as f32;
    let r = n as f32 / 2.0;
    for ty in 0..n {
        for tx in 0..n {
            // Цвет: среднее по области источника.
            let x0 = ox + (tx as f32 * scale) as usize;
            let y0 = oy + (ty as f32 * scale) as usize;
            let x1 = (ox + ((tx + 1) as f32 * scale).ceil() as usize).min(ox + s).max(x0 + 1);
            let y1 = (oy + ((ty + 1) as f32 * scale).ceil() as usize).min(oy + s).max(y0 + 1);
            let (mut acc, mut cnt) = ([0u32; 3], 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 3;
                    for c in 0..3 {
                        acc[c] += u32::from(rgb[i + c]);
                    }
                    cnt += 1;
                }
            }
            // Покрытие кругом.
            let mut inside = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = tx as f32 + (sx as f32 + 0.5) / 4.0 - r;
                    let dy = ty as f32 + (sy as f32 + 0.5) / 4.0 - r;
                    if dx * dx + dy * dy <= r * r {
                        inside += 1;
                    }
                }
            }
            let a = (inside * 255 / 16) as u8;
            buf[ty * n + tx] = Rgba8Pixel {
                r: (acc[0] / cnt) as u8,
                g: (acc[1] / cnt) as u8,
                b: (acc[2] / cnt) as u8,
                a,
            };
        }
    }
    out
}

/// Файл аватара → круглая картинка (None — не JPEG или повреждён).
pub fn load(path: &str, side: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let data = std::fs::read(path).ok()?;
    let (w, h, rgb) = decode_jpeg(&data)?;
    Some(circle(w, h, &rgb, side))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_colors() {
        assert_eq!(initials("Рабочая группа"), "РГ");
        assert_eq!(initials("мама"), "М");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("😀 Party"), "P");
        assert_eq!(color_for(5), color_for(-5));
    }

    #[test]
    fn circle_mask_is_round_and_smooth() {
        let (w, h) = (120, 80);
        let rgb = vec![200u8; w * h * 3];
        let px = circle(w, h, &rgb, 48);
        let at = |x: usize, y: usize| px.as_slice()[y * 48 + x];
        assert_eq!(at(24, 24).a, 255, "центр непрозрачный");
        assert_eq!(at(0, 0).a, 0, "угол прозрачный");
        assert_eq!(at(24, 24).r, 200);
        // На краю есть полупрозрачные пиксели (сглаживание).
        let edge = (0..48).map(|x| at(x, 2).a).filter(|&a| a > 0 && a < 255).count();
        assert!(edge > 0);
    }
}
