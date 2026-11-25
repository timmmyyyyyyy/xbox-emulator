use compat_common::gdk::{self, xasync};
use core::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const E_ABORT: i32 = 0x8000_4004u32 as i32;
const E_PENDING: i32 = 0x8000_000Au32 as i32;
const OP_CLEANUP: u32 = 4;
const MANUAL: u32 = 0;
const THREAD_POOL: u32 = 1;

#[repr(C)]
struct Block {
    queue: usize,
    context: usize,
    callback: Option<unsafe extern "system" fn(*mut Block)>,
    internal: [u8; 32],
}

#[repr(C)]
struct Data {
    block: *mut Block,
    size: usize,
    buffer: *mut u8,
    context: usize,
}

type Provider = unsafe extern "system" fn(u32, *const Data) -> i32;

#[derive(Clone, Copy)]
struct Api {
    o: usize,
    status: unsafe extern "system" fn(*mut c_void, *mut Block, bool) -> i32,
    begin: unsafe extern "system" fn(*mut c_void, *mut Block, usize, usize, *const u8, Provider) -> i32,
    complete: unsafe extern "system" fn(*mut c_void, *mut Block, i32, usize),
    result: unsafe extern "system" fn(*mut c_void, *mut Block, usize, usize, *mut c_void, *mut usize) -> i32,
    create: unsafe extern "system" fn(*mut c_void, u32, u32, *mut usize) -> i32,
    dispatch: unsafe extern "system" fn(*mut c_void, usize, u32, u32) -> bool,
}

fn api() -> Api {
    std::env::set_var("TOMBOX_GDK_XASYNC", "emulated");
    std::env::set_var("TOMBOX_GDK_ROOT", std::env::temp_dir().join(format!("tombox_gdk_wait_{}", std::process::id())));
    assert!(gdk::rt::active());
    unsafe {
        let mut o: *mut c_void = core::ptr::null_mut();
        assert_eq!(gdk::query_api_impl(&xasync::XASYNC, &xasync::XASYNC, &mut o), 0);
        let vt = *(o as *const *const usize);
        let f = |n: usize| *vt.add(n);
        Api {
            o: o as usize,
            status: core::mem::transmute(f(3)),
            begin: core::mem::transmute(f(7)),
            complete: core::mem::transmute(f(10)),
            result: core::mem::transmute(f(11)),
            create: core::mem::transmute(f(12)),
            dispatch: core::mem::transmute(f(16)),
        }
    }
}

unsafe extern "system" fn idle(_op: u32, _d: *const Data) -> i32 {
    0
}

unsafe extern "system" fn slow_cleanup(op: u32, _d: *const Data) -> i32 {
    if op == OP_CLEANUP {
        std::thread::sleep(Duration::from_millis(200));
    }
    0
}

unsafe extern "system" fn slow_callback(b: *mut Block) {
    let phase = &*((*b).context as *const AtomicU32);
    phase.store(1, SeqCst);
    std::thread::sleep(Duration::from_millis(200));
    phase.store(2, SeqCst);
}

struct Call {
    a: Api,
    q: usize,
    b: *mut Block,
    phase: &'static AtomicU32,
}

impl Call {
    fn status(&self, wait: bool) -> i32 {
        unsafe { (self.a.status)(self.a.o as *mut c_void, self.b, wait) }
    }
    fn dispatch_completion(&self) -> bool {
        unsafe { (self.a.dispatch)(self.a.o as *mut c_void, self.q, 1, 0) }
    }
    fn completer(&self, delay_ms: u64, hr: i32, size: usize) -> impl FnOnce() + Send + 'static {
        let (a, b) = (self.a, self.b as usize);
        move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            unsafe { (a.complete)(a.o as *mut c_void, b as *mut Block, hr, size) }
        }
    }
}

fn call(provider: Provider, callback: bool) -> Call {
    call_on(THREAD_POOL, provider, callback)
}

