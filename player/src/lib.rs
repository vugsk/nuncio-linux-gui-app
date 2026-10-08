//! Воспроизведение видео и звука внутри приложения (В8, Св-2, проверка П-6):
//! свой минимальный статический FFmpeg (декодеры H.264, HEVC, VP8/9, MPEG-4,
//! AAC, Opus, Vorbis, MP3, FLAC) и вывод звука через `cpal`.
//!
//! Поток декодирования читает пакеты и раскладывает их в две очереди с
//! ограничением: кадры видео в RGBA не больше заданного размера (П-6: кадр
//! масштабируется на декодере до размера окна) и звук в f32 на частоте
//! устройства. Часы — по звуку: сколько сэмплов реально забрало устройство;
//! без звука — по системному времени. Интерфейс забирает кадр, чьё время
//! наступило (`take_frame`), опоздавшие отбрасываются — синхронизация
//! не расходится, сколько бы ни длился ролик. Буферы кадров переиспользуются.
//!
//! Без Slint: тип кадра свой, интерфейс копирует его в картинку сам.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;

/// Кадр видео: RGBA без отступов строк, время показа в секундах.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub pts: f64,
}

/// Что известно о файле после открытия.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Info {
    /// Длительность, с (0 — неизвестна).
    pub duration: f64,
    /// Размер кадра на выходе (после вписывания).
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
    pub has_audio: bool,
}

/// Кадров видео вперёд (П-6: память определяется числом кадров; 720p RGBA —
/// 3,7 МБ кадр). Трёх хватает: декодер быстрее реального времени в разы.
const VIDEO_AHEAD: usize = 3;
/// Пакетов видео в ожидании не больше (сжатые — секунды видео).
const MAX_PACKETS: usize = 600;
/// Свободных буферов кадров про запас.
const POOL: usize = 2;
/// Звука вперёд, с.
const AUDIO_AHEAD: f64 = 0.5;
/// Декодер видео — в 1 поток. Потоки FFmpeg создаются на каждый плеер, и
/// каждому glibc даёт свою арену malloc, которую `malloc_trim` не отдаёт:
/// при 2 потоках каждый просмотр оставлял +8–10 МБ (playtest, malloc_stats:
/// занято 7 МБ стабильно, «system» росло). 1080p H.264/HEVC/VP9 в один
/// поток — 30 кадров/с без пропусков (examples/bench).
const VIDEO_THREADS: usize = 1;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Часы воспроизведения.
#[derive(Default)]
struct Clock {
    /// Позиция начала отсчёта (открытие или перемотка), с.
    base: f64,
    /// По звуку: сэмплов на канал, забранных устройством с `base`.
    played: u64,
    rate: u32,
    /// Без звука: накопленное время и момент последнего пуска.
    wall: f64,
    started: Option<Instant>,
}

struct Shared {
    video: Mutex<VecDeque<Frame>>,
    audio: Mutex<VecDeque<f32>>,
    pool: Mutex<Vec<Vec<u8>>>,
    space: Condvar,
    clock: Mutex<Clock>,
    /// Звук идёт на устройство (иначе часы — системные).
    audio_out: AtomicBool,
    channels: AtomicU32,
    paused: AtomicBool,
    stop: AtomicBool,
    eof: AtomicBool,
    seek: Mutex<Option<f64>>,
    volume: AtomicU32,
}

impl Shared {
    fn position(&self) -> f64 {
        let c = lock(&self.clock);
        if self.audio_out.load(Ordering::Relaxed) && c.rate > 0 {
            c.base + c.played as f64 / f64::from(c.rate)
        } else {
            c.base + c.wall + c.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0)
        }
    }

    fn audio_secs(&self) -> f64 {
        let rate = lock(&self.clock).rate.max(1);
        let ch = self.channels.load(Ordering::Relaxed).max(1);
        lock(&self.audio).len() as f64 / f64::from(ch) / f64::from(rate)
    }

    fn buffer(&self, len: usize) -> Vec<u8> {
        let mut b = lock(&self.pool).pop().unwrap_or_default();
        b.clear();
        b.resize(len, 0);
        b
    }
}

