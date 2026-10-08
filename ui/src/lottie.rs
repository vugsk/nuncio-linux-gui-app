//! Анимированные стикеры TGS (Lottie в gzip) через ThorVG (C API, статически,
//! только программная отрисовка; решение пользователя 2026-10-09). Кадр
//! рисуется в буфер RGBA по времени, по кругу.

use std::ffi::{c_char, c_void};
use std::io::Read;
use std::time::Instant;

type TvgResult = i32;
const TVG_RESULT_SUCCESS: TvgResult = 0;
/// RGBA в памяти (младший байт — красный), без предумножения альфы.
const TVG_COLORSPACE_ABGR8888S: i32 = 2;
const TVG_ENGINE_OPTION_DEFAULT: i32 = 1;

#[link(name = "thorvg-1", kind = "static")]
unsafe extern "C" {
    fn tvg_engine_init(threads: u32) -> TvgResult;
    fn tvg_swcanvas_create(op: i32) -> *mut c_void;
    fn tvg_swcanvas_set_target(canvas: *mut c_void, buf: *mut u32, stride: u32, w: u32, h: u32, cs: i32) -> TvgResult;
    fn tvg_canvas_add(canvas: *mut c_void, paint: *mut c_void) -> TvgResult;
    fn tvg_canvas_update(canvas: *mut c_void) -> TvgResult;
    fn tvg_canvas_draw(canvas: *mut c_void, clear: bool) -> TvgResult;
    fn tvg_canvas_sync(canvas: *mut c_void) -> TvgResult;
    fn tvg_canvas_destroy(canvas: *mut c_void) -> TvgResult;
    fn tvg_animation_new() -> *mut c_void;
    fn tvg_animation_get_picture(anim: *mut c_void) -> *mut c_void;
    fn tvg_animation_set_frame(anim: *mut c_void, no: f32) -> TvgResult;
    fn tvg_animation_get_total_frame(anim: *mut c_void, cnt: *mut f32) -> TvgResult;
    fn tvg_animation_get_duration(anim: *mut c_void, d: *mut f32) -> TvgResult;
    fn tvg_animation_del(anim: *mut c_void) -> TvgResult;
    fn tvg_picture_load_data(pic: *mut c_void, data: *const c_char, size: u32, mime: *const c_char,
        rpath: *const c_char, copy: bool) -> TvgResult;
    fn tvg_picture_set_size(pic: *mut c_void, w: f32, h: f32) -> TvgResult;
}

/// Распакованный JSON Lottie не больше этого (защита от «бомб» в gzip).
const MAX_JSON: u64 = 8 * 1024 * 1024;

fn engine() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| unsafe {
        tvg_engine_init(0);
    });
}

/// Анимация, отрисовываемая в собственный буфер размера `w`×`h`.
pub struct Lottie {
    anim: *mut c_void,
    canvas: *mut c_void,
    buf: Vec<u32>,
    w: u32,
    h: u32,
    frames: f32,
    duration: f32,
    start: Instant,
    last: f32,
}

impl Lottie {
    /// Файл TGS (gzip) или JSON Lottie → анимация `w`×`h`.
    pub fn open(path: &str, w: u32, h: u32) -> Result<Self, String> {
        let raw = std::fs::read(path).map_err(|e| e.to_string())?;
        let json = if raw.starts_with(&[0x1f, 0x8b]) {
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(raw.as_slice()).take(MAX_JSON + 1).read_to_end(&mut out)
                .map_err(|e| format!("tgs: {e}"))?;
            if out.len() as u64 > MAX_JSON {
                return Err("tgs: слишком большой".into());
            }
            out
        } else {
            raw
        };
        Self::from_json(&json, w, h)
    }

