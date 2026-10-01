//! Фото в ленте и окне просмотра (Чт-11, Св-3): уменьшение усреднением,
//! скруглённые углы маской в Rust (Slint в software-рендерере картинку по
//! скруглению не обрезает), размытое мини-превью Telegram до скачивания.
//! Работает вне потока UI: наружу — `SharedPixelBuffer`.

use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::avatar::decode_jpeg;

/// Радиус скругления фото в пузыре (как у пузыря).
pub const RADIUS: f32 = 10.0;

/// JPEG или PNG → RGB.
pub fn decode(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        return decode_png_rgb(data);
    }
    decode_jpeg(data)
}

/// PNG любого вида (палитра, серый, с прозрачностью, 16 бит) → RGB;
/// прозрачное — на тёмном фоне ленты.
fn decode_png_rgb(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width as usize, info.height as usize);
    const BG: [u16; 3] = [0x18, 0x25, 0x33];
    let blend = |c: u8, a: u8, bg: u16| ((u16::from(c) * u16::from(a) + bg * (255 - u16::from(a))) / 255) as u8;
    let rgb: Vec<u8> = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf.chunks_exact(4).flat_map(|p| [0, 1, 2].map(|i| blend(p[i], p[3], BG[i]))).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [0, 1, 2].map(|i| blend(p[0], p[1], BG[i]))).collect(),
        png::ColorType::Indexed => return None, // EXPAND превращает палитру в RGB(A)
    };
    (rgb.len() == w * h * 3).then_some((w, h, rgb))
}

/// Уменьшение до `tw`×`th` (заполнение с обрезкой по центру) усреднением;
/// увеличение — ближайшим с сглаживанием (для мини-превью).
fn resample(w: usize, h: usize, rgb: &[u8], tw: usize, th: usize) -> Vec<[u8; 3]> {
    // Кадрирование под пропорции цели (как object-fit: cover).
    let (sw, sh) = if w * th > h * tw { (h * tw / th, h) } else { (w, w * th / tw) };
    let (sw, sh) = (sw.max(1), sh.max(1));
    let (ox, oy) = ((w - sw) / 2, (h - sh) / 2);
    let sx = sw as f32 / tw as f32;
    let sy = sh as f32 / th as f32;
    let mut out = Vec::with_capacity(tw * th);
    for ty in 0..th {
        for tx in 0..tw {
            if sx <= 1.0 || sy <= 1.0 {
                // Увеличение: билинейно.
                let fx = ((tx as f32 + 0.5) * sx - 0.5).clamp(0.0, (sw - 1) as f32);
                let fy = ((ty as f32 + 0.5) * sy - 0.5).clamp(0.0, (sh - 1) as f32);
                let (x0, y0) = (fx as usize, fy as usize);
                let (x1, y1) = ((x0 + 1).min(sw - 1), (y0 + 1).min(sh - 1));
                let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
                let px = |x: usize, y: usize, c: usize| f32::from(rgb[((oy + y) * w + ox + x) * 3 + c]);
                let mut p = [0u8; 3];
                for (c, v) in p.iter_mut().enumerate() {
                    let top = px(x0, y0, c) * (1.0 - ax) + px(x1, y0, c) * ax;
                    let bot = px(x0, y1, c) * (1.0 - ax) + px(x1, y1, c) * ax;
                    *v = (top * (1.0 - ay) + bot * ay).round() as u8;
                }
                out.push(p);
                continue;
            }
            let x0 = ox + (tx as f32 * sx) as usize;
            let y0 = oy + (ty as f32 * sy) as usize;
            let x1 = (ox + ((tx + 1) as f32 * sx).ceil() as usize).min(ox + sw).max(x0 + 1);
            let y1 = (oy + ((ty + 1) as f32 * sy).ceil() as usize).min(oy + sh).max(y0 + 1);
            let (mut acc, mut n) = ([0u32; 3], 0u32);
            for y in y0..y1 {
                let row = &rgb[(y * w + x0) * 3..(y * w + x1) * 3];
                for c in row.chunks_exact(3) {
                    acc[0] += u32::from(c[0]);
                    acc[1] += u32::from(c[1]);
                    acc[2] += u32::from(c[2]);
                    n += 1;
                }
            }
            out.push([(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]);
        }
    }
    out
}

/// Покрытие пикселя скруглённым прямоугольником (4×4 подвыборки у углов).
fn coverage(x: usize, y: usize, w: usize, h: usize, r: f32) -> u8 {
    let (fx, fy) = (x as f32, y as f32);
    let near = |v: f32, size: usize| v < r || v > size as f32 - r - 1.0;
    if !(near(fx, w) && near(fy, h)) {
        return 255;
    }
    let mut inside = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            let px = fx + (sx as f32 + 0.5) / 4.0;
            let py = fy + (sy as f32 + 0.5) / 4.0;
            let cx = px.clamp(r, w as f32 - r);
            let cy = py.clamp(r, h as f32 - r);
            if (px - cx).powi(2) + (py - cy).powi(2) <= r * r {
                inside += 1;
            }
        }
    }
    (inside * 255 / 16) as u8
}