fn call_on(completion: u32, provider: Provider, callback: bool) -> Call {
    let a = api();
    let mut q = 0usize;
    assert_eq!(unsafe { (a.create)(a.o as *mut c_void, THREAD_POOL, completion, &mut q) }, 0);
    let phase: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(0)));
    let b = Box::into_raw(Box::new(Block { queue: q, context: phase as *const AtomicU32 as usize, callback: if callback { Some(slow_callback) } else { None }, internal: [0; 32] }));
    assert_eq!(unsafe { (a.begin)(a.o as *mut c_void, b, 0, 0, core::ptr::null(), provider) }, 0);
    Call { a, q, b, phase }
}

fn until(ms: u64, f: impl Fn() -> bool) -> bool {
    let start = std::time::Instant::now();
    while !f() {
        if start.elapsed() > Duration::from_millis(ms) {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

#[test]
fn wait_on_a_completed_call_with_a_result_returns_after_the_callback() {
    let c = call(idle, true);
    c.completer(0, 0, 4)();
    assert_eq!(c.status(true), 0);
    assert_eq!(c.phase.load(SeqCst), 2, "XAsyncGetStatus(wait) returned before the completion callback had finished");
    let (mut v, mut used) = (0u32, 0usize);
    assert_eq!(unsafe { (c.a.result)(c.a.o as *mut c_void, c.b, 0, 4, &mut v as *mut u32 as *mut c_void, &mut used) }, 0);
    assert_eq!(used, 4);
}

#[test]
fn wait_on_a_completed_call_without_a_result_does_not_wait_for_the_callback() {
    for hr in [0, E_ABORT] {
        let c = call(idle, true);
        c.completer(0, hr, 0)();
        assert_eq!(c.status(true), hr);
        assert_ne!(c.phase.load(SeqCst), 2, "XAsyncGetStatus(wait) waited for the completion callback of a call without a result");
        assert!(until(5000, || c.phase.load(SeqCst) == 2));
    }
}

#[test]
fn wait_started_while_pending_returns_after_the_callback() {
    for size in [0usize, 4] {
        let c = call(idle, true);
        let completer = std::thread::spawn(c.completer(100, 0, size));
        assert_eq!(c.status(false), E_PENDING);
        assert_eq!(c.status(true), 0);
        assert_eq!(c.phase.load(SeqCst), 2, "size {size}: XAsyncGetStatus(wait) returned before the completion callback had finished");
        completer.join().unwrap();
    }
}

#[test]
fn block_is_not_written_after_wait_returns() {
    let c = call(slow_cleanup, false);
    let completer = std::thread::spawn(c.completer(0, E_ABORT, 0));
    assert!(until(5000, || c.status(false) == E_ABORT));
    assert_eq!(c.status(true), E_ABORT);
    let snap = unsafe { (*c.b).internal };
    completer.join().unwrap();
    assert_eq!(snap, unsafe { (*c.b).internal }, "the async block was written after XAsyncGetStatus(wait) returned");
}

#[test]
fn wait_does_not_pump_a_manual_completion_queue() {
    for late in [false, true] {
        let c = call_on(MANUAL, idle, true);
        let complete = c.completer(if late { 0 } else { 100 }, 0, 4);
        let completer = if late {
            complete();
            None
        } else {
            Some(std::thread::spawn(complete))
        };
        let returned = Arc::new(AtomicI32::new(1));
        let (a, b, r) = (c.a, c.b as usize, returned.clone());
        let waiter = std::thread::spawn(move || r.store(unsafe { (a.status)(a.o as *mut c_void, b as *mut Block, true) }, SeqCst));
        if let Some(t) = completer {
            t.join().unwrap();
        }
        assert!(!until(400, || returned.load(SeqCst) != 1), "late {late}: XAsyncGetStatus(wait) returned while the completion callback was still queued");
        assert_eq!(c.phase.load(SeqCst), 0, "late {late}: the wait ran the completion callback itself");
        assert!(c.dispatch_completion());
        waiter.join().unwrap();
        assert_eq!(returned.load(SeqCst), 0);
        assert_eq!(c.phase.load(SeqCst), 2);
    }
}

#[test]
fn wait_on_a_completed_call_without_a_result_leaves_the_callback_queued() {
    let c = call_on(MANUAL, idle, true);
    c.completer(0, 0, 0)();
    assert_eq!(c.status(true), 0);
    assert_eq!(c.phase.load(SeqCst), 0);
    assert!(c.dispatch_completion());
    assert_eq!(c.phase.load(SeqCst), 2);
}