/// Плеер одного файла. Живёт в потоке, где создан (поток вывода звука `cpal`
/// не переносится между потоками); декодирование — в своём потоке.
pub struct Player {
    shared: Arc<Shared>,
    info: Info,
    thread: Option<JoinHandle<()>>,
}

/// Вывод звука — один на приложение: открытие и закрытие устройства на
/// каждый плеер (ALSA через PipeWire) оставляло ~8 МБ за раз (playtest).
/// Без воспроизведения поток на паузе; плеер подключается к нему и отключается.
struct AudioOut {
    stream: cpal::Stream,
    rate: u32,
    channels: u16,
    current: Arc<Mutex<Option<Arc<Shared>>>>,
    /// Короткие звуки (уведомления): подмешиваются поверх источника.
    effect: Arc<Mutex<VecDeque<f32>>>,
}

thread_local! {
    static OUT: std::cell::RefCell<Option<Option<AudioOut>>> = const { std::cell::RefCell::new(None) };
}

/// Открыть устройство вывода звука: стерео f32 на его частоте по умолчанию.
fn open_audio() -> Option<AudioOut> {
    let device = cpal::default_host().default_output_device()?;
    let def = device.default_output_config().ok()?;
    let rate = def.sample_rate();
    let channels = 2u16;
    let config = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
    let current: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));
    let effect: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
    let cur = current.clone();
    let eff = effect.clone();
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
                if let Some(sh) = lock(&cur).clone().filter(|sh| !sh.paused.load(Ordering::Relaxed)) {
                    let vol = f32::from_bits(sh.volume.load(Ordering::Relaxed));
                    let mut q = lock(&sh.audio);
                    let n = (data.len().min(q.len()) / usize::from(channels)) * usize::from(channels);
                    for s in data.iter_mut().take(n) {
                        *s = q.pop_front().unwrap_or(0.0) * vol;
                    }
                    drop(q);
                    lock(&sh.clock).played += (n / usize::from(channels)) as u64;
                    sh.space.notify_all();
                }
                let mut e = lock(&eff);
                for s in data.iter_mut() {
                    match e.pop_front() {
                        Some(x) => *s = (*s + x).clamp(-1.0, 1.0),
                        None => break,
                    }
                }
            },
            |e| log::warn!("звук: {e}"),
            None,
        )
        .map_err(|e| log::warn!("звук: устройство не открыто: {e}"))
        .ok()?;
    Some(AudioOut { stream, rate, channels, current, effect })
}

/// Подключить плеер к общему выводу звука: (частота, каналы) или None.
fn attach_audio(shared: &Arc<Shared>) -> Option<(u32, u16)> {
    OUT.with(|o| {
        let mut o = o.borrow_mut();
        let out = o.get_or_insert_with(open_audio).as_ref()?;
        *lock(&out.current) = Some(shared.clone());
        if let Err(e) = out.stream.play() {
            log::warn!("звук: {e}");
        }
        Some((out.rate, out.channels))
    })
}

fn detach_audio(shared: &Arc<Shared>) {
    // Плеер может закрываться при завершении потока, когда общий вывод уже
    // уничтожен (порядок уничтожения thread_local не задан): тогда — ничего.
    let _ = OUT.try_with(|o| {
        if let Some(Some(out)) = o.borrow().as_ref() {
            let mut cur = lock(&out.current);
            if cur.as_ref().is_some_and(|c| Arc::ptr_eq(c, shared)) {
                *cur = None;
                // Звук уведомления ещё звучит — поток не останавливать.
                if lock(&out.effect).is_empty() {
                    let _ = out.stream.pause();
                }
            }
        }
    });
}

