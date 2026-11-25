use core::ffi::c_void;
use pixrt10::runtime::{title_table, CaptureHandler, CAPTURE};
use std::sync::atomic::{AtomicUsize, Ordering};

static ENDS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn begin(_: usize, _: *const c_void) -> i32 {
    0
}

unsafe extern "system" fn end(_: usize) -> i32 {
    ENDS.fetch_add(1, Ordering::SeqCst);
    0
}

#[test]
fn end_without_a_worker_preserves_ending_state_and_skips_handler() {
    std::env::set_var("TOMBOX_CONSOLE_OSBUILD", "19041");
    let handler = CaptureHandler { kind: 5, reserved: [0; 3], ctx: 0, begin, end };
    let slots = title_table();
    unsafe {
        let end_capture: unsafe extern "system" fn(u32) -> i32 = core::mem::transmute(slots[1]);
        let register: unsafe extern "system" fn(*const CaptureHandler) = core::mem::transmute(slots[4]);
        let clear: unsafe extern "system" fn(u32) = core::mem::transmute(slots[5]);
        let state: unsafe extern "system" fn() -> u32 = core::mem::transmute(slots[6]);
        register(&handler);
        let hr = end_capture(17);
        clear(handler.kind);
        assert_eq!(hr, 0x8007_0006u32 as i32);
        assert_eq!(state(), 2);
        assert_eq!(CAPTURE.end_arg.load(Ordering::SeqCst), 17);
        assert_eq!(ENDS.load(Ordering::SeqCst), 0);
    }
}
