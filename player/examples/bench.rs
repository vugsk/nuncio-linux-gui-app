//! Проверка плеера без интерфейса (В8): кадры забираются с частотой 60 Гц,
//! как это делает окно просмотра; меряются отставание показанного кадра от
//! часов, расхождение часов с реальным временем, пауза, перемотка, память.
//! Звук — с громкостью 0 (устройство открывается, слышно ничего нет).
//!
//!   cargo run --release -p player --example bench -- файл [секунд] [--controls]

use std::time::{Duration, Instant};

fn pss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Pss:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("файл");
    let limit: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20.0);
    let controls = std::env::args().any(|a| a == "--controls");
    let before = pss_mb();
    let mw: u32 = std::env::var("BENCH_MAX_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1280);
    let p = player::Player::open(std::path::Path::new(&path), mw, mw * 5 / 8).expect("открытие");
    p.set_volume(0.0);
    let info = p.info();
    println!("файл {path}: {info:?}; PSS до {before:.1} МБ");
    let start = Instant::now();
    let (mut shown, mut lag_sum, mut lag_max) = (0u64, 0.0f64, 0.0f64);
    let mut peak = 0.0f64;
    let mut paused_at = None;
    let mut checks = Vec::new();
    while start.elapsed().as_secs_f64() < limit && !p.ended() {
        let t = start.elapsed().as_secs_f64();
        if controls {
            if (5.0..5.02).contains(&t) && paused_at.is_none() {
                p.set_paused(true);
                paused_at = Some(p.position());
            }
            if (6.0..6.02).contains(&t) {
                if let Some(at) = paused_at {
                    let moved = p.position() - at;
                    checks.push(format!("пауза 1 с: часы сдвинулись на {moved:.3} с"));
                }
                p.set_paused(false);
                p.seek(2.0);
            }
            if (6.5..6.52).contains(&t) {
                checks.push(format!("перемотка на 2,0 с: через 0,5 с позиция {:.2} с", p.position()));
            }
        }
        if let Some(f) = p.take_frame() {
            let lag = p.position() - f.pts;
            lag_sum += lag.abs();
            lag_max = lag_max.max(lag.abs());
            shown += 1;
            p.recycle(f);
        }
        if shown % 30 == 0 {
            peak = peak.max(pss_mb());
        }
        std::thread::sleep(Duration::from_micros(16_667));
    }
    let wall = start.elapsed().as_secs_f64();
    let pos = p.position();
    let during = pss_mb();
    drop(p);
    let dropped = pss_mb();
    player::trim_memory();
    let trimmed = pss_mb();
    if info.has_video {
        println!("показано кадров {shown} за {wall:.1} с ({:.1}/с); отставание кадра от часов: среднее {:.1} мс, макс {:.1} мс",
            shown as f64 / wall, lag_sum / shown.max(1) as f64 * 1000.0, lag_max * 1000.0);
    }
    if !controls {
        println!("часы {pos:.2} с против реального времени {wall:.2} с: расхождение {:+.0} мс", (pos - wall) * 1000.0);
    }
    for c in checks {
        println!("{c}");
    }
    println!("PSS: во время {during:.1} (пик {peak:.1}), после закрытия {dropped:.1}, после malloc_trim {trimmed:.1} МБ");
}