/// Сыграть короткий звук (уведомление) поверх текущего воспроизведения, не
/// прерывая его. Файл декодируется целиком (не длиннее `EFFECT_MAX_SECS`) и
/// нормализуется по пику (усиление не больше `EFFECT_MAX_GAIN`), затем —
/// громкость `volume` (0…1).
pub fn play_effect(path: &Path, volume: f32) -> Result<(), String> {
    let (rate, channels, effect) = OUT.with(|o| {
        let mut o = o.borrow_mut();
        let out = o.get_or_insert_with(open_audio).as_ref()?;
        Some((out.rate, out.channels, out.effect.clone()))
    }).ok_or("звук: нет устройства вывода")?;
    let mut samples = decode_all(path, rate, channels)?;
    let gain = normalize_gain(&samples) * volume.clamp(0.0, 1.0);
    for s in &mut samples {
        *s *= gain;
    }
    lock(&effect).extend(samples);
    OUT.with(|o| {
        if let Some(Some(out)) = o.borrow().as_ref() {
            if let Err(e) = out.stream.play() {
                log::warn!("звук: {e}");
            }
        }
    });
    Ok(())
}

/// Наибольшее усиление при нормализации (тишину не раздувать в шум).
pub const EFFECT_MAX_GAIN: f32 = 8.0;

/// Усиление до пика 0,95.
fn normalize_gain(s: &[f32]) -> f32 {
    let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak <= f32::EPSILON { 1.0 } else { (0.95 / peak).min(EFFECT_MAX_GAIN) }
}

/// Наибольшая длина короткого звука, с.
pub const EFFECT_MAX_SECS: u32 = 5;

fn decode_all(path: &Path, rate: u32, channels: u16) -> Result<Vec<f32>, String> {
    let err = |e: ff::Error| e.to_string();
    ff::init().map_err(err)?;
    let mut ictx = ff::format::input(&path).map_err(err)?;
    let stream = ictx.streams().best(ff::media::Type::Audio).ok_or("в файле нет звука")?;
    let index = stream.index();
    let mut dec = ff::codec::context::Context::from_parameters(stream.parameters()).map_err(err)?.decoder().audio()
        .map_err(err)?;
    let limit = (rate * EFFECT_MAX_SECS) as usize * usize::from(channels);
    let mut out: Vec<f32> = Vec::new();
    let mut resampler: Option<ff::software::resampling::Context> = None;
    let mut frame = ff::frame::Audio::empty();
    let mut drain = |dec: &mut ff::decoder::Audio, out: &mut Vec<f32>| -> Result<(), String> {
        while dec.receive_frame(&mut frame).is_ok() {
            if resampler.is_none() {
                let layout = frame.channel_layout();
                let layout = if layout.channels() > 0 { layout } else { ff::ChannelLayout::default(2) };
                resampler = Some(ff::software::resampling::Context::get(frame.format(), layout, frame.rate(),
                    ff::format::Sample::F32(ff::format::sample::Type::Packed),
                    ff::ChannelLayout::default(i32::from(channels)), rate).map_err(err)?);
            }
            let Some(r) = resampler.as_mut() else { break };
            let mut res = ff::frame::Audio::empty();
            if r.run(&frame, &mut res).is_err() {
                continue;
            }
            let n = res.samples() * usize::from(channels);
            out.extend(res.data(0)[..n * 4].chunks_exact(4).map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])));
        }
        Ok(())
    };
    for (s, packet) in ictx.packets() {
        if s.index() != index {
            continue;
        }
        if dec.send_packet(&packet).is_ok() {
            drain(&mut dec, &mut out)?;
        }
        if out.len() >= limit {
            break;
        }
    }
    let _ = dec.send_eof();
    drain(&mut dec, &mut out)?;
    out.truncate(limit);
    Ok(out)
}

impl Player {
    /// Открыть файл и начать воспроизведение. Кадр вписывается в `max_w`×`max_h`.
    pub fn open(path: &Path, max_w: u32, max_h: u32) -> Result<Self, String> {
        Self::open_with(path, max_w, max_h, false)
    }

