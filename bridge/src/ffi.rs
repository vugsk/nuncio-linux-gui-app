//! Сырые объявления C ABI сетевого слоя (target/native/libnuncio_net.h).
//! Коды возврата зеркалируют #define NN_* из net/ffi/ffi.go.

#[repr(C)]
pub struct NnBuf {
    pub ptr: *mut u8,
    pub len: usize,
}

pub const NN_OK: i32 = 0;
pub const NN_TIMEOUT: i32 = 1;
pub const NN_STOPPED: i32 = 2;
pub const NN_ERR_ARG: i32 = -1;
pub const NN_ERR_STATE: i32 = -2;
pub const NN_ERR_VERSION: i32 = -3;
pub const NN_ERR_INTERNAL: i32 = -4;
pub const NN_ERR_FULL: i32 = -5;

extern "C" {
    pub fn nn_start(req: *const u8, len: usize) -> i32;
    pub fn nn_submit(cmd_id: u64, cmd: *const u8, len: usize) -> i32;
    pub fn nn_next_event(timeout_ms: u32, out: *mut NnBuf) -> i32;
    pub fn nn_free(buf: NnBuf);
    pub fn nn_shutdown(timeout_ms: u32) -> i32;
}
