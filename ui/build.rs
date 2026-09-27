// 1. Компилирует .slint с переводами (lang/<язык>/LC_MESSAGES/ui.po, Вх-8).
// 2. Извлекает картинки флагов стран из Noto Color Emoji (О-14, target/native,
//    `just deps noto-emoji`): шейпинг последовательности региональных
//    индикаторов (harfrust), растровый глиф CBDT (skrifa), уменьшение до 48 px,
//    PNG в OUT_DIR. Оба крейта — fontations/HarfBuzz, поддерживаются
//    (rustybuzz и ttf-parser объявлены заброшенными, RUSTSEC-2026-0206/0192).
use std::fmt::Write as _;
use std::io::Cursor;
use std::path::PathBuf;

const FLAG_BOX: u32 = 48; // 2× для 24 px в интерфейсе

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("src/app.slint", config).expect("не удалось скомпилировать src/app.slint");
    println!("cargo:rerun-if-changed=lang");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let font_path = manifest.join("../target/native/NotoColorEmoji.ttf");
    println!("cargo:rerun-if-changed={}", font_path.display());
    let data = std::fs::read(&font_path).unwrap_or_else(|_| {
        panic!("нет {}: выполните `just deps noto-emoji` в корне репозитория", font_path.display())
    });
    let hb_font = harfrust::FontRef::new(&data).expect("NotoColorEmoji.ttf не разобран");
    let hb_data = harfrust::ShaperData::new(&hb_font);
    let shaper = hb_data.shaper(&hb_font).build();
    let sk_font = skrifa::FontRef::new(&data).expect("NotoColorEmoji.ttf не разобран");
    let strikes = skrifa::bitmap::BitmapStrikes::new(&sk_font);
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // На любую пару букв шрифт даёт глиф; несуществующие коды — общий глиф
    // «неизвестный флаг». Его номер берётся по заведомо свободным кодам.
    let unknown: Vec<u32> = ["ZZ", "QQ", "XX"].iter().filter_map(|iso| flag_glyph(&shaper, iso)).collect();
    let mut table = String::from("pub static FLAGS: &[(&str, &[u8])] = &[\n");
    let mut n = 0;
    for a in b'A'..=b'Z' {
        for b in b'A'..=b'Z' {
            let iso = format!("{}{}", a as char, b as char);
            let Some(gid) = flag_glyph(&shaper, &iso) else { continue };
            if unknown.contains(&gid) {
                continue;
            }
            let Some(png) = flag_png(&strikes, gid) else { continue };
            let file = out.join(format!("flag_{iso}.png"));
            std::fs::write(&file, png).unwrap();
            writeln!(table, "    (\"{iso}\", include_bytes!(\"flag_{iso}.png\")),").unwrap();
            n += 1;
        }
    }
    table.push_str("];\n");
    assert!(!unknown.is_empty(), "не найден глиф «неизвестный флаг»");
    assert!((200..400).contains(&n), "из шрифта извлечено {n} флагов (ожидалось около 250)");
    std::fs::write(out.join("flags.rs"), table).unwrap();
}

/// Глиф флага: последовательность региональных индикаторов сливается в один глиф.
fn flag_glyph(shaper: &harfrust::Shaper, iso: &str) -> Option<u32> {
    let seq: String = iso.chars().map(|c| char::from_u32(0x1F1E6 + (c as u32 - 'A' as u32)).unwrap()).collect();
    let mut buf = harfrust::UnicodeBuffer::new();
    buf.push_str(&seq);
    buf.guess_segment_properties();
    let shaped = shaper.shape(buf, harfrust::ShapeOptions::new());
    let infos = shaped.glyph_infos();
    if infos.len() != 1 || infos[0].glyph_id == 0 {
        return None; // шрифт не знает такого флага
    }
    Some(infos[0].glyph_id)
}

fn flag_png(strikes: &skrifa::bitmap::BitmapStrikes, gid: u32) -> Option<Vec<u8>> {
    let glyph = strikes.glyph_for_size(skrifa::instance::Size::unscaled(), skrifa::GlyphId::new(gid))?;
    let skrifa::bitmap::BitmapData::Png(data) = glyph.data else { return None };
    let (w, h, rgba) = decode_rgba(data)?;
    let (tw, th, small) = downscale(w, h, &rgba, FLAG_BOX);
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, tw, th);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::High);
        enc.write_header().ok()?.write_image_data(&small).ok()?;
    }
    Some(png)
}

fn decode_rgba(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None, // EXPAND превращает палитру в RGB(A)
    };
    Some((info.width, info.height, rgba))
}

/// Уменьшение усреднением по площади с учётом прозрачности; вписывается в квадрат `side`.
fn downscale(w: u32, h: u32, src: &[u8], side: u32) -> (u32, u32, Vec<u8>) {
    let scale = f64::from(side) / f64::from(w.max(h));
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    let th = ((f64::from(h) * scale).round() as u32).max(1);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for ty in 0..th {
        let y0 = f64::from(ty) / scale;
        let y1 = f64::from(ty + 1) / scale;
        for tx in 0..tw {
            let x0 = f64::from(tx) / scale;
            let x1 = f64::from(tx + 1) / scale;
            let mut acc = [0f64; 4];
            let mut area = 0f64;
            let mut sy = y0.floor() as u32;
            while f64::from(sy) < y1 && sy < h {
                let fy = (f64::from(sy + 1).min(y1) - f64::from(sy).max(y0)).max(0.0);
                let mut sx = x0.floor() as u32;
                while f64::from(sx) < x1 && sx < w {
                    let fx = (f64::from(sx + 1).min(x1) - f64::from(sx).max(x0)).max(0.0);
                    let wgt = fx * fy;
                    let i = ((sy * w + sx) * 4) as usize;
                    let a = f64::from(src[i + 3]) / 255.0;
                    for c in 0..3 {
                        acc[c] += f64::from(src[i + c]) * a * wgt; // с предумножением
                    }
                    acc[3] += a * wgt;
                    area += wgt;
                    sx += 1;
                }
                sy += 1;
            }
            let o = ((ty * tw + tx) * 4) as usize;
            if acc[3] > 0.0 {
                for c in 0..3 {
                    out[o + c] = (acc[c] / acc[3]).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[o + 3] = (acc[3] / area.max(1e-9) * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    (tw, th, out)
}
