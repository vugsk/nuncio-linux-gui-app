//! Журнал в `~/.local/state/nuncio/nuncio.log` (Архитектура/nuncio - Приложение
//! (Rust и Slint).md, раздел 7). Сюда не попадают тексты сообщений, номера,
//! коды и пароли — только события и счётчики. Файл больше 1 МБ переименовывается
//! в `nuncio.log.1` при старте.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct FileLogger {
    file: Mutex<Option<File>>,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::max_level()
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let line = format!("{}.{:03} {} {}: {}\n", t.as_secs(), t.subsec_millis(), r.level(), r.target(), r.args());
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
        if std::env::var_os("NUNCIO_LOG_STDERR").is_some() {
            eprint!("{line}");
        }
    }

    fn flush(&self) {
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = f.flush();
        }
    }
}

pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("nuncio.log");
    if std::fs::metadata(&path).map(|m| m.len() > 1 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("nuncio.log.1"));
    }
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(&path).ok();
    let logger = Box::leak(Box::new(FileLogger { file: Mutex::new(file) }));
    if log::set_logger(logger).is_ok() {
        // Отладка прокрутки и «прочитано» (номера сообщений, без текста).
        let debug = std::env::var_os("NUNCIO_DEBUG_SCROLL").is_some();
        log::set_max_level(if debug { log::LevelFilter::Debug } else { log::LevelFilter::Info });
    }
}