    /// `muted` — без звука вовсе (звук не декодируется, устройство не занимается):
    /// GIF и кружки, играющие в ленте сами (В8).
    pub fn open_with(path: &Path, max_w: u32, max_h: u32, muted: bool) -> Result<Self, String> {
        ff::init().map_err(|e| e.to_string())?;
        ff::util::log::set_level(ff::util::log::Level::Error);
        let shared = Arc::new(Shared {
            video: Mutex::new(VecDeque::new()),
            audio: Mutex::new(VecDeque::new()),
            pool: Mutex::new(Vec::new()),
            space: Condvar::new(),
            clock: Mutex::new(Clock::default()),
            audio_out: AtomicBool::new(false),
            channels: AtomicU32::new(2),
            paused: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            seek: Mutex::new(None),
            volume: AtomicU32::new(1.0f32.to_bits()),
        });
        let audio = if muted { None } else { attach_audio(&shared) };
        let (out_rate, out_ch) = audio.unwrap_or((48_000, 2));
        lock(&shared.clock).rate = out_rate;
        shared.channels.store(u32::from(out_ch), Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let sh = shared.clone();
        let path: PathBuf = path.to_path_buf();
        let audio_ok = audio.is_some();
        let thread = std::thread::Builder::new()
            .name("player".into())
            .spawn(move || {
                if let Err(e) = decode(&sh, &path, max_w, max_h, out_rate, out_ch, audio_ok, &tx) {
                    log::warn!("воспроизведение: {e}");
                    let _ = tx.send(Err(e));
                }
                sh.eof.store(true, Ordering::Relaxed);
            })
            .map_err(|e| e.to_string())?;
        let info = match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(info)) => info,
            Ok(Err(e)) => {
                detach_audio(&shared);
                return Err(e);
            }
            Err(_) => {
                detach_audio(&shared);
                return Err("файл не открылся".into());
            }
        };
        shared.audio_out.store(info.has_audio && audio_ok, Ordering::Relaxed);
        lock(&shared.clock).started = Some(Instant::now());
        Ok(Self { shared, info, thread: Some(thread) })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    /// Текущая позиция, с.
    pub fn position(&self) -> f64 {
        let p = self.shared.position();
        if self.info.duration > 0.0 { p.min(self.info.duration) } else { p }
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        if self.paused() == paused {
            return;
        }
        let mut c = lock(&self.shared.clock);
        if paused {
            if let Some(s) = c.started.take() {
                c.wall += s.elapsed().as_secs_f64();
            }
        } else {
            c.started = Some(Instant::now());
        }
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Перемотка: очереди сбрасываются, декодер догоняет позицию.
    pub fn seek(&self, secs: f64) {
        let t = if self.info.duration > 0.0 { secs.clamp(0.0, self.info.duration) } else { secs.max(0.0) };
        *lock(&self.shared.seek) = Some(t);
        self.shared.eof.store(false, Ordering::Relaxed);
        self.shared.space.notify_all();
    }

    /// Громкость 0…1.
    pub fn set_volume(&self, v: f32) {
        self.shared.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume.load(Ordering::Relaxed))
    }

    /// Кадр, время которого наступило (самый свежий; опоздавшие — в пул).
    pub fn take_frame(&self) -> Option<Frame> {
        let pos = self.shared.position();
        let mut q = lock(&self.shared.video);
        let mut last: Option<Frame> = None;
        while q.front().is_some_and(|f| f.pts <= pos + 0.005) {
            if let Some(old) = last.replace(q.pop_front()?) {
                self.recycle(old);
            }
        }
        drop(q);
        if last.is_some() {
            self.shared.space.notify_all();
        }
        last
    }

    /// Вернуть буфер показанного кадра в пул.
    pub fn recycle(&self, f: Frame) {
        let mut pool = lock(&self.shared.pool);
        if pool.len() < POOL {
            pool.push(f.rgba);
        }
    }

