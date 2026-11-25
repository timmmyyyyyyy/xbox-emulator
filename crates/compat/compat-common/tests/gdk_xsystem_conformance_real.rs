use core::ffi::c_void;
use std::sync::OnceLock;

#[macro_use]
#[path = "support/xsys.rs"]
mod xsys;

extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, name: *const u8) -> *mut c_void;
}

fn setup(_scenario: &str) -> bool {
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| unsafe {
        let name: Vec<u16> = "xgameruntime.dll".encode_utf16().chain(Some(0)).collect();
        let h = LoadLibraryExW(name.as_ptr(), core::ptr::null_mut(), 0x800);
        if h.is_null() {
            eprintln!("SKIP: no System32 xgameruntime.dll (Gaming Services not installed)");
            return false;
        }
        let init: unsafe extern "system" fn(u32, u32) -> i32 = core::mem::transmute(GetProcAddress(h, b"InitializeApiImpl\0".as_ptr()));
        let hr = init(0x4a61, 0x2712);
        if hr < 0 {
            eprintln!("SKIP: InitializeApiImpl failed hr={hr:#010x}");
            return false;
        }
        xsys::install(core::mem::transmute::<*mut c_void, xsys::Query>(GetProcAddress(h, b"QueryApiImpl\0".as_ptr())));
        true
    })
}

xsys_tests!(setup);
