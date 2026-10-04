//! Утечка плеера: открыть и закрыть один файл несколько раз в одном процессе.
//!   cargo run --release -p player --example repeat -- файл [раз]
fn anon_mb() -> f64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Pss_Anon:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn main() {
    let path = std::env::args().nth(1).expect("файл");
    let runs: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(5);
    for i in 1..=runs {
        let p = player::Player::open(std::path::Path::new(&path), 1280, 720).expect("открытие");
        p.set_volume(0.0);
        let start = std::time::Instant::now();
        while start.elapsed().as_secs_f64() < 2.0 {
            if let Some(f) = p.take_frame().filter(|_| std::env::var_os("NOTAKE").is_none()) {
                p.recycle(f);
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        drop(p);
        player::trim_memory();
        println!("после {i}-го: анонимная {:.1} МБ", anon_mb());
    }
}