    /// Файл доигран.
    pub fn ended(&self) -> bool {
        let sh = &self.shared;
        sh.eof.load(Ordering::Relaxed)
            && lock(&sh.video).is_empty()
            && lock(&sh.audio).is_empty()
            && lock(&sh.seek).is_none()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        detach_audio(&self.shared);
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.space.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Вернуть освобождённую память системе (П-6: без этого glibc держит
/// страницы декодера — +30 МБ после закрытия окна просмотра).
pub fn trim_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: функция glibc без указателей; безопасна в любой момент.
        unsafe {
            malloc_trim(0);
        }

    }
}

/// Вписать `w`×`h` в `max_w`×`max_h` без увеличения; ширина чётная.
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (2, 2);
    }
    let k = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h)).min(1.0);
    let tw = ((f64::from(w) * k).round() as u32).max(2) & !1;
    let th = ((f64::from(h) * k).round() as u32).max(2);
    (tw, th)
}

#[allow(clippy::too_many_arguments)]
fn decode(sh: &Shared, path: &Path, max_w: u32, max_h: u32, out_rate: u32, out_ch: u16, audio_ok: bool,
    ready: &mpsc::Sender<Result<Info, String>>) -> Result<(), String> {
    let err = |e: ff::Error| e.to_string();
    let mut ictx = ff::format::input(path).map_err(err)?;
    let duration = if ictx.duration() > 0 { ictx.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE) } else { 0.0 };

    let mut video = match ictx.streams().best(ff::media::Type::Video) {
        Some(s) => {
            let mut ctx = ff::codec::context::Context::from_parameters(s.parameters()).map_err(err)?;
            ctx.set_threading(ff::codec::threading::Config {
                kind: ff::codec::threading::Type::Frame,
                count: VIDEO_THREADS,
            });
            let dec = ctx.decoder().video().map_err(err)?;
            Some((s.index(), f64::from(s.time_base()), dec))
        }
        None => None,
    };
    let mut audio = match ictx.streams().best(ff::media::Type::Audio).filter(|_| audio_ok) {
        Some(s) => {
            let ctx = ff::codec::context::Context::from_parameters(s.parameters()).map_err(err)?;
            let dec = ctx.decoder().audio().map_err(err)?;
            Some((s.index(), f64::from(s.time_base()), dec))
        }
        None => None,
    };
    let (vw, vh) = video.as_ref().map(|v| fit(v.2.width(), v.2.height(), max_w, max_h)).unwrap_or((0, 0));
    let info = Info { duration, width: vw, height: vh, has_video: video.is_some(), has_audio: audio.is_some() };
    if video.is_none() && audio.is_none() {
        return Err("в файле нет ни видео, ни звука".into());
    }
    let _ = ready.send(Ok(info));

    let mut scaler: Option<(ff::format::Pixel, u32, u32, ff::software::scaling::Context)> = None;
    let mut resampler: Option<ff::software::resampling::Context> = None;
    let mut scaled = ff::frame::Video::empty();
    let mut vframe = ff::frame::Video::empty();
    let mut aframe = ff::frame::Audio::empty();
    let mut skip_until = 0.0f64;
    let has_audio = audio.is_some();
    let has_video = video.is_some();

    // Пакеты видео, прочитанные ради звука, ждут сжатыми: в кадры (3,7 МБ
    // на 720p) декодируются, только когда в очереди кадров есть место —
    // иначе при звуке, лежащем в файле кусками, кадров набиралось на секунду.
    let mut vpackets: VecDeque<ff::Packet> = VecDeque::new();
    let mut read_eof = false;
    let room = |sh: &Shared| lock(&sh.video).len() < VIDEO_AHEAD;

    while !sh.stop.load(Ordering::Relaxed) {
        // Перемотка: к ключевому кадру не позже цели, затем догнать цель.
        if let Some(t) = lock(&sh.seek).take() {
            let ts = (t * f64::from(ff::ffi::AV_TIME_BASE)) as i64;
            if let Err(e) = ictx.seek(ts, ..ts) {
                log::warn!("перемотка: {e}");
            }
            if let Some(v) = video.as_mut() {
                v.2.flush();
            }
            if let Some(a) = audio.as_mut() {
                a.2.flush();
            }
            resampler = None;
            vpackets.clear();
            read_eof = false;
            let old: Vec<Frame> = lock(&sh.video).drain(..).collect();
            for f in old {
                let mut pool = lock(&sh.pool);
                if pool.len() < POOL {
                    pool.push(f.rgba);
                }
            }
            lock(&sh.audio).clear();
            {
                let mut c = lock(&sh.clock);
                c.base = t;
                c.played = 0;
                c.wall = 0.0;
                if c.started.is_some() {
                    c.started = Some(Instant::now());
                }
            }
            sh.eof.store(false, Ordering::Relaxed);
            skip_until = t;
        }
        // Видео: из накопленных пакетов, пока есть место для кадров.
        while room(sh) {
            let Some(pk) = vpackets.pop_front() else { break };
            if let Some(v) = video.as_mut() {
                if v.2.send_packet(&pk).is_ok() {
                    drain_video(sh, &mut video, &mut vframe, &mut scaled, &mut scaler, max_w, max_h, skip_until);
                }
            }
        }
        if read_eof && vpackets.is_empty() && !sh.eof.load(Ordering::Relaxed) {
            if let Some(v) = video.as_mut() {
                let _ = v.2.send_eof();
            }
            drain_video(sh, &mut video, &mut vframe, &mut scaled, &mut scaler, max_w, max_h, skip_until);
            sh.eof.store(true, Ordering::Relaxed);
        }
        // Читать дальше, только если не хватает звука или пакетов видео.
        let need_audio = has_audio && sh.audio_secs() < AUDIO_AHEAD;
        let need_video = has_video && vpackets.is_empty() && room(sh);
        if read_eof || !(need_audio || need_video) || vpackets.len() > MAX_PACKETS {
            let g = lock(&sh.pool);
            let _ = sh.space.wait_timeout(g, Duration::from_millis(20));
            continue;
        }
        let mut packet = ff::Packet::empty();
        match packet.read(&mut ictx) {
            Ok(()) => {}
            Err(ff::Error::Eof) => {
                if let Some(a) = audio.as_mut() {
                    let _ = a.2.send_eof();
                }
                drain_audio(sh, &mut audio, &mut aframe, &mut resampler, out_rate, out_ch, skip_until);
                read_eof = true;
                continue;
            }
            Err(e) => return Err(e.to_string()),
        }
        let idx = packet.stream();
        if video.as_ref().is_some_and(|v| v.0 == idx) {
            vpackets.push_back(packet);
        } else if let Some(a) = audio.as_mut().filter(|a| a.0 == idx) {
            if a.2.send_packet(&packet).is_ok() {
                drain_audio(sh, &mut audio, &mut aframe, &mut resampler, out_rate, out_ch, skip_until);
            }
        }
    }
    Ok(())
}

