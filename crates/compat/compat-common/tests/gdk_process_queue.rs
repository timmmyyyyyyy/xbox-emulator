use compat_common::gdk::{self, streaming, xasync, Guid};
use core::ffi::c_void;

unsafe fn method(clsid: Guid, iid: Guid, slot: usize) -> (*mut c_void, usize) {
    let mut obj: *mut c_void = core::ptr::null_mut();
    assert_eq!(gdk::query_api_impl(&clsid, &iid, &mut obj), 0);
    (obj, *(*(obj as *const *const usize)).add(slot))
}

extern "system" {
    fn SetErrorMode(mode: u32) -> u32;
}

unsafe extern "system" fn task(_c: *mut c_void, _canceled: bool) {}
unsafe extern "system" fn on_state(_ctx: *mut c_void, _client: u64, _state: u32) {}

#[test]
fn set_current_process_task_queue_replaces_and_clears_the_process_queue() {
    std::env::set_var("TOMBOX_GDK_XASYNC", "emulated");
    std::env::set_var("TOMBOX_GDK_ROOT", std::env::temp_dir().join(format!("tombox_gdk_pq_{}", std::process::id())));
    assert!(gdk::rt::active());
    unsafe {
        let x = xasync::XASYNC;
        let (o, create) = method(x, x, 12);
        let create: unsafe extern "system" fn(*mut c_void, u32, u32, *mut usize) -> i32 = core::mem::transmute(create);
        let close: unsafe extern "system" fn(*mut c_void, usize) = core::mem::transmute(method(x, x, 17).1);
        let submit: unsafe extern "system" fn(*mut c_void, usize, u32, *mut c_void, unsafe extern "system" fn(*mut c_void, bool)) -> i32 = core::mem::transmute(method(x, x, 18).1);
        let terminate: unsafe extern "system" fn(*mut c_void, usize, bool, *mut c_void, usize) -> i32 = core::mem::transmute(method(x, x, 22).1);
        let get: unsafe extern "system" fn(*mut c_void, *mut usize) -> bool = core::mem::transmute(method(x, x, 25).1);
        let set: unsafe extern "system" fn(*mut c_void, usize) = core::mem::transmute(method(x, x, 26).1);

        let mut def = 0usize;
        assert!(get(o, &mut def));
        assert_ne!(def, 0);
        assert_eq!(terminate(o, def, false, core::ptr::null_mut(), 0), 0x8007_0005u32 as i32);

        let mut mine = 0usize;
        assert_eq!(create(o, 1, 1, &mut mine), 0);
        set(o, mine);
        close(o, mine);
        let mut cur = 0usize;
        assert!(get(o, &mut cur));
        assert_ne!(cur, 0);
        assert_ne!(cur, mine);
        close(o, cur);
        assert_eq!(submit(o, 0, 0, core::ptr::null_mut(), task), 0);

        set(o, 0);
        cur = 0x55;
        assert!(!get(o, &mut cur));
        assert_eq!(cur, 0);
        assert_eq!(submit(o, 0, 0, core::ptr::null_mut(), task), 0x8007_0057u32 as i32);
        assert!(!gdk::rt::process_queue_available());

        let (so, init) = method(streaming::CLSID, streaming::IID_5F5E, 3);
        let init: unsafe extern "system" fn(*mut c_void) -> i32 = core::mem::transmute(init);
        let reg: unsafe extern "system" fn(*mut c_void, usize, *mut c_void, usize, *mut u64) -> i32 = core::mem::transmute(method(streaming::CLSID, streaming::IID_5F5E, 12).1);
        assert_eq!(init(so), 0);
        let mut tok = 0u64;
        assert_eq!(reg(so, 0, core::ptr::null_mut(), on_state as *const () as usize, &mut tok), streaming::E_NO_TASK_QUEUE);

        set(o, def);
        assert!(get(o, &mut cur));
        assert_ne!(cur, 0);
        assert!(gdk::rt::process_queue_available());
        assert_eq!(reg(so, 0, core::ptr::null_mut(), on_state as *const () as usize, &mut tok), 0);
        assert_eq!(terminate(o, def, false, core::ptr::null_mut(), 0), 0x8007_0005u32 as i32);
    }
}

#[test]
#[ignore]
fn set_current_process_task_queue_invalid_handle_child() {
    if std::env::var("TOMBOX_PQ_FAILFAST_CHILD").as_deref() != Ok("1") {
        return;
    }
    std::env::set_var("TOMBOX_GDK_XASYNC", "emulated");
    std::env::set_var("TOMBOX_GDK_ROOT", std::env::temp_dir().join(format!("tombox_gdk_pqc_{}", std::process::id())));
    unsafe {
        SetErrorMode(0x8003);
        let x = xasync::XASYNC;
        let (o, set) = method(x, x, 26);
        let set: unsafe extern "system" fn(*mut c_void, usize) = core::mem::transmute(set);
        let fake = vec![0u64; 64];
        set(o, fake.as_ptr() as usize);
    }
    std::process::exit(0);
}

#[test]
fn set_current_process_task_queue_fail_fasts_on_an_invalid_handle() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "set_current_process_task_queue_invalid_handle_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("TOMBOX_PQ_FAILFAST_CHILD", "1")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0xC000_0602u32 as i32));
}