    fn from_json(json: &[u8], w: u32, h: u32) -> Result<Self, String> {
        engine();
        let (w, h) = (w.max(1), h.max(1));
        let size = u32::try_from(json.len()).map_err(|_| "tgs: слишком большой")?;
        unsafe {
            let anim = tvg_animation_new();
            if anim.is_null() {
                return Err("thorvg: анимация не создана".into());
            }
            let pic = tvg_animation_get_picture(anim);
            let mime = c"lottie+json";
            if pic.is_null()
                || tvg_picture_load_data(pic, json.as_ptr().cast(), size, mime.as_ptr(), std::ptr::null(), true)
                    != TVG_RESULT_SUCCESS
            {
                tvg_animation_del(anim);
                return Err("thorvg: Lottie не разобран".into());
            }
            tvg_picture_set_size(pic, w as f32, h as f32);
            let (mut frames, mut duration) = (0.0f32, 0.0f32);
            tvg_animation_get_total_frame(anim, &mut frames);
            tvg_animation_get_duration(anim, &mut duration);
            let canvas = tvg_swcanvas_create(TVG_ENGINE_OPTION_DEFAULT);
            if canvas.is_null() {
                tvg_animation_del(anim);
                return Err("thorvg: холст не создан".into());
            }
            let mut buf = vec![0u32; (w * h) as usize];
            if tvg_swcanvas_set_target(canvas, buf.as_mut_ptr(), w, w, h, TVG_COLORSPACE_ABGR8888S)
                != TVG_RESULT_SUCCESS
                || tvg_canvas_add(canvas, pic) != TVG_RESULT_SUCCESS
            {
                tvg_canvas_destroy(canvas);
                tvg_animation_del(anim);
                return Err("thorvg: холст не настроен".into());
            }
            Ok(Self { anim, canvas, buf, w, h, frames, duration, start: Instant::now(), last: -1.0 })
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// Кадр на текущий момент (по кругу) как RGBA; `None` — тот же, что прошлый.
    pub fn frame(&mut self) -> Option<&[u8]> {
        if self.frames < 1.0 || self.duration <= 0.0 {
            return None;
        }
        let t = self.start.elapsed().as_secs_f32() % self.duration;
        let no = (t / self.duration * self.frames).floor().min(self.frames - 1.0);
        self.render(no)
    }

    fn render(&mut self, no: f32) -> Option<&[u8]> {
        if no == self.last {
            return None;
        }
        self.last = no;
        unsafe {
            tvg_animation_set_frame(self.anim, no);
            tvg_canvas_update(self.canvas);
            if tvg_canvas_draw(self.canvas, true) != TVG_RESULT_SUCCESS {
                return None;
            }
            tvg_canvas_sync(self.canvas);
        }
        // u32 ABGR (младший байт — R) в памяти — это RGBA по байтам.
        let bytes = unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), self.buf.len() * 4) };
        Some(bytes)
    }
}

impl Drop for Lottie {
    fn drop(&mut self) {
        unsafe {
            tvg_canvas_destroy(self.canvas);
            tvg_animation_del(self.anim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Красный квадрат 50×50, едущий слева направо за 10 кадров.
    const MOVING: &str = r#"{"v":"5.7.4","fr":10,"ip":0,"op":10,"w":100,"h":100,"layers":[{"ty":4,"ind":1,"ip":0,"op":10,"st":0,
"ks":{"o":{"a":0,"k":100},"r":{"a":0,"k":0},"p":{"a":1,"k":[{"t":0,"s":[25,50,0]},{"t":10,"s":[75,50,0]}]},
"a":{"a":0,"k":[0,0,0]},"s":{"a":0,"k":[100,100,100]}},
"shapes":[{"ty":"rc","p":{"a":0,"k":[0,0]},"s":{"a":0,"k":[50,50]},"r":{"a":0,"k":0}},
{"ty":"fl","c":{"a":0,"k":[1,0,0,1]},"o":{"a":0,"k":100}}]}]}"#;

    #[test]
    fn renders_frames_with_alpha() {
        let mut l = Lottie::from_json(MOVING.as_bytes(), 100, 100).expect("разобран");
        assert_eq!(l.frames, 10.0);
        let px = |b: &[u8], x: usize, y: usize| { let i = (y * 100 + x) * 4; [b[i], b[i + 1], b[i + 2], b[i + 3]] };
        let first = l.render(0.0).unwrap().to_vec();
        assert_eq!(px(&first, 5, 50)[0], 255, "в начале квадрат слева, красный");
        assert_eq!(px(&first, 95, 50)[3], 0, "справа прозрачно");
        let last = l.render(9.0).unwrap().to_vec();
        assert_eq!(px(&last, 90, 50)[0], 255, "к концу квадрат уехал вправо");
        assert_eq!(px(&last, 5, 50)[3], 0, "слева уже прозрачно");
        assert!(l.render(9.0).is_none(), "тот же кадр не перерисовывается");
    }

    #[test]
    fn rejects_garbage() {
        assert!(Lottie::from_json(b"not json", 64, 64).is_err());
    }
}