type VideoDec = Option<(usize, f64, ff::decoder::Video)>;
type AudioDec = Option<(usize, f64, ff::decoder::Audio)>;

#[allow(clippy::too_many_arguments)]
fn drain_video(sh: &Shared, video: &mut VideoDec, frame: &mut ff::frame::Video, scaled: &mut ff::frame::Video,
    scaler: &mut Option<(ff::format::Pixel, u32, u32, ff::software::scaling::Context)>, max_w: u32, max_h: u32,
    skip_until: f64) {
    let Some((_, tb, dec)) = video.as_mut() else { return };
    while dec.receive_frame(frame).is_ok() {
        let pts = frame.timestamp().unwrap_or(0) as f64 * *tb;
        if pts + 0.001 < skip_until {
            continue;
        }
        let (w, h, fmt) = (frame.width(), frame.height(), frame.format());
        let rebuild = !matches!(scaler, Some((f, sw, shh, _)) if *f == fmt && *sw == w && *shh == h);
        if rebuild {
            let (tw, th) = fit(w, h, max_w, max_h);
            match ff::software::scaling::Context::get(fmt, w, h, ff::format::Pixel::RGBA, tw, th,
                ff::software::scaling::Flags::BILINEAR)
            {
                Ok(ctx) => *scaler = Some((fmt, w, h, ctx)),
                Err(e) => {
                    log::warn!("масштабирование: {e}");
                    continue;
                }
            }
            *scaled = ff::frame::Video::empty();
        }
        let Some((_, _, _, ctx)) = scaler.as_mut() else { continue };
        if ctx.run(frame, scaled).is_err() {
            continue;
        }
        let (tw, th) = (scaled.width(), scaled.height());
        let stride = scaled.stride(0);
        let row = tw as usize * 4;
        let mut rgba = sh.buffer(row * th as usize);
        let src = scaled.data(0);
        for y in 0..th as usize {
            rgba[y * row..(y + 1) * row].copy_from_slice(&src[y * stride..y * stride + row]);
        }
        lock(&sh.video).push_back(Frame { width: tw, height: th, rgba, pts });
    }
}