/// RGB-картинка → `tw`×`th` со скруглёнными углами.
pub fn rounded(w: usize, h: usize, rgb: &[u8], tw: u32, th: u32, radius: f32) -> SharedPixelBuffer<Rgba8Pixel> {
    let (tw_, th_) = (tw.max(1) as usize, th.max(1) as usize);
    let px = resample(w, h, rgb, tw_, th_);
    let mut out = SharedPixelBuffer::<Rgba8Pixel>::new(tw_ as u32, th_ as u32);
    let buf = out.make_mut_slice();
    for y in 0..th_ {
        for x in 0..tw_ {
            let [r, g, b] = px[y * tw_ + x];
            buf[y * tw_ + x] = Rgba8Pixel { r, g, b, a: coverage(x, y, tw_, th_, radius) };
        }
    }
    out
}

/// Файл фото → картинка для ленты.
pub fn load_feed(path: &str, tw: u32, th: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let data = std::fs::read(path).ok()?;
    let (w, h, rgb) = decode(&data)?;
    Some(rounded(w, h, &rgb, tw, th, RADIUS))
}

/// Мини-превью (JPEG ~40 px) → размытая картинка размера для ленты.
pub fn stripped(jpeg: &[u8], tw: u32, th: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let (w, h, rgb) = decode_jpeg(jpeg)?;
    Some(rounded(w, h, &rgb, tw, th, RADIUS))
}

/// Файл фото → для окна просмотра: вписать в `max_w`×`max_h`, не увеличивая.
pub fn load_full(path: &str, max_w: u32, max_h: u32) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let data = std::fs::read(path).ok()?;
    let (w, h, rgb) = decode(&data)?;
    let k = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    let (tw, th) = (((w as f32) * k).round().max(1.0) as u32, ((h as f32) * k).round().max(1.0) as u32);
    Some(rounded(w, h, &rgb, tw, th, 0.0))
}

/// RGBA в PNG-файл (картинка из буфера обмена для отправки).
pub fn save_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(f, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(std::io::Error::other)?;
    wr.write_image_data(rgba).map_err(std::io::Error::other)
}

/// Стороны картинки по заголовку (JPEG — маркер SOF, PNG — IHDR), без раскодирования.
pub fn image_size(path: &str) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut head = Vec::with_capacity(64 * 1024);
    std::fs::File::open(path).ok()?.take(256 * 1024).read_to_end(&mut head).ok()?;
    header_size(&head)
}

fn header_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) && b.len() >= 24 {
        let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        return Some((be(16), be(20)));
    }
    if !b.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xff {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        if marker == 0xff {
            i += 1;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        // SOF0…SOF15, кроме DHT (C4), JPG (C8) и DAC (CC).
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let h = u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]]));
            let w = u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]]));
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_rgb_and_gray_decode() {
        let enc = |color, bytes: &[u8], w: u32, h: u32| {
            let mut out = Vec::new();
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(color);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(bytes).unwrap();
            out
        };
        let rgb = enc(png::ColorType::Rgb, &[10, 20, 30].repeat(6), 3, 2);
        assert_eq!(decode(&rgb), Some((3, 2, [10, 20, 30].repeat(6))));
        let gray = enc(png::ColorType::Grayscale, &[7; 4], 2, 2);
        assert_eq!(decode(&gray).map(|d| d.2), Some(vec![7; 12]));
        let rgba = enc(png::ColorType::Rgba, &[255, 255, 255, 255, 0, 0, 0, 0], 2, 1);
        assert_eq!(decode(&rgba).map(|d| d.2), Some(vec![255, 255, 255, 0x18, 0x25, 0x33]), "прозрачное — фон ленты");
    }

    #[test]
    fn header_sizes() {
        let mut png = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        png.extend(640u32.to_be_bytes());
        png.extend(480u32.to_be_bytes());
        assert_eq!(header_size(&png), Some((640, 480)));
        // JPEG: APP0 (длина 16), затем SOF0 с высотой 300 и шириной 400.
        let mut jpg = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
        jpg.extend([0u8; 14]);
        jpg.extend([0xff, 0xc0, 0, 17, 8, 0x01, 0x2c, 0x01, 0x90, 3]);
        jpg.extend([0u8; 9]);
        assert_eq!(header_size(&jpg), Some((400, 300)));
        assert_eq!(header_size(b"GIF89a"), None);
    }

    #[test]
    fn rounded_corners_and_size() {
        let (w, h) = (100, 50);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 50, 200]).collect();
        let p = rounded(w, h, &rgb, 40, 20, 8.0);
        assert_eq!((p.width(), p.height()), (40, 20));
        let s = p.as_slice();
        assert_eq!(s[0].a, 0, "угол прозрачный");
        assert_eq!(s[10 * 40 + 20].a, 255, "середина непрозрачная");
        assert!(s[39].a < 64 && s[20].a == 255);
        // Увеличение (мини-превью): без паники, цвета в пределах.
        let small: Vec<u8> = [10u8, 20, 30].repeat(4);
        let big = rounded(2, 2, &small, 64, 64, 0.0);
        assert!(big.as_slice().iter().all(|p| p.r == 10 && p.g == 20 && p.b == 30 && p.a == 255));
    }
}