fn drain_audio(sh: &Shared, audio: &mut AudioDec, frame: &mut ff::frame::Audio,
    resampler: &mut Option<ff::software::resampling::Context>, out_rate: u32, out_ch: u16, skip_until: f64) {
    let Some((_, tb, dec)) = audio.as_mut() else { return };
    while dec.receive_frame(frame).is_ok() {
        let pts = frame.timestamp().unwrap_or(0) as f64 * *tb;
        if pts + 0.02 < skip_until {
            continue;
        }
        if resampler.is_none() {
            let layout = frame.channel_layout();
            let layout = if layout.channels() > 0 { layout } else { ff::ChannelLayout::default(2) };
            match ff::software::resampling::Context::get(frame.format(), layout, frame.rate(),
                ff::format::Sample::F32(ff::format::sample::Type::Packed), ff::ChannelLayout::default(i32::from(out_ch)),
                out_rate)
            {
                Ok(r) => *resampler = Some(r),
                Err(e) => {
                    log::warn!("звук: {e}");
                    return;
                }
            }
        }
        let Some(r) = resampler.as_mut() else { return };
        let mut out = ff::frame::Audio::empty();
        if r.run(frame, &mut out).is_err() {
            continue;
        }
        let n = out.samples() * usize::from(out_ch);
        let bytes = &out.data(0)[..n * 4];
        let mut q = lock(&sh.audio);
        q.extend(bytes.chunks_exact(4).map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Звук уведомления из системной темы декодируется нашей сборкой FFmpeg
    /// (Ogg/Vorbis); без вывода на устройство. Нет файла — пропуск.
    #[test]
    fn decodes_theme_sound() {
        let p = Path::new("/usr/share/sounds/freedesktop/stereo/message-new-instant.oga");
        if !p.is_file() {
            return;
        }
        let s = decode_all(p, 48000, 2).expect("декодируется");
        let secs = s.len() as f64 / 2.0 / 48000.0;
        assert!(secs > 0.1 && secs <= f64::from(EFFECT_MAX_SECS), "длина {secs:.2} с");
        assert!(s.iter().any(|x| x.abs() > 0.01), "не тишина");
        let g = normalize_gain(&s);
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs())) * g;
        assert!(g >= 1.0 && (0.9..=0.96).contains(&peak), "после нормализации пик {peak:.2}, усиление {g:.1}");
    }

    #[test]
    fn fit_keeps_aspect_without_upscale() {
        assert_eq!(fit(1920, 1080, 960, 960), (960, 540));
        assert_eq!(fit(640, 480, 1920, 1080), (640, 480), "не увеличивать");
        assert_eq!(fit(1081, 1920, 1000, 1000), (562, 1000), "ширина чётная");
    }
}
