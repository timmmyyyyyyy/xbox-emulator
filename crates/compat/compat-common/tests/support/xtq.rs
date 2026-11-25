#![allow(dead_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MANUAL: u32 = 0;
pub const THREAD_POOL: u32 = 1;
pub const SERIALIZED: u32 = 2;
pub const IMMEDIATE: u32 = 3;
pub const WORK: u32 = 0;
pub const COMPLETION: u32 = 1;
pub const INFINITE: u32 = u32::MAX;

pub const E_ABORT: i32 = 0x8000_4004u32 as i32;
pub const E_FAIL: i32 = 0x8000_4005u32 as i32;
pub const E_PENDING: i32 = 0x8000_000Au32 as i32;
pub const E_UNEXPECTED: i32 = 0x8000_FFFFu32 as i32;
pub const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
pub const E_ILLEGAL_METHOD_CALL: i32 = 0x8000_000Eu32 as i32;
pub const E_NOT_SUFFICIENT_BUFFER: i32 = 0x8007_007Au32 as i32;
pub const E_NO_TASK_QUEUE: i32 = 0x8007_01ABu32 as i32;
pub const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;

pub type TaskCb = unsafe extern "system" fn(*mut c_void, bool);
pub type MonitorCb = unsafe extern "system" fn(*mut c_void, usize, u32);
pub type TermCb = unsafe extern "system" fn(*mut c_void);
pub type Provider = unsafe extern "system" fn(u32, *const Data) -> i32;
pub type Completion = unsafe extern "system" fn(*mut Block);
pub type Work = unsafe extern "system" fn(*mut Block) -> i32;

pub const OP_BEGIN: u32 = 0;
pub const OP_DO_WORK: u32 = 1;
pub const OP_GET_RESULT: u32 = 2;
pub const OP_CANCEL: u32 = 3;
pub const OP_CLEANUP: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Block {
    pub queue: usize,
    pub context: usize,
    pub callback: Option<Completion>,
    pub internal: [u8; 32],
}

impl Block {
    pub fn new(queue: usize) -> Box<Block> {
        Box::new(Block { queue, context: 0, callback: None, internal: [0; 32] })
    }
    fn bytes(&self) -> [u8; 56] {
        unsafe { core::mem::transmute_copy(self) }
    }
}

#[repr(C)]
pub struct Data {
    pub block: *mut Block,
    pub size: usize,
    pub buffer: *mut u8,
    pub context: usize,
}

extern "system" {
    fn CreateEventW(attrs: *const c_void, manual: i32, initial: i32, name: *const u16) -> usize;
    fn SetEvent(h: usize) -> i32;
    fn ResetEvent(h: usize) -> i32;
    fn CloseHandle(h: usize) -> i32;
    fn WaitForSingleObject(h: usize, ms: u32) -> u32;
}

pub struct Event(pub usize);
impl Event {
    pub fn new(manual: bool) -> Event {
        let h = unsafe { CreateEventW(core::ptr::null(), manual as i32, 0, core::ptr::null()) };
        assert_ne!(h, 0);
        Event(h)
    }
    pub fn set(&self) {
        unsafe { SetEvent(self.0) };
    }
    pub fn reset(&self) {
        unsafe { ResetEvent(self.0) };
    }
    pub fn wait(&self, ms: u32) -> bool {
        unsafe { WaitForSingleObject(self.0, ms) == 0 }
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[derive(Clone, Copy)]
pub struct Api {
    obj: usize,
}

static API: OnceLock<Api> = OnceLock::new();

pub fn install(obj: *mut c_void) {
    assert!(!obj.is_null());
    let _ = API.set(Api { obj: obj as usize });
}

pub fn api() -> Api {
    *API.get().expect("XAsync interface not installed")
}

static CONSOLE_MODEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_console_model(on: bool) {
    CONSOLE_MODEL.store(on, SeqCst);
}

fn console_model() -> bool {
    CONSOLE_MODEL.load(SeqCst)
}

type Once = Box<dyn FnOnce(bool) + Send>;
unsafe extern "system" fn once(ctx: *mut c_void, canceled: bool) {
    Box::from_raw(ctx as *mut Once)(canceled)
}
type Many = dyn Fn(bool) + Send + Sync;
unsafe extern "system" fn many(ctx: *mut c_void, canceled: bool) {
    (*(ctx as *const Box<Many>))(canceled)
}
type Mon = Box<dyn Fn(usize, u32) + Send + Sync>;
unsafe extern "system" fn monitor_tramp(ctx: *mut c_void, queue: usize, port: u32) {
    (*(ctx as *const Mon))(queue, port)
}
type Term = Box<dyn FnOnce() + Send>;
unsafe extern "system" fn term_tramp(ctx: *mut c_void) {
    Box::from_raw(ctx as *mut Term)()
}

fn leak<T>(v: T) -> *mut c_void {
    Box::into_raw(Box::new(v)) as *mut c_void
}

impl Api {
    fn f(&self, slot: usize) -> usize {
        unsafe { *(*(self.obj as *const *const usize)).add(slot) }
    }
    fn o(&self) -> *mut c_void {
        self.obj as *mut c_void
    }

    pub fn try_create(&self, work: u32, completion: u32, out: &mut usize) -> i32 {
        let f: unsafe extern "system" fn(*mut c_void, u32, u32, *mut usize) -> i32 = unsafe { core::mem::transmute(self.f(12)) };
        unsafe { f(self.o(), work, completion, out) }
    }
    pub fn create(&self, work: u32, completion: u32) -> usize {
        let mut q = 0;
        assert_eq!(self.try_create(work, completion, &mut q), 0);
        assert_ne!(q, 0);
        q
    }
    pub fn try_composite(&self, work: usize, completion: usize, out: &mut usize) -> i32 {
        let f: unsafe extern "system" fn(*mut c_void, usize, usize, *mut usize) -> i32 = unsafe { core::mem::transmute(self.f(13)) };
        unsafe { f(self.o(), work, completion, out) }
    }
    pub fn composite(&self, work: usize, completion: usize) -> usize {
        let mut q = 0;
        assert_eq!(self.try_composite(work, completion, &mut q), 0);
        q
    }
    pub fn port(&self, q: usize, port: u32) -> usize {
        let f: unsafe extern "system" fn(*mut c_void, usize, u32, *mut usize) -> i32 = unsafe { core::mem::transmute(self.f(14)) };
        let mut p = 0;
        assert_eq!(unsafe { f(self.o(), q, port, &mut p) }, 0);
        assert_ne!(p, 0);
        p
    }
    pub fn try_dup(&self, q: usize, out: &mut usize) -> i32 {
        let f: unsafe extern "system" fn(*mut c_void, usize, *mut usize) -> i32 = unsafe { core::mem::transmute(self.f(15)) };
        unsafe { f(self.o(), q, out) }
    }
    pub fn dup(&self, q: usize) -> usize {
        let mut d = 0;
        assert_eq!(self.try_dup(q, &mut d), 0);
        d
    }
    pub fn dispatch(&self, q: usize, port: u32, timeout: u32) -> bool {
        let f: unsafe extern "system" fn(*mut c_void, usize, u32, u32) -> bool = unsafe { core::mem::transmute(self.f(16)) };
        unsafe { f(self.o(), q, port, timeout) }
    }
    pub fn close(&self, q: usize) {
        let f: unsafe extern "system" fn(*mut c_void, usize) = unsafe { core::mem::transmute(self.f(17)) };
        unsafe { f(self.o(), q) }
    }
    pub fn submit_raw(&self, q: usize, port: u32, delay: u32, ctx: *mut c_void, cb: TaskCb) -> i32 {
        if delay == 0 {
            let f: unsafe extern "system" fn(*mut c_void, usize, u32, *mut c_void, TaskCb) -> i32 = unsafe { core::mem::transmute(self.f(18)) };
            unsafe { f(self.o(), q, port, ctx, cb) }
        } else {
            let f: unsafe extern "system" fn(*mut c_void, usize, u32, u32, *mut c_void, TaskCb) -> i32 = unsafe { core::mem::transmute(self.f(19)) };
            unsafe { f(self.o(), q, port, delay, ctx, cb) }
        }
    }
    pub fn submit(&self, q: usize, port: u32, delay: u32, f: impl FnOnce(bool) + Send + 'static) -> i32 {
        let ctx = leak::<Once>(Box::new(f));
        let hr = self.submit_raw(q, port, delay, ctx, once);
        if hr < 0 {
            drop(unsafe { Box::from_raw(ctx as *mut Once) });
        }
        hr
    }
    pub fn submit_delayed_slot(&self, q: usize, port: u32, delay: u32, f: impl FnOnce(bool) + Send + 'static) -> i32 {
        let f2: unsafe extern "system" fn(*mut c_void, usize, u32, u32, *mut c_void, TaskCb) -> i32 = unsafe { core::mem::transmute(self.f(19)) };
        let ctx = leak::<Once>(Box::new(f));
        let hr = unsafe { f2(self.o(), q, port, delay, ctx, once) };
        if hr < 0 {
            drop(unsafe { Box::from_raw(ctx as *mut Once) });
        }
        hr
    }
    pub fn register_waiter(&self, q: usize, port: u32, handle: usize, f: impl Fn(bool) + Send + Sync + 'static) -> Result<u64, i32> {
        let g: unsafe extern "system" fn(*mut c_void, usize, u32, usize, *mut c_void, TaskCb, *mut u64) -> i32 = unsafe { core::mem::transmute(self.f(20)) };
        let ctx = leak::<Box<Many>>(Box::new(f));
        let mut token = 0u64;
        let hr = unsafe { g(self.o(), q, port, handle, ctx, many, &mut token) };
        if hr < 0 {
            Err(hr)
        } else {
            Ok(token)
        }
    }
    pub fn unregister_waiter(&self, q: usize, token: u64) {
        let g: unsafe extern "system" fn(*mut c_void, usize, u64) = unsafe { core::mem::transmute(self.f(21)) };
        unsafe { g(self.o(), q, token) }
    }
    pub fn terminate(&self, q: usize, wait: bool, f: Option<Box<dyn FnOnce() + Send>>) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, usize, bool, *mut c_void, Option<TermCb>) -> i32 = unsafe { core::mem::transmute(self.f(22)) };
        match f {
            Some(f) => unsafe { g(self.o(), q, wait, leak::<Term>(f), Some(term_tramp)) },
            None => unsafe { g(self.o(), q, wait, core::ptr::null_mut(), None) },
        }
    }
    pub fn register_monitor(&self, q: usize, f: impl Fn(usize, u32) + Send + Sync + 'static) -> u64 {
        let g: unsafe extern "system" fn(*mut c_void, usize, *mut c_void, MonitorCb, *mut u64) -> i32 = unsafe { core::mem::transmute(self.f(23)) };
        let ctx = leak::<Mon>(Box::new(f));
        let mut token = 0u64;
        assert_eq!(unsafe { g(self.o(), q, ctx, monitor_tramp, &mut token) }, 0);
        token
    }
    pub fn unregister_monitor(&self, q: usize, token: u64) {
        let g: unsafe extern "system" fn(*mut c_void, usize, u64) = unsafe { core::mem::transmute(self.f(24)) };
        unsafe { g(self.o(), q, token) }
    }
    pub fn process_queue(&self) -> (bool, usize) {
        let g: unsafe extern "system" fn(*mut c_void, *mut usize) -> bool = unsafe { core::mem::transmute(self.f(25)) };
        let mut q = 0usize;
        let ok = unsafe { g(self.o(), &mut q) };
        (ok, q)
    }
    pub fn set_process_queue(&self, q: usize) {
        let g: unsafe extern "system" fn(*mut c_void, usize) = unsafe { core::mem::transmute(self.f(26)) };
        unsafe { g(self.o(), q) }
    }

    pub fn status(&self, b: *mut Block, wait: bool) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, bool) -> i32 = unsafe { core::mem::transmute(self.f(3)) };
        unsafe { g(self.o(), b, wait) }
    }
    pub fn result_size(&self, b: *mut Block, out: &mut usize) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, *mut usize) -> i32 = unsafe { core::mem::transmute(self.f(4)) };
        unsafe { g(self.o(), b, out) }
    }
    pub fn cancel(&self, b: *mut Block) {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block) = unsafe { core::mem::transmute(self.f(5)) };
        unsafe { g(self.o(), b) }
    }
    pub fn run(&self, b: *mut Block, w: Work) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, Work) -> i32 = unsafe { core::mem::transmute(self.f(6)) };
        unsafe { g(self.o(), b, w) }
    }
    pub fn begin(&self, b: *mut Block, ctx: usize, identity: usize, p: Provider) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, usize, usize, *const u8, Provider) -> i32 = unsafe { core::mem::transmute(self.f(7)) };
        unsafe { g(self.o(), b, ctx, identity, core::ptr::null(), p) }
    }
    pub fn schedule(&self, b: *mut Block, delay: u32) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, u32) -> i32 = unsafe { core::mem::transmute(self.f(9)) };
        unsafe { g(self.o(), b, delay) }
    }
    pub fn complete(&self, b: *mut Block, hr: i32, size: usize) {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, i32, usize) = unsafe { core::mem::transmute(self.f(10)) };
        unsafe { g(self.o(), b, hr, size) }
    }
    pub fn result(&self, b: *mut Block, identity: usize, size: usize, buf: *mut c_void, used: *mut usize) -> i32 {
        let g: unsafe extern "system" fn(*mut c_void, *mut Block, usize, usize, *mut c_void, *mut usize) -> i32 = unsafe { core::mem::transmute(self.f(11)) };
        unsafe { g(self.o(), b, identity, size, buf, used) }
    }
}

fn until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    f()
}

fn counter() -> Arc<AtomicU32> {
    Arc::new(AtomicU32::new(0))
}

fn bump(c: &Arc<AtomicU32>) -> impl FnOnce(bool) + Send + 'static {
    let c = c.clone();
    move |_| {
        c.fetch_add(1, SeqCst);
    }
}

fn drain(q: usize) {
    let a = api();
    while a.dispatch(q, WORK, 0) | a.dispatch(q, COMPLETION, 0) {}
}

pub fn verify_stock_queue() {
    let a = api();
    let q = a.create(THREAD_POOL, MANUAL);
    let (work, done) = (counter(), counter());
    let (w, d) = (work.clone(), bump(&done));
    assert_eq!(
        a.submit(q, WORK, 0, move |_| {
            w.fetch_add(1, SeqCst);
            assert_eq!(api().submit(q, COMPLETION, 0, d), 0);
        }),
        0
    );
    assert!(a.dispatch(q, COMPLETION, 5000), "completion submitted from a thread-pool work callback never reached the manual port");
    assert_eq!((work.load(SeqCst), done.load(SeqCst)), (1, 1));
    a.close(q);
}

pub fn verify_composite_queue() {
    let a = api();
    let q = a.create(THREAD_POOL, MANUAL);
    let calls = counter();
    let c = calls.clone();
    assert_eq!(
        a.submit(q, WORK, 0, move |_| {
            c.fetch_add(1, SeqCst);
            let a = api();
            let wp = a.port(q, WORK);
            let comp = a.composite(wp, wp);
            assert_eq!(a.submit(comp, WORK, 0, bump(&c)), 0);
            a.close(comp);
        }),
        0
    );
    assert!(until(1000, || calls.load(SeqCst) == 2), "work submitted to a composite over the work port did not run on the thread pool");
    assert!(!a.dispatch(q, COMPLETION, 0), "nothing may be queued on the completion port");
    a.close(q);
}

pub fn verify_duplicate_queue_handle() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let dups: Vec<usize> = (0..10).map(|_| a.dup(q)).collect();
    for d in &dups {
        assert_ne!(*d, q, "XTaskQueueDuplicateHandle must return a new handle");
    }
    let c = counter();
    assert_eq!(a.submit(dups[3], WORK, 0, bump(&c)), 0);
    assert!(a.dispatch(dups[7], WORK, 0));
    assert_eq!(c.load(SeqCst), 1);
    for d in dups {
        a.close(d);
    }
    let fake = Box::new([0usize; 8]);
    let mut out = 0usize;
    assert!(a.try_dup(fake.as_ptr() as usize, &mut out) < 0, "a fake handle must not duplicate");
    assert_eq!(a.submit(q, WORK, 0, bump(&c)), 0);
    assert!(a.dispatch(q, WORK, 0));
    assert_eq!(c.load(SeqCst), 2);
    a.close(q);
}

pub fn verify_dispatch() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let (work, comp) = (counter(), counter());
    for _ in 0..10 {
        assert_eq!(a.submit(q, WORK, 0, bump(&work)), 0);
    }
    for _ in 0..10 {
        assert_eq!(a.submit(q, COMPLETION, 0, bump(&comp)), 0);
    }
    let mut n = 0;
    while a.dispatch(q, WORK, 0) {
        n += 1;
    }
    assert_eq!((n, work.load(SeqCst), comp.load(SeqCst)), (10, 10, 0));
    n = 0;
    while a.dispatch(q, COMPLETION, 0) {
        n += 1;
    }
    assert_eq!((n, work.load(SeqCst), comp.load(SeqCst)), (10, 10, 10));
    a.close(q);

    let q = a.create(MANUAL, THREAD_POOL);
    let me = std::thread::current().id();
    let threads = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..2 {
        let t = threads.clone();
        assert_eq!(a.submit(q, WORK, 0, move |_| t.lock().unwrap().push(("work", std::thread::current().id()))), 0);
    }
    assert!(a.dispatch(q, WORK, 0));
    assert_eq!(threads.lock().unwrap().len(), 1);
    assert!(a.dispatch(q, WORK, 0));
    assert_eq!(threads.lock().unwrap().len(), 2);
    assert!(threads.lock().unwrap().iter().all(|(_, id)| *id == me), "manual work must run on the dispatching thread");
    let t = threads.clone();
    assert_eq!(a.submit(q, COMPLETION, 0, move |_| t.lock().unwrap().push(("completion", std::thread::current().id()))), 0);
    assert!(until(1000, || threads.lock().unwrap().len() == 3));
    assert_ne!(threads.lock().unwrap()[2].1, me, "thread-pool completion ran on the test thread");

    threads.lock().unwrap().clear();
    let t = threads.clone();
    assert_eq!(
        a.submit(q, COMPLETION, 0, move |_| {
            let t2 = t.clone();
            assert_eq!(api().submit(q, WORK, 0, move |_| t2.lock().unwrap().push(("work", std::thread::current().id()))), 0);
        }),
        0
    );
    assert!(a.dispatch(q, WORK, 1000), "work handed off from the completion port did not wake the manual dispatcher");
    assert_eq!(threads.lock().unwrap()[0], ("work", me));
    a.close(q);
}

pub fn verify_submitted_callback() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let (w, c) = (counter(), counter());
    let (w2, c2) = (w.clone(), c.clone());
    let token = a.register_monitor(q, move |_, port| {
        if port == WORK {
            w2.fetch_add(1, SeqCst);
        } else {
            c2.fetch_add(1, SeqCst);
        }
    });
    for _ in 0..4 {
        assert_eq!(a.submit(q, WORK, 0, |_| {}), 0);
    }
    for _ in 0..7 {
        assert_eq!(a.submit(q, COMPLETION, 0, |_| {}), 0);
    }
    assert_eq!((w.load(SeqCst), c.load(SeqCst)), (4, 7), "monitor must fire synchronously on every submit");
    a.unregister_monitor(q, token);
    drain(q);
    a.close(q);
}

pub fn verify_submit_callback_with_wait() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    for port in [WORK, COMPLETION] {
        let base = Instant::now();
        let times = Arc::new(Mutex::new([None::<Duration>; 3]));
        for (i, delay) in [(0usize, 1000u32), (1, 0), (2, 500)] {
            let t = times.clone();
            assert_eq!(a.submit(q, port, delay, move |_| t.lock().unwrap()[i] = Some(base.elapsed())), 0);
        }
        assert!(a.dispatch(q, port, 0));
        assert!(!a.dispatch(q, port, 0), "a delayed callback was dispatchable before its delay");
        assert!(a.dispatch(q, port, 700), "500 ms callback did not wake a 700 ms dispatch");
        assert!(a.dispatch(q, port, 1200), "1000 ms callback did not wake a 1200 ms dispatch");
        assert!(!a.dispatch(q, port, 0));
        let t = *times.lock().unwrap();
        let ms = |d: Option<Duration>| d.expect("callback never ran").as_millis();
        assert!(ms(t[0]) >= 1000 && ms(t[0]) < 1600, "1000 ms callback at {} ms", ms(t[0]));
        assert!(ms(t[1]) < 500, "immediate callback at {} ms", ms(t[1]));
        assert!(ms(t[2]) >= 500 && ms(t[2]) < 1000, "500 ms callback at {} ms", ms(t[2]));
    }
    a.close(q);
}

pub fn verify_register_callback_submitted() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let calls: Vec<Arc<AtomicU32>> = (0..5).map(|_| counter()).collect();
    let tokens: Vec<u64> = calls
        .iter()
        .map(|c| {
            let c = c.clone();
            a.register_monitor(q, move |_, _| {
                c.fetch_add(1, SeqCst);
            })
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    assert!(tokens.iter().all(|t| seen.insert(*t)), "registration tokens must be unique: {tokens:?}");
    for _ in 0..3 {
        assert_eq!(a.submit(q, WORK, 0, |_| {}), 0);
    }
    assert!(calls.iter().all(|c| c.load(SeqCst) == 3));
    for i in (1..5).step_by(2) {
        a.unregister_monitor(q, tokens[i]);
    }
    for _ in 0..3 {
        assert_eq!(a.submit(q, WORK, 0, |_| {}), 0);
    }
    let got: Vec<u32> = calls.iter().map(|c| c.load(SeqCst)).collect();
    assert_eq!(got, vec![6, 3, 6, 3, 6]);
    drain(q);
    a.close(q);
}

pub fn verify_immediate_dispatch() {
    let a = api();
    let q = a.create(MANUAL, IMMEDIATE);
    let c = counter();
    for i in 1..=10 {
        assert_eq!(a.submit(q, COMPLETION, 0, bump(&c)), 0);
        assert_eq!(c.load(SeqCst), i, "immediate port must run the callback inside the submit");
    }
    assert_eq!(a.submit(q, COMPLETION, 200, bump(&c)), 0);
    assert_eq!(c.load(SeqCst), 10, "a delayed callback on an immediate port ran early");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(c.load(SeqCst), 11, "a delayed callback on an immediate port never ran");
    a.close(q);
}

pub fn verify_serialized_thread_pool_dispatch() {
    let a = api();
    let q = a.create(THREAD_POOL, SERIALIZED);
    let order = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let overlap = Arc::new(AtomicBool::new(false));
    for i in 0..100u32 {
        let (o, act, ov) = (order.clone(), active.clone(), overlap.clone());
        assert_eq!(
            a.submit(q, COMPLETION, 0, move |_| {
                if act.fetch_add(1, SeqCst) != 0 {
                    ov.store(true, SeqCst);
                }
                o.lock().unwrap().push(i);
                std::thread::yield_now();
                act.fetch_sub(1, SeqCst);
            }),
            0
        );
    }
    assert!(until(2000, || order.lock().unwrap().len() == 100));
    assert!(!overlap.load(SeqCst), "serialized thread-pool callbacks overlapped");
    assert_eq!(*order.lock().unwrap(), (0..100).collect::<Vec<_>>(), "serialized thread-pool callbacks ran out of order");
    a.close(q);
}

pub fn verify_serialized_thread_pool_does_not_strand_items() {
    let a = api();
    let q = a.create(SERIALIZED, SERIALIZED);
    let ev = Arc::new(Event::new(false));
    for round in 0..5000 {
        let e = ev.clone();
        assert_eq!(a.submit(q, COMPLETION, 0, move |_| e.set()), 0);
        assert!(ev.wait(5000), "item {round} was not dispatched");
    }
    a.close(q);
}

pub fn verify_multithread_manual_dispatch() {
    let a = api();
    let q = a.create(MANUAL, IMMEDIATE);
    let ran = counter();
    let spawn = || {
        std::thread::spawn(move || {
            let a = api();
            while a.dispatch(q, WORK, INFINITE) {}
        })
    };
    let (t1, t2) = (spawn(), spawn());
    for _ in 0..5 {
        assert_eq!(a.submit(q, WORK, 0, bump(&ran)), 0);
    }
    std::thread::sleep(Duration::from_millis(750));
    assert!(!t1.is_finished() && !t2.is_finished(), "XTaskQueueDispatch(INFINITE) returned without termination");
    assert_eq!(ran.load(SeqCst), 5);

    let comp = a.composite(a.port(q, WORK), a.port(q, COMPLETION));
    for _ in 0..5 {
        assert_eq!(a.submit(comp, WORK, 0, bump(&ran)), 0);
    }
    assert_eq!(a.terminate(comp, true, None), 0);
    std::thread::sleep(Duration::from_millis(750));
    assert!(!t1.is_finished() && !t2.is_finished(), "terminating a composite stopped the base queue's dispatchers");

    assert_eq!(a.terminate(q, true, None), 0);
    assert!(until(750, || t1.is_finished() && t2.is_finished()), "terminating the queue did not release XTaskQueueDispatch(INFINITE)");
    t1.join().unwrap();
    t2.join().unwrap();
    a.close(comp);
    a.close(q);
}

pub struct Pumped {
    pub q: usize,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
    events: [Arc<Event>; 2],
}

impl Pumped {
    pub fn new() -> Pumped {
        let a = api();
        let q = a.create(MANUAL, MANUAL);
        let events = [Arc::new(Event::new(false)), Arc::new(Event::new(false))];
        let ev = events.clone();
        a.register_monitor(q, move |_, port| ev[port as usize].set());
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..2u32)
            .map(|port| {
                let (e, s) = (events[port as usize].clone(), stop.clone());
                std::thread::spawn(move || loop {
                    e.wait(INFINITE);
                    if s.load(SeqCst) {
                        return;
                    }
                    while api().dispatch(q, port, 0) {}
                })
            })
            .collect();
        Pumped { q, stop, threads, events }
    }
}

impl Drop for Pumped {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for e in &self.events {
            e.set();
        }
        for t in self.threads.drain(..) {
            t.join().unwrap();
        }
        api().close(self.q);
    }
}

fn register_with_auto_reset(q: usize) {
    let a = api();
    for port in [WORK, COMPLETION] {
        let wait = Event::new(false);
        let signaled = Arc::new(Event::new(false));
        let count = counter();
        let (s, c) = (signaled.clone(), count.clone());
        let token = a
            .register_waiter(q, port, wait.0, move |_| {
                c.fetch_add(1, SeqCst);
                s.set();
            })
            .expect("XTaskQueueRegisterWaiter");
        for i in 1..=5 {
            wait.set();
            assert!(signaled.wait(1000), "port {port}: waiter callback did not run");
            assert!(!signaled.wait(100), "port {port}: an auto-reset event ran the waiter twice");
            assert_eq!(count.load(SeqCst), i);
        }
        a.unregister_waiter(q, token);
    }
}

pub fn verify_register_with_auto_reset() {
    let a = api();
    let q = a.create(THREAD_POOL, THREAD_POOL);
    register_with_auto_reset(q);
    a.close(q);
    let pumped = Pumped::new();
    register_with_auto_reset(pumped.q);
}

pub fn verify_shared_port_queue_rundown_keeps_other_waiters() {
    let a = api();
    for terminate in [true, false] {
        let q = a.create(THREAD_POOL, THREAD_POOL);
        let signaled = Arc::new(Event::new(false));
        let wait = Event::new(false);
        let count = counter();
        let (s, c) = (signaled.clone(), count.clone());
        let token = a
            .register_waiter(q, WORK, wait.0, move |canceled| {
                if !canceled {
                    c.fetch_add(1, SeqCst);
                    s.set();
                }
            })
            .unwrap();
        wait.set();
        assert!(signaled.wait(1000));
        let wp = a.port(q, WORK);
        let sibling = a.composite(wp, wp);
        if terminate {
            assert_eq!(a.terminate(sibling, true, None), 0);
        }
        a.close(sibling);
        wait.set();
        assert!(signaled.wait(1000), "terminate={terminate}: a sibling's rundown broke this queue's waiter");
        assert_eq!(count.load(SeqCst), 2);
        a.unregister_waiter(q, token);
        wait.set();
        assert!(!signaled.wait(300), "terminate={terminate}: waiter fired after XTaskQueueUnregisterWaiter");
        assert_eq!(count.load(SeqCst), 2);
        a.close(q);
    }
}

pub fn verify_shared_port_queue_rundown_drops_its_wait_handles() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let wp = a.port(q, WORK);
    let sibling = a.composite(wp, wp);
    let wait = Event::new(true);
    a.register_waiter(sibling, WORK, wait.0, |_| {}).unwrap();
    a.close(sibling);
    wait.set();
    let returned = Arc::new(AtomicBool::new(false));
    let r = returned.clone();
    let t = std::thread::spawn(move || {
        api().dispatch(q, WORK, 100);
        r.store(true, SeqCst);
    });
    let ok = until(2000, || returned.load(SeqCst));
    wait.reset();
    t.join().unwrap();
    assert!(ok, "Dispatch(100 ms) kept spinning on a closed sibling's wait handle");
    a.close(q);
}

fn queue_termination(work_mode: u32, wait: bool, serialized: bool, empty: bool) {
    let a = api();
    let q = a.create(work_mode, work_mode);
    struct D {
        q: usize,
        work: AtomicU32,
        completion: AtomicU32,
        serialized: bool,
        failure: Mutex<Option<(u32, i32)>>,
    }
    let d = Arc::new(D { q, work: AtomicU32::new(0), completion: AtomicU32::new(0), serialized, failure: Mutex::new(None) });
    let fail = |d: &D, line: u32, hr: i32| {
        d.failure.lock().unwrap().get_or_insert((line, hr));
    };
    unsafe extern "system" fn completion_cb(ctx: *mut c_void, cancel: bool) {
        let d = &*(ctx as *const D);
        if d.serialized && !cancel {
            d.failure.lock().unwrap().get_or_insert((line!(), E_UNEXPECTED));
        }
        d.completion.fetch_add(1, SeqCst);
    }
    unsafe extern "system" fn work_cb(ctx: *mut c_void, cancel: bool) {
        let d = &*(ctx as *const D);
        let a = api();
        if d.serialized {
            if !cancel && d.work.load(SeqCst) != 0 {
                d.failure.lock().unwrap().get_or_insert((line!(), E_UNEXPECTED));
            }
            let start = Instant::now();
            while a.submit(d.q, WORK, 0, |_| {}) != E_ABORT {
                if start.elapsed() > Duration::from_secs(30) {
                    d.failure.lock().unwrap().get_or_insert((line!(), E_UNEXPECTED));
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        d.work.fetch_add(1, SeqCst);
        let hr = a.submit_raw(d.q, COMPLETION, 0, ctx, completion_cb);
        if hr < 0 {
            d.failure.lock().unwrap().get_or_insert((line!(), hr));
        }
    }
    let ctx = Arc::as_ptr(&d) as *mut c_void;
    let (normal, future, events) = if empty { (0, 0, 0) } else { (5, 5, 5) };
    for _ in 0..normal {
        assert_eq!(a.submit_raw(q, WORK, 0, ctx, work_cb), 0);
    }
    for _ in 0..future {
        assert_eq!(a.submit_raw(q, WORK, 10000, ctx, work_cb), 0);
    }
    let handles: Vec<Event> = (0..events).map(|_| Event::new(false)).collect();
    for h in &handles {
        let dd = d.clone();
        a.register_waiter(q, WORK, h.0, move |cancel| unsafe { work_cb(Arc::as_ptr(&dd) as *mut c_void, cancel) }).unwrap();
    }
    if wait {
        assert_eq!(a.terminate(q, true, None), 0);
    } else {
        let done = Arc::new(Event::new(true));
        let dn = done.clone();
        assert_eq!(a.terminate(q, false, Some(Box::new(move || dn.set()))), 0);
        assert!(done.wait(5000), "termination callback never ran");
    }
    assert_eq!(a.submit_raw(q, WORK, 0, ctx, work_cb), E_ABORT, "submit to a terminated work port");
    assert_eq!(a.submit_raw(q, COMPLETION, 0, ctx, completion_cb), E_ABORT, "submit to a terminated completion port");
    let expected = normal + future + events;
    if !wait {
        until(5000, || d.work.load(SeqCst) == expected && d.completion.load(SeqCst) == expected);
    }
    assert_eq!(*d.failure.lock().unwrap(), None);
    assert_eq!(d.work.load(SeqCst), expected, "every pending, delayed and waiter work item must run (canceled) before termination completes");
    assert_eq!(d.completion.load(SeqCst), expected);
    let _ = fail;
    a.close(q);
}

pub fn verify_queue_termination() {
    queue_termination(THREAD_POOL, false, false, false);
    queue_termination(THREAD_POOL, true, false, false);
    queue_termination(SERIALIZED, true, true, false);
}

pub fn verify_empty_queue_termination() {
    queue_termination(THREAD_POOL, false, false, true);
    queue_termination(THREAD_POOL, true, false, true);
    queue_termination(SERIALIZED, true, true, true);
}

pub fn verify_global_queue() {
    let a = api();
    let (ok, global) = a.process_queue();
    assert!(ok);
    assert_ne!(global, 0);
    assert_eq!(a.submit(global, WORK, 0, |_| {}), 0);

    let ours = a.create(MANUAL, MANUAL);
    a.set_process_queue(ours);
    let (ok, q) = a.process_queue();
    assert!(ok);
    let c = counter();
    assert_eq!(a.submit(q, WORK, 0, bump(&c)), 0);
    assert!(a.dispatch(ours, WORK, 0), "the queue set with XTaskQueueSetCurrentProcessTaskQueue is not the process queue");
    assert_eq!(c.load(SeqCst), 1);
    a.close(q);
    a.set_process_queue(0);
    let (ok, q) = a.process_queue();
    assert!(!ok);
    assert_eq!(q, 0);

    let mut b = Block::new(0);
    assert_eq!(a.begin(&mut *b, 0, 0, abort_on_cancel), E_NO_TASK_QUEUE, "XAsyncBegin with no process queue");
    a.set_process_queue(global);
    let mut b = Block::new(0);
    assert_eq!(a.begin(&mut *b, 0, 0, abort_on_cancel), 0);
    a.cancel(&mut *b);
    assert_eq!(a.status(&mut *b, true), E_ABORT);

    let p = a.port(global, WORK);
    let comp = a.composite(p, p);
    assert_eq!(a.terminate(comp, true, None), 0);
    a.close(comp);
    let ev = Arc::new(Event::new(false));
    let e = ev.clone();
    assert_eq!(a.submit(global, WORK, 0, move |_| e.set()), 0);
    assert!(ev.wait(5000));
    if console_model() {
        assert_eq!(a.terminate(global, true, None), E_ACCESSDENIED);
        assert_eq!(a.submit(global, WORK, 0, |_| {}), 0);
    } else {
        assert_eq!(a.terminate(global, true, None), 0);
        assert_eq!(a.submit(global, WORK, 0, |_| {}), E_ABORT);
    }
    a.close(global);
    a.close(ours);
}

pub fn verify_close_in_termination_for_thread_pool() {
    let a = api();
    let q = a.create(THREAD_POOL, THREAD_POOL);
    let called = Arc::new(AtomicBool::new(false));
    let c = called.clone();
    assert_eq!(
        a.terminate(
            q,
            false,
            Some(Box::new(move || {
                api().close(q);
                c.store(true, SeqCst);
            }))
        ),
        0
    );
    assert!(until(5000, || called.load(SeqCst)));
}

pub fn verify_close_in_termination_for_manual() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let called = Arc::new(AtomicBool::new(false));
    let c = called.clone();
    assert_eq!(
        a.terminate(
            q,
            false,
            Some(Box::new(move || {
                c.store(true, SeqCst);
                api().close(q);
            }))
        ),
        0
    );
    assert!(!called.load(SeqCst), "a manual queue's termination callback must wait for dispatch");
    let end = Instant::now() + Duration::from_secs(5);
    while !called.load(SeqCst) && Instant::now() < end {
        a.dispatch(q, WORK, 0);
        if !called.load(SeqCst) {
            a.dispatch(q, COMPLETION, 0);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(called.load(SeqCst));
}

pub fn verify_wait_termination() {
    let a = api();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        let q = a.create(THREAD_POOL, THREAD_POOL);
        assert_eq!(a.terminate(q, true, None), 0);
        a.close(q);
    }
}

pub fn verify_manual_dispatch_at_termination() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    a.register_monitor(q, |queue, port| {
        api().dispatch(queue, port, 0);
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(api().terminate(q, true, None)).unwrap());
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(0), "Terminate(wait) on a monitor-dispatched manual queue did not finish");
    a.close(q);
}

#[derive(Default)]
struct CompositeData {
    q: AtomicUsize,
    work: AtomicI32,
    completion: AtomicI32,
    future: AtomicI32,
    waiter: AtomicI32,
    terminated: AtomicBool,
}

fn record(slot: &AtomicI32, canceled: bool) {
    slot.store(1 + canceled as i32, SeqCst);
}

pub fn verify_termination_of_composite_queue() {
    let a = api();
    let data = Arc::new(CompositeData::default());
    let comp = Arc::new(CompositeData::default());
    let wait = Event::new(false);
    let cwait = Event::new(false);
    let q = a.create(MANUAL, MANUAL);
    data.q.store(q, SeqCst);
    let wp = a.port(q, WORK);
    let cq = a.composite(wp, wp);
    comp.q.store(cq, SeqCst);

    let submit_all = |d: &Arc<CompositeData>, ev: &Event| {
        let q = d.q.load(SeqCst);
        let d1 = d.clone();
        assert_eq!(
            a.submit(q, WORK, 0, move |canceled| {
                record(&d1.work, canceled);
                let d2 = d1.clone();
                assert_eq!(api().submit(d1.q.load(SeqCst), COMPLETION, 0, move |c| record(&d2.completion, c)), 0);
            }),
            0
        );
        let d1 = d.clone();
        assert_eq!(a.submit(q, WORK, 300, move |c| record(&d1.future, c)), 0);
        let d1 = d.clone();
        a.register_waiter(q, WORK, ev.0, move |c| record(&d1.waiter, c)).unwrap();
    };
    submit_all(&data, &wait);
    submit_all(&comp, &cwait);
    let c1 = comp.clone();
    assert_eq!(
        a.terminate(
            cq,
            false,
            Some(Box::new(move || {
                c1.terminated.store(true, SeqCst);
                api().close(c1.q.load(SeqCst));
            }))
        ),
        0
    );
    while a.dispatch(q, WORK, 500) | a.dispatch(q, COMPLETION, 500) {}

    assert_eq!(
        (data.work.load(SeqCst), data.completion.load(SeqCst), data.future.load(SeqCst), data.waiter.load(SeqCst)),
        (1, 1, 1, 0),
        "base queue callbacks must run normally"
    );
    assert_eq!(
        (comp.work.load(SeqCst), comp.completion.load(SeqCst), comp.future.load(SeqCst), comp.waiter.load(SeqCst)),
        (2, 2, 2, 2),
        "composite callbacks must all run canceled"
    );
    assert!(comp.terminated.load(SeqCst));

    data.work.store(0, SeqCst);
    data.waiter.store(0, SeqCst);
    comp.waiter.store(0, SeqCst);
    let d1 = data.clone();
    assert_eq!(a.submit(q, WORK, 0, move |c| record(&d1.work, c)), 0);
    wait.set();
    cwait.set();
    while a.dispatch(q, WORK, 500) | a.dispatch(q, COMPLETION, 500) {}
    assert_eq!(data.work.load(SeqCst), 1);
    assert_eq!(data.waiter.load(SeqCst), 1, "the base queue's waiter must survive the composite's termination");
    assert_eq!(comp.waiter.load(SeqCst), 0, "the terminated composite's waiter must be gone");
    a.close(q);
}

pub fn verify_termination_of_base_queue_keeps_composites() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let (w, c) = (a.port(q, WORK), a.port(q, COMPLETION));
    let comp = a.composite(w, c);
    let slots: Arc<[AtomicI32; 3]> = Arc::new(Default::default());
    let s = slots.clone();
    assert_eq!(a.submit(comp, WORK, 0, move |cn| record(&s[0], cn)), 0);
    let s = slots.clone();
    assert_eq!(a.submit(q, WORK, 0, move |cn| record(&s[1], cn)), 0);
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    assert_eq!(a.terminate(q, false, Some(Box::new(move || d.store(true, SeqCst)))), 0);
    let delivered = until(2000, || {
        a.dispatch(q, WORK, 0);
        a.dispatch(q, COMPLETION, 0);
        done.load(SeqCst)
    });
    assert!(delivered, "the termination callback was not delivered");
    assert_eq!(slots[0].load(SeqCst), 1, "the composite's pending callback must run normally");
    assert_eq!(slots[1].load(SeqCst), 2, "the terminated queue's pending callback must run canceled");
    assert_eq!(a.submit(q, WORK, 0, |_| {}), E_ABORT);
    let s = slots.clone();
    assert_eq!(a.submit(comp, WORK, 0, move |cn| record(&s[2], cn)), 0);
    assert_eq!(a.submit(comp, COMPLETION, 0, |_| {}), 0);
    assert_eq!(a.submit(comp, WORK, 10, |_| {}), 0);
    let mut comp2 = 0;
    assert_eq!(a.try_composite(w, c, &mut comp2), 0);
    assert!(until(2000, || {
        a.dispatch(comp, WORK, 0);
        a.dispatch(comp, COMPLETION, 0);
        slots[2].load(SeqCst) != 0
    }));
    assert_eq!(slots[2].load(SeqCst), 1, "a callback queued on the composite afterwards must run normally");
    std::thread::sleep(Duration::from_millis(30));
    drain(comp);
    a.close(comp2);
    a.close(comp);
    a.close(q);
}

pub fn verify_queue_monitor_has_correct_ports() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let wp = a.port(q, WORK);
    let cq = a.composite(wp, wp);
    let qp = Arc::new(AtomicU32::new(u32::MAX));
    let cp = Arc::new(AtomicU32::new(u32::MAX));
    let (q1, c1) = (qp.clone(), cp.clone());
    a.register_monitor(q, move |_, port| q1.store(port, SeqCst));
    a.register_monitor(cq, move |_, port| c1.store(port, SeqCst));
    assert_eq!(a.submit(cq, COMPLETION, 0, |_| {}), 0);
    while a.dispatch(cq, COMPLETION, 100) {}
    assert_eq!(qp.load(SeqCst), WORK, "base queue monitor must report its own port for the shared port");
    assert_eq!(cp.load(SeqCst), COMPLETION);
    a.close(cq);
    a.close(q);
}

pub fn verify_nested_queue_dispatch_order() {
    let a = api();
    let nest = |q: usize| {
        let p = a.port(q, WORK);
        a.composite(p, p)
    };
    let q = a.create(MANUAL, MANUAL);
    let outer = nest(q);
    let inner = nest(outer);
    let step = Arc::new(Mutex::new(Vec::new()));
    let s = step.clone();
    assert_eq!(a.submit(outer, WORK, 3000, move |_| s.lock().unwrap().push("delayed outer")), 0);
    let s = step.clone();
    assert_eq!(a.submit(inner, WORK, 0, move |_| s.lock().unwrap().push("inner")), 0);
    a.dispatch(outer, WORK, 0);
    a.dispatch(outer, COMPLETION, 0);
    assert_eq!(*step.lock().unwrap(), ["inner"]);
    a.close(inner);
    a.dispatch(outer, WORK, 0);
    a.dispatch(outer, COMPLETION, 0);
    assert_eq!(*step.lock().unwrap(), ["inner"]);
    a.dispatch(outer, WORK, 4000);
    a.dispatch(outer, COMPLETION, 1000);
    assert_eq!(*step.lock().unwrap(), ["inner", "delayed outer"]);
    a.close(outer);
    a.close(q);
}

pub fn verify_terminate_waits_for_async_in_flight() {
    let a = api();
    let q = a.create(THREAD_POOL, THREAD_POOL);
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, 0, 0, nop_provider), 0);
    let done = Arc::new(Event::new(true));
    let d = done.clone();
    assert_eq!(a.terminate(q, false, Some(Box::new(move || d.set()))), 0);
    assert!(!done.wait(1000), "termination finished while an XAsync call on the queue was still pending");
    a.complete(&mut *b, 0, 0);
    assert!(done.wait(2000), "termination did not finish after the pending XAsync call completed");
    assert_eq!(a.status(&mut *b, true), 0);
    a.close(q);
}

pub fn verify_single_queue_delayed_poll_strand() {
    let a = api();
    for iter in 0..10 {
        let q = a.create(MANUAL, MANUAL);
        let pumping = Arc::new(AtomicBool::new(true));
        let p = pumping.clone();
        let pump = std::thread::spawn(move || {
            while p.load(SeqCst) {
                api().dispatch(q, WORK, 50);
            }
        });
        let keep = Arc::new(AtomicBool::new(true));
        let polls = counter();
        fn poll(q: usize, keep: Arc<AtomicBool>, polls: Arc<AtomicU32>) -> impl FnOnce(bool) + Send {
            move |canceled| {
                if canceled {
                    return;
                }
                polls.fetch_add(1, SeqCst);
                if keep.load(SeqCst) {
                    let (k, p) = (keep.clone(), polls.clone());
                    api().submit(q, WORK, 2, poll(q, k, p));
                }
            }
        }
        assert_eq!(a.submit(q, WORK, 2, poll(q, keep.clone(), polls.clone())), 0);
        assert!(until(1000, || polls.load(SeqCst) > 0));
        let outstanding = Arc::new(AtomicI32::new(0));
        let burst: Vec<_> = (0..4)
            .map(|_| {
                let o = outstanding.clone();
                std::thread::spawn(move || {
                    for _ in 0..250 {
                        o.fetch_add(1, SeqCst);
                        let o2 = o.clone();
                        if api().submit(q, WORK, 1, move |_| {
                            o2.fetch_sub(1, SeqCst);
                        }) < 0
                        {
                            o.fetch_sub(1, SeqCst);
                        }
                    }
                })
            })
            .collect();
        for t in burst {
            t.join().unwrap();
        }
        assert!(until(5000, || outstanding.load(SeqCst) == 0), "iteration {iter}: burst callbacks did not drain");
        let target = polls.load(SeqCst) + 30;
        let progressed = until(5000, || polls.load(SeqCst) >= target);
        keep.store(false, SeqCst);
        pumping.store(false, SeqCst);
        pump.join().unwrap();
        a.terminate(q, false, None);
        drain(q);
        a.close(q);
        assert!(progressed, "iteration {iter}: delayed poll loop stranded at {} polls", polls.load(SeqCst));
    }
}

pub fn verify_composite_queue_delayed_callback_starvation() {
    struct Req {
        comp: usize,
        target: u32,
        delay: u32,
        polls: AtomicU32,
        done: AtomicBool,
        term: AtomicBool,
        completed: Arc<AtomicU32>,
    }
    impl Req {
        fn mark(&self) {
            if !self.done.swap(true, SeqCst) {
                self.completed.fetch_add(1, SeqCst);
            }
        }
        fn terminate(&self) {
            if !self.term.swap(true, SeqCst) {
                api().terminate(self.comp, false, None);
            }
        }
    }
    unsafe extern "system" fn poll(ctx: *mut c_void, canceled: bool) {
        let r = &*(ctx as *const Req);
        if canceled {
            return r.mark();
        }
        let n = r.polls.fetch_add(1, SeqCst) + 1;
        if n < r.target {
            if api().submit_raw(r.comp, WORK, r.delay, ctx, poll) < 0 {
                r.mark();
            }
        } else {
            r.terminate();
            r.mark();
        }
    }
    let a = api();
    let (threads, per) = (6usize, 24usize);
    let total = threads * per;
    for iter in 0..5 {
        let q = a.create(MANUAL, MANUAL);
        let wp = a.port(q, WORK);
        let pumping = Arc::new(AtomicBool::new(true));
        let pumps: Vec<_> = (0..2)
            .map(|_| {
                let p = pumping.clone();
                std::thread::spawn(move || {
                    while p.load(SeqCst) {
                        api().dispatch(q, WORK, 50);
                    }
                })
            })
            .collect();
        let completed = Arc::new(AtomicU32::new(0));
        let slots: Arc<Vec<AtomicUsize>> = Arc::new((0..total).map(|_| AtomicUsize::new(0)).collect());
        let submitters: Vec<_> = (0..threads)
            .map(|t| {
                let (c, s) = (completed.clone(), slots.clone());
                std::thread::spawn(move || {
                    let a = api();
                    for i in 0..per {
                        let idx = t * per + i;
                        let r = Box::into_raw(Box::new(Req {
                            comp: a.composite(wp, wp),
                            target: if idx % 3 == 0 { u32::MAX } else { 20 },
                            delay: if i & 1 == 1 { 1 } else { 2 },
                            polls: AtomicU32::new(0),
                            done: AtomicBool::new(false),
                            term: AtomicBool::new(false),
                            completed: c.clone(),
                        }));
                        s[idx].store(r as usize, SeqCst);
                        if a.submit_raw(unsafe { (*r).comp }, WORK, 0, r as *mut c_void, poll) < 0 {
                            unsafe { (*r).mark() };
                        }
                    }
                })
            })
            .collect();
        let s = slots.clone();
        let canceller = std::thread::spawn(move || loop {
            let mut handled = 0;
            for i in 0..total {
                if i % 3 != 0 {
                    handled += 1;
                    continue;
                }
                let r = s[i].load(SeqCst) as *const Req;
                if !r.is_null() {
                    unsafe { (*r).terminate() };
                    handled += 1;
                }
            }
            if handled == total {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        });
        for t in submitters {
            t.join().unwrap();
        }
        canceller.join().unwrap();
        let ok = until(15000, || completed.load(SeqCst) as usize == total);
        pumping.store(false, SeqCst);
        for p in pumps {
            p.join().unwrap();
        }
        for slot in slots.iter() {
            let r = slot.load(SeqCst) as *const Req;
            unsafe {
                (*r).terminate();
                a.close((*r).comp);
            }
        }
        let fin = Arc::new(AtomicBool::new(false));
        let f = fin.clone();
        a.terminate(q, false, Some(Box::new(move || f.store(true, SeqCst))));
        let end = Instant::now() + Duration::from_secs(5);
        while !fin.load(SeqCst) && Instant::now() < end {
            a.dispatch(q, WORK, 10);
            a.dispatch(q, COMPLETION, 0);
        }
        assert!(fin.load(SeqCst), "iteration {iter}: queue termination stalled");
        a.close(q);
        assert!(ok, "iteration {iter}: starvation, {} of {total} poll loops finished", completed.load(SeqCst));
    }
}

pub fn verify_termination_cancels_delayed_callbacks() {
    let a = api();
    for mode in [THREAD_POOL, MANUAL] {
        let q = a.create(mode, mode);
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        assert_eq!(a.submit(q, WORK, 10000, move |canceled| l.lock().unwrap().push(if canceled { "delayed canceled" } else { "delayed ran" })), 0);
        let l = log.clone();
        assert_eq!(a.terminate(q, false, Some(Box::new(move || l.lock().unwrap().push("terminated")))), 0);
        let end = Instant::now() + Duration::from_secs(2);
        while !log.lock().unwrap().contains(&"terminated") && Instant::now() < end {
            if mode == MANUAL {
                a.dispatch(q, WORK, 10);
                a.dispatch(q, COMPLETION, 10);
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert_eq!(*log.lock().unwrap(), ["delayed canceled", "terminated"], "mode {mode}");
        a.close(q);
    }
}

pub fn verify_termination_order_on_a_manual_queue() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let phase = Arc::new(AtomicU32::new(0));
    let note = |name: &'static str| {
        let (l, p) = (log.clone(), phase.clone());
        move |canceled: bool| l.lock().unwrap().push(format!("{name} {} in {}", if canceled { "canceled" } else { "ran" }, ["no dispatch", "work dispatch", "completion dispatch"][p.load(SeqCst) as usize]))
    };
    assert_eq!(a.submit(q, WORK, 10000, note("delayed work")), 0);
    assert_eq!(a.submit(q, COMPLETION, 10000, note("delayed completion")), 0);
    assert_eq!(a.submit(q, WORK, 0, note("queued work")), 0);
    assert_eq!(a.submit(q, COMPLETION, 0, note("queued completion")), 0);
    let (l, p) = (log.clone(), phase.clone());
    assert_eq!(a.terminate(q, false, Some(Box::new(move || l.lock().unwrap().push(format!("terminated in {}", ["no dispatch", "work dispatch", "completion dispatch"][p.load(SeqCst) as usize]))))), 0);
    let after_terminate = (a.submit(q, WORK, 0, note("late work")), a.submit(q, COMPLETION, 0, note("late completion")));
    let before_dispatch = log.lock().unwrap().clone();
    phase.store(1, SeqCst);
    while a.dispatch(q, WORK, 0) {}
    phase.store(0, SeqCst);
    let after_work = (a.submit(q, WORK, 0, note("later work")), a.submit(q, COMPLETION, 0, note("later completion")));
    let end = Instant::now() + Duration::from_secs(2);
    while !log.lock().unwrap().iter().any(|e| e.starts_with("terminated")) && Instant::now() < end {
        phase.store(2, SeqCst);
        while a.dispatch(q, COMPLETION, 0) {}
        phase.store(1, SeqCst);
        while a.dispatch(q, WORK, 0) {}
        phase.store(0, SeqCst);
    }
    let at_end = (a.submit(q, WORK, 0, note("last work")), a.submit(q, COMPLETION, 0, note("last completion")));
    assert_eq!(after_terminate, (E_ABORT, 0), "right after XTaskQueueTerminate the work port refuses and the completion port still accepts");
    assert!(before_dispatch.is_empty(), "nothing is canceled before its port is dispatched: {before_dispatch:?}");
    assert_eq!(after_work, (E_ABORT, E_ABORT), "once the work port has drained, the completion port refuses too");
    assert_eq!(at_end, (E_ABORT, E_ABORT), "a terminated queue takes no new work");
    assert_eq!(
        *log.lock().unwrap(),
        [
            "queued work canceled in work dispatch",
            "delayed work canceled in work dispatch",
            "queued completion canceled in completion dispatch",
            "late completion canceled in completion dispatch",
            "delayed completion canceled in completion dispatch",
            "terminated in completion dispatch",
        ]
    );
    a.close(q);
}

pub fn verify_close_keeps_queued_callbacks() {
    let a = api();
    let q = a.create(SERIALIZED, SERIALIZED);
    let gate = Arc::new(Event::new(true));
    let (ran, canceled) = (counter(), counter());
    let g = gate.clone();
    assert_eq!(a.submit(q, WORK, 0, move |_| { g.wait(5000); }), 0);
    for delay in std::iter::repeat(0).take(20).chain([200]) {
        let (r, c) = (ran.clone(), canceled.clone());
        assert_eq!(
            a.submit(q, WORK, delay, move |cx| {
                if cx { c.fetch_add(1, SeqCst) } else { r.fetch_add(1, SeqCst) };
            }),
            0
        );
    }
    a.close(q);
    gate.set();
    assert!(until(3000, || ran.load(SeqCst) + canceled.load(SeqCst) == 21));
    assert_eq!((ran.load(SeqCst), canceled.load(SeqCst)), (21, 0), "closing the handle canceled queued callbacks");
}

pub fn verify_dispatch_on_pool_port() {
    let a = api();
    let q = a.create(MANUAL, SERIALIZED);
    let gate = Arc::new(Event::new(true));
    let started = Arc::new(Event::new(true));
    let (g, s) = (gate.clone(), started.clone());
    assert_eq!(
        a.submit(q, COMPLETION, 0, move |_| {
            s.set();
            g.wait(5000);
        }),
        0
    );
    assert!(started.wait(2000));
    let ran_on = Arc::new(Mutex::new(None));
    let r = ran_on.clone();
    assert_eq!(a.submit(q, COMPLETION, 0, move |_| *r.lock().unwrap() = Some(std::thread::current().id())), 0);
    assert!(a.dispatch(q, COMPLETION, 0), "dispatch on a thread-pool port with a queued item");
    assert_eq!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
    gate.set();
    a.close(q);
}

pub fn verify_thread_pool_callbacks_can_block() {
    let a = api();
    let q = a.create(THREAD_POOL, THREAD_POOL);
    const N: u32 = 24;
    let pair = Arc::new((Mutex::new(0u32), std::sync::Condvar::new()));
    let met = counter();
    for _ in 0..N {
        let (p, m) = (pair.clone(), met.clone());
        assert_eq!(
            a.submit(q, WORK, 0, move |_| {
                let (lock, cv) = &*p;
                let mut n = lock.lock().unwrap();
                *n += 1;
                cv.notify_all();
                let (n, _) = cv.wait_timeout_while(n, Duration::from_secs(10), |n| *n < N).unwrap();
                if *n >= N {
                    m.fetch_add(1, SeqCst);
                }
            }),
            0
        );
    }
    assert!(until(12000, || met.load(SeqCst) == N), "{} thread-pool callbacks could not all run at once", N);
    a.close(q);
}

#[repr(C)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    tid: u32,
    pid: u32,
    base: i32,
    delta: i32,
    flags: u32,
}

extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> usize;
    fn Thread32First(snap: usize, e: *mut ThreadEntry) -> i32;
    fn Thread32Next(snap: usize, e: *mut ThreadEntry) -> i32;
    fn GetCurrentProcessId() -> u32;
}

fn thread_count() -> usize {
    unsafe {
        let snap = CreateToolhelp32Snapshot(4, 0);
        let mut e = ThreadEntry { size: 28, usage: 0, tid: 0, pid: 0, base: 0, delta: 0, flags: 0 };
        let me = GetCurrentProcessId();
        let mut n = 0;
        let mut ok = Thread32First(snap, &mut e);
        while ok != 0 {
            n += (e.pid == me) as usize;
            ok = Thread32Next(snap, &mut e);
        }
        CloseHandle(snap);
        n
    }
}

pub fn verify_serialized_queues_do_not_keep_threads() {
    let a = api();
    let before = thread_count();
    let ran = counter();
    for _ in 0..256 {
        let q = a.create(SERIALIZED, SERIALIZED);
        assert_eq!(a.submit(q, WORK, 0, bump(&ran)), 0);
        assert_eq!(a.submit(q, COMPLETION, 0, bump(&ran)), 0);
        a.close(q);
    }
    assert!(until(5000, || ran.load(SeqCst) == 512));
    std::thread::sleep(Duration::from_millis(200));
    let after = thread_count();
    assert!(after < before + 192, "threads went from {before} to {after} for 256 closed serialized queues");
}


pub fn verify_serialized_port_final_release() {
    let a = api();
    let before = thread_count();
    for _ in 0..64 {
        let q = a.create(SERIALIZED, SERIALIZED);
        let ev = Arc::new(Event::new(true));
        let e = ev.clone();
        assert_eq!(a.submit(q, WORK, 0, move |canceled| if !canceled { e.set() }), 0);
        assert!(ev.wait(5000), "serialized callback never ran");
        let d = a.dup(q);
        a.close(q);
        a.close(d);
    }
    std::thread::sleep(Duration::from_millis(200));
    let after = thread_count();
    assert!(after < before + 48, "threads went from {before} to {after} after releasing 64 serialized queues");
}

pub fn verify_serialized_port_survives_one_release() {
    let a = api();
    let q = a.create(SERIALIZED, SERIALIZED);
    let d = a.dup(q);
    let ev = Arc::new(Event::new(true));
    let e = ev.clone();
    assert_eq!(a.submit(q, WORK, 0, move |canceled| if !canceled { e.set() }), 0);
    assert!(ev.wait(5000));
    a.close(q);
    ev.reset();
    let got = Arc::new(Mutex::new(None));
    let (e, g) = (ev.clone(), got.clone());
    assert_eq!(a.submit(d, WORK, 0, move |canceled| { *g.lock().unwrap() = Some(canceled); e.set() }), 0);
    assert!(ev.wait(5000), "the remaining handle's serialized port stopped running");
    assert_eq!(*got.lock().unwrap(), Some(false), "a callback through the remaining handle was canceled");
    a.close(d);
}

pub fn verify_serialized_idle_port_delivers_termination() {
    let a = api();
    let q = a.create(SERIALIZED, SERIALIZED);
    let ev = Arc::new(Event::new(true));
    let e = ev.clone();
    assert_eq!(a.submit(q, COMPLETION, 0, move |_| e.set()), 0);
    assert!(ev.wait(5000));
    std::thread::sleep(Duration::from_millis(50));
    let done = Arc::new(Event::new(true));
    let dn = done.clone();
    assert_eq!(a.terminate(q, false, Some(Box::new(move || dn.set()))), 0);
    assert!(done.wait(5000), "termination callback of an idle serialized queue never ran");
    a.close(q);
}

pub fn verify_serialized_termination_during_callback() {
    let a = api();
    let q = a.create(SERIALIZED, SERIALIZED);
    let order = Arc::new(Mutex::new(Vec::new()));
    let (started, gate) = (Arc::new(Event::new(true)), Arc::new(Event::new(true)));
    let (s, g, o) = (started.clone(), gate.clone(), order.clone());
    assert_eq!(a.submit(q, WORK, 0, move |_| { s.set(); g.wait(5000); o.lock().unwrap().push("first") }), 0);
    assert!(started.wait(5000));
    let o = order.clone();
    assert_eq!(a.submit(q, WORK, 0, move |canceled| o.lock().unwrap().push(if canceled { "second canceled" } else { "second ran" })), 0);
    let done = Arc::new(Event::new(true));
    let (dn, o) = (done.clone(), order.clone());
    assert_eq!(a.terminate(q, false, Some(Box::new(move || { o.lock().unwrap().push("terminated"); dn.set() }))), 0);
    assert!(!done.wait(100), "termination completed while a callback was still running");
    gate.set();
    assert!(done.wait(5000), "termination callback never ran");
    let order = order.lock().unwrap().clone();
    assert_eq!(order.len(), 3, "{order:?}");
    assert!(order.contains(&"first") && order.contains(&"second canceled"), "{order:?}");
    assert_eq!(order[2], "terminated", "the termination callback ran before the port's callbacks: {order:?}");
    a.close(q);
}


const FACTORIAL_ID: usize = 0xFAC7;

#[derive(Default)]
pub struct Factorial {
    pub value: AtomicU32,
    pub result: AtomicU32,
    pub wait_ms: u32,
    pub work_thread: Mutex<Option<std::thread::ThreadId>>,
    pub ops: Mutex<Vec<u32>>,
    pub in_work: AtomicI32,
    pub canceled: AtomicBool,
    pub cleanup_in_work: AtomicBool,
}

impl Factorial {
    pub fn new(value: u32, wait_ms: u32) -> Arc<Factorial> {
        Arc::new(Factorial { value: AtomicU32::new(value), wait_ms, ..Default::default() })
    }
    fn ops(&self) -> Vec<u32> {
        self.ops.lock().unwrap().clone()
    }
    fn common(&self, op: u32, d: *const Data) -> Option<i32> {
        self.ops.lock().unwrap().push(op);
        match op {
            OP_CANCEL => self.canceled.store(true, SeqCst),
            OP_CLEANUP => {
                if self.in_work.load(SeqCst) != 0 {
                    self.cleanup_in_work.store(true, SeqCst);
                }
            }
            OP_GET_RESULT => unsafe { ((*d).buffer as *mut u32).write_unaligned(self.result.load(SeqCst)) },
            OP_DO_WORK => return None,
            _ => {}
        }
        Some(0)
    }
}

unsafe extern "system" fn factorial_simple(op: u32, d: *const Data) -> i32 {
    let f = &*((*d).context as *const Factorial);
    if let Some(hr) = f.common(op, d) {
        return hr;
    }
    f.in_work.fetch_add(1, SeqCst);
    *f.work_thread.lock().unwrap() = Some(std::thread::current().id());
    let mut r = 1u32;
    let mut v = f.value.load(SeqCst);
    while v != 0 {
        r = r.wrapping_mul(v);
        v -= 1;
    }
    f.result.store(r, SeqCst);
    if f.wait_ms != 0 {
        std::thread::sleep(Duration::from_millis(f.wait_ms as u64));
    }
    f.in_work.fetch_sub(1, SeqCst);
    api().complete((*d).block, if f.canceled.load(SeqCst) { E_ABORT } else { 0 }, 4);
    0
}

unsafe extern "system" fn factorial_distributed(op: u32, d: *const Data) -> i32 {
    let f = &*((*d).context as *const Factorial);
    if let Some(hr) = f.common(op, d) {
        return hr;
    }
    f.in_work.fetch_add(1, SeqCst);
    *f.work_thread.lock().unwrap() = Some(std::thread::current().id());
    if f.result.load(SeqCst) == 0 {
        f.result.store(1, SeqCst);
    }
    let v = f.value.load(SeqCst);
    if v != 0 {
        if f.canceled.load(SeqCst) {
            f.in_work.fetch_sub(1, SeqCst);
            return E_ABORT;
        }
        f.result.store(f.result.load(SeqCst) * v, SeqCst);
        f.value.store(v - 1, SeqCst);
        let hr = api().schedule((*d).block, f.wait_ms);
        f.in_work.fetch_sub(1, SeqCst);
        return if hr >= 0 { E_PENDING } else { hr };
    }
    f.in_work.fetch_sub(1, SeqCst);
    api().complete((*d).block, 0, 4);
    0
}

fn factorial_async(f: &Arc<Factorial>, b: &mut Block, distributed: bool) -> i32 {
    let a = api();
    let p: Provider = if distributed { factorial_distributed } else { factorial_simple };
    let hr = a.begin(b, Arc::as_ptr(f) as usize, FACTORIAL_ID, p);
    if hr < 0 {
        return hr;
    }
    a.schedule(b, 0)
}

fn factorial_result(b: *mut Block, out: &mut u32) -> i32 {
    let mut used = 0usize;
    let hr = api().result(b, FACTORIAL_ID, 4, out as *mut u32 as *mut c_void, &mut used);
    if hr >= 0 {
        assert_eq!(used, 4);
    }
    hr
}

type Done = Box<dyn Fn(*mut Block) + Send + Sync>;
unsafe extern "system" fn done_tramp(b: *mut Block) {
    (*((*b).context as *const Done))(b)
}
fn on_done(b: &mut Block, f: impl Fn(*mut Block) + Send + Sync + 'static) {
    b.context = leak::<Done>(Box::new(f)) as usize;
    b.callback = Some(done_tramp);
}

unsafe extern "system" fn nop_provider(op: u32, d: *const Data) -> i32 {
    let _ = (op, d);
    0
}

unsafe extern "system" fn abort_on_cancel(op: u32, d: *const Data) -> i32 {
    if op == OP_CANCEL {
        api().complete((*d).block, E_ABORT, 0);
    }
    0
}

fn thread_pool_queue() -> usize {
    api().create(THREAD_POOL, THREAD_POOL)
}

fn settle(q: usize) {
    let _ = q;
    std::thread::sleep(Duration::from_millis(50));
}

pub fn verify_simple_async_call() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    let result = Arc::new(AtomicI32::new(-1));
    let r = result.clone();
    on_done(&mut b, move |b| {
        let mut v = 0;
        let hr = factorial_result(b, &mut v);
        r.store(if hr >= 0 { v as i32 } else { hr }, SeqCst);
    });
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    assert_eq!(result.load(SeqCst), 120, "XAsyncGetStatus(wait) returned before the completion callback ran");
    settle(q);
    assert_eq!(f.ops(), vec![OP_BEGIN, OP_DO_WORK, OP_GET_RESULT, OP_CLEANUP]);
    a.close(q);
}

pub fn verify_multiple_calls() {
    let a = api();
    let q = thread_pool_queue();
    let done = counter();
    let mut blocks = Vec::new();
    for i in 0..10u32 {
        let f = Factorial::new(5 * (i + 1), 0);
        let mut b = Block::new(q);
        let (d, f2) = (done.clone(), f.clone());
        on_done(&mut b, move |b| {
            let mut v = 0;
            if factorial_result(b, &mut v) >= 0 && v == f2.result.load(SeqCst) {
                d.fetch_add(1, SeqCst);
            }
        });
        assert_eq!(factorial_async(&f, &mut b, false), 0);
        blocks.push((b, f));
    }
    assert!(until(5000, || done.load(SeqCst) == 10));
    a.close(q);
}

pub fn verify_distributed_async_call() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 100);
    let mut b = Block::new(q);
    let result = Arc::new(AtomicI32::new(-1));
    let r = result.clone();
    on_done(&mut b, move |b| {
        let mut v = 0;
        let hr = factorial_result(b, &mut v);
        r.store(if hr >= 0 { v as i32 } else { hr }, SeqCst);
    });
    let start = Instant::now();
    assert_eq!(factorial_async(&f, &mut b, true), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    let took = start.elapsed();
    assert_eq!(result.load(SeqCst), 120);
    assert!(took >= Duration::from_millis(400), "rescheduled DoWork ignored its delay ({took:?})");
    settle(q);
    let mut ops = vec![OP_BEGIN];
    ops.extend([OP_DO_WORK; 6]);
    ops.extend([OP_GET_RESULT, OP_CLEANUP]);
    assert_eq!(f.ops(), ops);
    a.close(q);
}

pub fn verify_async_block_reuse() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 0);
    let mut b = Box::new(Block { queue: q, context: 0, callback: None, internal: [0xCD; 32] });
    let result = Arc::new(AtomicI32::new(-1));
    let r = result.clone();
    on_done(&mut b, move |b| {
        let mut v = 0;
        let hr = factorial_result(b, &mut v);
        r.store(if hr >= 0 { v as i32 } else { hr }, SeqCst);
    });
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    assert_eq!(result.load(SeqCst), 120);
    b.callback = None;
    f.value.store(6, SeqCst);
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(factorial_async(&f, &mut b, false), E_INVALIDARG, "an async block in use must not be reusable");
    assert_eq!(a.status(&mut *b, true), 0);
    let mut v = 0;
    assert_eq!(factorial_result(&mut *b, &mut v), 0);
    assert_eq!(v, 720);
    a.close(q);
}

pub fn verify_cancellation() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 100);
    let mut b = Block::new(q);
    let in_cb = Arc::new(AtomicI32::new(1));
    let c = in_cb.clone();
    on_done(&mut b, move |b| c.store(api().status(b, false), SeqCst));
    assert_eq!(factorial_async(&f, &mut b, true), 0);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(a.status(&mut *b, false), E_PENDING);
    a.cancel(&mut *b);
    assert_eq!(a.status(&mut *b, true), E_ABORT);
    assert_eq!(in_cb.load(SeqCst), E_ABORT);
    settle(q);
    let ops = f.ops();
    assert!(ops.contains(&OP_CANCEL) && ops.contains(&OP_CLEANUP), "{ops:?}");
    a.close(q);
}

fn cleanup_waits_for_work(distributed: bool) {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 500);
    let mut b = Block::new(q);
    let in_cb = Arc::new(AtomicI32::new(1));
    let c = in_cb.clone();
    on_done(&mut b, move |b| {
        let mut v = 0;
        c.store(factorial_result(b, &mut v), SeqCst)
    });
    assert_eq!(factorial_async(&f, &mut b, distributed), 0);
    std::thread::sleep(Duration::from_millis(if distributed { 700 } else { 50 }));
    assert_eq!(a.status(&mut *b, false), E_PENDING);
    a.cancel(&mut *b);
    assert_eq!(a.status(&mut *b, true), E_ABORT);
    assert_eq!(in_cb.load(SeqCst), E_ABORT);
    assert!(until(2000, || f.ops().contains(&OP_CLEANUP)));
    let ops = f.ops();
    assert!(ops.contains(&OP_CANCEL) && ops.contains(&OP_DO_WORK), "{ops:?}");
    assert!(!f.cleanup_in_work.load(SeqCst), "XAsyncOp::Cleanup ran while DoWork was still running: {ops:?}");
    a.close(q);
}

pub fn verify_cleanup_waits_for_work() {
    cleanup_waits_for_work(false);
}

pub fn verify_cleanup_waits_for_work_distributed() {
    cleanup_waits_for_work(true);
}

static RUN_EXPECTED: AtomicI32 = AtomicI32::new(0);
unsafe extern "system" fn run_work(_b: *mut Block) -> i32 {
    RUN_EXPECTED.load(SeqCst)
}

pub fn verify_run_async() {
    let a = api();
    let q = thread_pool_queue();
    RUN_EXPECTED.store(0x12345678, SeqCst);
    let mut b = Block::new(q);
    assert_eq!(a.run(&mut *b, run_work), 0);
    assert_eq!(a.status(&mut *b, true), 0x12345678);
    a.close(q);
}

pub fn verify_custom_queue() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    let seen = Arc::new(Mutex::new(None));
    let s = seen.clone();
    on_done(&mut b, move |b| {
        let mut v = 0;
        let hr = factorial_result(b, &mut v);
        *s.lock().unwrap() = Some((hr, v, std::thread::current().id()));
    });
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert!(a.dispatch(q, WORK, 100), "XAsyncSchedule did not post DoWork to the work port");
    assert_eq!(f.result.load(SeqCst), 120);
    assert_eq!(*f.work_thread.lock().unwrap(), Some(std::thread::current().id()));
    assert!(seen.lock().unwrap().is_none(), "completion ran before its port was dispatched");
    assert!(a.dispatch(q, COMPLETION, 100), "completion was not posted to the completion port");
    assert_eq!(*seen.lock().unwrap(), Some((0, 120, std::thread::current().id())));
    assert!(!a.dispatch(q, WORK, 0) && !a.dispatch(q, COMPLETION, 0));
    a.close(q);
}

pub fn verify_cant_schedule_twice() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, Arc::as_ptr(&f) as usize, 0, factorial_simple), 0);
    assert_eq!(a.schedule(&mut *b, 0), 0);
    assert_eq!(a.schedule(&mut *b, 0), E_UNEXPECTED);
    a.cancel(&mut *b);
    drain(q);
    assert_ne!(a.status(&mut *b, false), E_PENDING);
    a.close(q);
}

pub fn verify_wait_for_completion() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    let finished = Arc::new(AtomicBool::new(false));
    let fin = finished.clone();
    on_done(&mut b, move |b| {
        std::thread::sleep(Duration::from_millis(300));
        let mut v = 0;
        if factorial_result(b, &mut v) >= 0 && v == 120 {
            fin.store(true, SeqCst);
        }
    });
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    assert!(finished.load(SeqCst), "XAsyncGetStatus(wait) must wait for the completion callback to finish");
    a.close(q);
}

pub fn verify_periodic_pattern() {
    let a = api();
    let q = thread_pool_queue();
    struct Ctl {
        q: usize,
        enabled: AtomicBool,
        count: AtomicU32,
    }
    unsafe extern "system" fn provider(op: u32, d: *const Data) -> i32 {
        if op == OP_DO_WORK {
            api().complete((*d).block, 0, 0);
        }
        0
    }
    unsafe extern "system" fn done(b: *mut Block) {
        let c = &*((*b).context as *const Ctl);
        c.count.fetch_add(1, SeqCst);
        if c.enabled.load(SeqCst) {
            schedule(c);
        }
        drop(Box::from_raw(b));
    }
    fn schedule(c: &Ctl) {
        let b = Box::into_raw(Box::new(Block { queue: c.q, context: c as *const Ctl as usize, callback: Some(done), internal: [0; 32] }));
        assert_eq!(api().begin(b, 0, 0, provider), 0);
        assert_eq!(api().schedule(b, 30), 0);
    }
    let c = Arc::new(Ctl { q, enabled: AtomicBool::new(true), count: AtomicU32::new(0) });
    schedule(&c);
    assert!(until(10000, || c.count.load(SeqCst) >= 10));
    c.enabled.store(false, SeqCst);
    assert_eq!(a.terminate(q, true, None), 0);
    a.close(q);
}

unsafe extern "system" fn zero_work(_b: *mut Block) -> i32 {
    0
}

pub fn verify_run_alot_async() {
    let a = api();
    let q = thread_pool_queue();
    let mut blocks: Vec<Block> = (0..20000).map(|_| *Block::new(q)).collect();
    for b in blocks.iter_mut() {
        assert_eq!(a.run(b, zero_work), 0);
    }
    for b in blocks.iter_mut() {
        assert_eq!(a.status(b, true), 0);
    }
    a.close(q);
}

unsafe extern "system" fn short_work(_b: *mut Block) -> i32 {
    std::thread::sleep(Duration::from_millis(1));
    0
}

pub fn verify_get_async_status_no_deadlock() {
    let a = api();
    let q = thread_pool_queue();
    let mut b = Block::new(q);
    for _ in 0..300 {
        assert_eq!(a.run(&mut *b, short_work), 0);
        while a.status(&mut *b, false) == E_PENDING {
            std::thread::yield_now();
        }
    }
    a.close(q);
}

pub fn verify_failed_begin_completes() {
    let a = api();
    let q = thread_pool_queue();
    unsafe extern "system" fn fail(op: u32, _d: *const Data) -> i32 {
        if op == OP_BEGIN {
            E_FAIL
        } else {
            E_UNEXPECTED
        }
    }
    let mut b = Block::new(q);
    let called = Arc::new(AtomicI32::new(1));
    let c = called.clone();
    on_done(&mut b, move |b| c.store(api().status(b, false), SeqCst));
    assert_eq!(a.begin(&mut *b, 0, 0, fail), 0, "XAsyncBegin succeeds even when the Begin op fails");
    assert_eq!(a.status(&mut *b, true), E_FAIL);
    assert!(until(2000, || called.load(SeqCst) == E_FAIL), "a failed Begin still completes through the callback");
    a.close(q);
}

pub fn verify_pending_begin_stays_pending() {
    let a = api();
    let q = thread_pool_queue();
    unsafe extern "system" fn pending(op: u32, _d: *const Data) -> i32 {
        if op == OP_BEGIN {
            E_PENDING
        } else {
            0
        }
    }
    let mut b = Block::new(q);
    let called = Arc::new(AtomicI32::new(1));
    let c = called.clone();
    on_done(&mut b, move |b| c.store(api().status(b, false), SeqCst));
    let hr = a.begin(&mut *b, 0, 0, pending);
    assert!(hr == 0 || hr == E_PENDING, "XAsyncBegin with a Begin op returning E_PENDING returned {hr:#x}");
    assert_eq!(a.status(&mut *b, false), E_PENDING, "a Begin op returning E_PENDING must leave the call pending");
    assert!(!until(300, || called.load(SeqCst) != 1), "the completion callback ran for a call still pending");
    a.complete(&mut *b, 0, 0);
    assert_eq!(a.status(&mut *b, true), 0);
    assert!(until(2000, || called.load(SeqCst) == 0), "completing the pending call must run the callback");
    a.close(q);
}

const BEGIN_FAILURE: i32 = 0x8009_1234u32 as i32;

#[derive(Clone, Copy, PartialEq)]
enum Completes {
    BeginReturns,
    InsideBegin,
    FromAnotherThreadInBegin,
    InDoWork,
    InDoWorkWithPayload,
}

struct BeginCase {
    ops: [AtomicU32; 5],
    hr: i32,
    how: Completes,
}

unsafe extern "system" fn begin_case_provider(op: u32, d: *const Data) -> i32 {
    let c = &*((*d).context as *const BeginCase);
    c.ops[op as usize].fetch_add(1, SeqCst);
    match (op, c.how) {
        (OP_BEGIN, Completes::BeginReturns) => c.hr,
        (OP_BEGIN, Completes::InsideBegin) => {
            api().complete((*d).block, c.hr, 0);
            0
        }
        (OP_BEGIN, Completes::FromAnotherThreadInBegin) => {
            let (b, hr) = ((*d).block as usize, c.hr);
            std::thread::spawn(move || api().complete(b as *mut Block, hr, 0)).join().unwrap();
            0
        }
        (OP_BEGIN, _) => api().schedule((*d).block, 0),
        (OP_DO_WORK, how) => {
            api().complete((*d).block, c.hr, if how == Completes::InDoWorkWithPayload { 4 } else { 0 });
            E_PENDING
        }
        _ => 0,
    }
}

fn begin_case(hr: i32, how: Completes) -> BeginCase {
    BeginCase { ops: Default::default(), hr, how }
}

fn op_counts(c: &BeginCase) -> [u32; 5] {
    core::array::from_fn(|i| c.ops[i].load(SeqCst))
}

pub fn verify_failed_begin_reports_its_failure() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let case = begin_case(BEGIN_FAILURE, Completes::BeginReturns);
    let mut b = Block::new(q);
    let done = counter();
    let in_callback = Arc::new(AtomicI32::new(0));
    let (c, s) = (done.clone(), in_callback.clone());
    on_done(&mut b, move |b| {
        s.store(api().status(b, false), SeqCst);
        c.fetch_add(1, SeqCst);
    });
    assert_eq!(a.begin(&mut *b, &case as *const _ as usize, 0, begin_case_provider), 0, "XAsyncBegin succeeds even when the Begin op fails");
    assert_eq!(a.status(&mut *b, false), BEGIN_FAILURE, "XAsyncGetStatus right after XAsyncBegin returned");
    let mut size = 7usize;
    assert_eq!(a.result_size(&mut *b, &mut size), BEGIN_FAILURE);
    assert_eq!(done.load(SeqCst), 0, "the completion callback waits for its port");
    assert_eq!(a.schedule(&mut *b, 0), BEGIN_FAILURE, "XAsyncSchedule on a call its Begin failed");
    assert!(!a.dispatch(q, WORK, 0), "a failed Begin queues no work");
    assert!(a.dispatch(q, COMPLETION, 0), "the completion callback of a failed Begin is queued");
    assert_eq!((done.load(SeqCst), in_callback.load(SeqCst)), (1, BEGIN_FAILURE), "callback count and the status it read");
    assert!(!a.dispatch(q, COMPLETION, 0));
    assert_eq!(a.status(&mut *b, false), BEGIN_FAILURE);
    assert_eq!(a.schedule(&mut *b, 0), BEGIN_FAILURE);
    assert!(!a.dispatch(q, WORK, 0));
    assert_eq!(op_counts(&case), [1, 0, 0, 0, 1], "ops seen as [Begin, DoWork, GetResult, Cancel, Cleanup]");
    a.close(q);
}

pub fn verify_cleanup_waits_for_the_callback_when_begin_completes() {
    let a = api();
    for how in [Completes::BeginReturns, Completes::InsideBegin, Completes::FromAnotherThreadInBegin, Completes::InDoWork, Completes::InDoWorkWithPayload] {
        for hr in [BEGIN_FAILURE, 0] {
            for with_callback in [true, false] {
                if how == Completes::BeginReturns && hr == 0 {
                    continue;
                }
                let what = format!("case {} hr {hr:#x} callback {with_callback}", how as u32);
                let q = a.create(MANUAL, MANUAL);
                let case = begin_case(hr, how);
                let cp = &case as *const BeginCase as usize;
                let mut b = Block::new(q);
                let seen = Arc::new(AtomicI32::new(-1));
                let s = seen.clone();
                if with_callback {
                    on_done(&mut b, move |_| s.store(unsafe { &*(cp as *const BeginCase) }.ops[OP_CLEANUP as usize].load(SeqCst) as i32, SeqCst));
                }
                assert_eq!(a.begin(&mut *b, cp, 0, begin_case_provider), 0, "{what}");
                let in_begin = how != Completes::InDoWork && how != Completes::InDoWorkWithPayload;
                assert_eq!(a.dispatch(q, WORK, 0), !in_begin, "{what}");
                assert_eq!(a.status(&mut *b, false), hr, "{what}");
                let payload = how == Completes::InDoWorkWithPayload && hr == 0;
                let early = if payload || (in_begin && with_callback) { 0 } else { 1 };
                assert_eq!(op_counts(&case)[OP_CLEANUP as usize], early, "{what}: Cleanup ops before the completion port is dispatched");
                assert_eq!(a.dispatch(q, COMPLETION, 0), with_callback, "{what}");
                if with_callback {
                    assert_eq!(seen.load(SeqCst), early as i32, "{what}: Cleanup ops the completion callback saw");
                }
                assert_eq!(op_counts(&case)[OP_CLEANUP as usize], if payload { 0 } else { 1 }, "{what}: Cleanup ops after the completion callback");
                let works = op_counts(&case)[OP_DO_WORK as usize];
                let again = a.schedule(&mut *b, 0);
                assert_eq!(again, if hr < 0 { hr } else if payload { 0 } else { E_INVALIDARG }, "{what}: XAsyncSchedule on the completed call");
                assert_eq!(a.dispatch(q, WORK, 0), payload, "{what}");
                assert_eq!(op_counts(&case)[OP_DO_WORK as usize], works + payload as u32, "{what}");
                a.close(q);
            }
        }
    }
}

pub fn verify_zero_payload_cleans_up_fast() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    unsafe extern "system" fn empty(op: u32, d: *const Data) -> i32 {
        match op {
            OP_BEGIN => {
                assert_eq!(api().schedule((*d).block, 0), 0);
            }
            OP_DO_WORK => api().complete((*d).block, 0, 0),
            OP_CLEANUP => {
                (*((*d).context as *const AtomicU32)).fetch_add(1, SeqCst);
            }
            _ => {}
        }
        0
    }
    let cleanups = AtomicU32::new(0);
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, &cleanups as *const AtomicU32 as usize, 0, empty), 0);
    assert!(a.dispatch(q, WORK, 0));
    assert_eq!(cleanups.load(SeqCst), 1, "a zero-payload call must clean up when DoWork completes it");
    drain(q);
    assert_eq!(cleanups.load(SeqCst), 1, "cleanup ran twice");
    a.close(q);
}

pub fn verify_complete_in_begin() {
    let a = api();
    let q = thread_pool_queue();
    unsafe extern "system" fn provider(op: u32, d: *const Data) -> i32 {
        if op == OP_BEGIN {
            api().complete((*d).block, 0, 0);
        }
        0
    }
    let ev = Arc::new(Event::new(true));
    let mut b = Block::new(q);
    let e = ev.clone();
    on_done(&mut b, move |_| e.set());
    assert_eq!(a.begin(&mut *b, 0, 0, provider), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    assert!(ev.wait(2500));
    a.close(q);
}

pub fn verify_begin_after_terminate() {
    let a = api();
    let q = thread_pool_queue();
    assert_eq!(a.terminate(q, true, None), 0);
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, 0, 0, nop_provider), E_ABORT);
    a.close(q);
}

pub fn verify_failure_in_do_work() {
    let a = api();
    let q = thread_pool_queue();
    unsafe extern "system" fn special(op: u32, d: *const Data) -> i32 {
        match op {
            OP_BEGIN => api().schedule((*d).block, 0),
            OP_DO_WORK => 0x8009_ABCDu32 as i32,
            _ => 0,
        }
    }
    unsafe extern "system" fn forgets(op: u32, d: *const Data) -> i32 {
        match op {
            OP_BEGIN => api().schedule((*d).block, 0),
            _ => 0,
        }
    }
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, 0, 0, special), 0);
    assert_eq!(a.status(&mut *b, true), 0x8009_ABCDu32 as i32);
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, 0, 0, forgets), 0);
    assert_eq!(a.status(&mut *b, true), E_UNEXPECTED, "DoWork returning S_OK without XAsyncComplete");
    a.close(q);
}

pub fn verify_duplicate_result_calls_fail() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    let mut v = 0;
    assert_eq!(factorial_result(&mut *b, &mut v), 0);
    assert_eq!(factorial_result(&mut *b, &mut v), E_ILLEGAL_METHOD_CALL);
    a.close(q);
}

pub fn verify_duplicate_pending_result_calls_succeed() {
    let a = api();
    let q = thread_pool_queue();
    struct Ctx {
        complete: Event,
        waiting: Event,
    }
    unsafe extern "system" fn provider(op: u32, d: *const Data) -> i32 {
        match op {
            OP_BEGIN => api().schedule((*d).block, 0),
            OP_DO_WORK => {
                let c = &*((*(*d).block).context as *const Ctx);
                c.waiting.set();
                c.complete.wait(INFINITE);
                api().complete((*d).block, 0, 0);
                0
            }
            _ => 0,
        }
    }
    let ctx = Ctx { complete: Event::new(true), waiting: Event::new(true) };
    let mut b = Block::new(q);
    b.context = &ctx as *const Ctx as usize;
    assert_eq!(a.begin(&mut *b, 0, 0, provider), 0);
    assert!(ctx.waiting.wait(2000));
    for _ in 0..10 {
        assert_eq!(a.result(&mut *b, 0, 0, core::ptr::null_mut(), core::ptr::null_mut()), E_PENDING);
    }
    ctx.complete.set();
    assert_eq!(a.status(&mut *b, true), 0);
    assert_eq!(a.result(&mut *b, 0, 0, core::ptr::null_mut(), core::ptr::null_mut()), 0);
    assert_eq!(a.result(&mut *b, 0, 0, core::ptr::null_mut(), core::ptr::null_mut()), 0, "a zero-payload result stays readable");
    a.close(q);
}

pub fn verify_no_write_after_get_status_race() {
    let a = api();
    let q = thread_pool_queue();
    struct Ctx {
        m: Mutex<(usize, bool, bool, bool)>,
        cv: std::sync::Condvar,
    }
    unsafe extern "system" fn provider(op: u32, d: *const Data) -> i32 {
        if op == OP_CANCEL {
            let c = &*((*d).context as *const Ctx);
            c.m.lock().unwrap().1 = true;
            c.cv.notify_all();
        }
        0
    }
    let ctx = Arc::new(Ctx { m: Mutex::new((0, false, false, false)), cv: std::sync::Condvar::new() });
    let c = ctx.clone();
    let worker = std::thread::spawn(move || loop {
        let ctx = &*c;
        let block = {
            let mut g = ctx.cv.wait_while(ctx.m.lock().unwrap(), |g| !g.3 && !g.1).unwrap();
            if g.3 {
                return;
            }
            g.1 = false;
            g.0
        };
        api().complete(block as *mut Block, E_ABORT, 0);
        ctx.m.lock().unwrap().2 = true;
        ctx.cv.notify_all();
    });
    for i in 0..2000 {
        let mut b = Block::new(q);
        *ctx.m.lock().unwrap() = (&mut *b as *mut Block as usize, false, false, false);
        assert_eq!(a.begin(&mut *b, Arc::as_ptr(&ctx) as usize, 0, provider), 0);
        a.cancel(&mut *b);
        assert_eq!(a.status(&mut *b, true), E_ABORT);
        let snap = b.bytes();
        drop(ctx.cv.wait_while(ctx.m.lock().unwrap(), |g| !g.2).unwrap());
        assert_eq!(snap, b.bytes(), "iteration {i}: the async block was written after XAsyncGetStatus(wait) returned");
    }
    ctx.m.lock().unwrap().3 = true;
    ctx.cv.notify_all();
    worker.join().unwrap();
    a.close(q);
}

pub fn verify_result_buffer_checks() {
    let a = api();
    let q = thread_pool_queue();
    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    let mut size = 0usize;
    assert_eq!(a.result_size(&mut *b, &mut size), 0);
    assert_eq!(size, 4);
    let mut used = 0usize;
    assert_eq!(a.result(&mut *b, FACTORIAL_ID, 4, core::ptr::null_mut(), &mut used), E_INVALIDARG);

    let f = Factorial::new(5, 0);
    let mut b = Block::new(q);
    assert_eq!(factorial_async(&f, &mut b, false), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    let mut v = 0u16;
    assert_eq!(a.result(&mut *b, FACTORIAL_ID, 2, &mut v as *mut u16 as *mut c_void, &mut used), E_NOT_SUFFICIENT_BUFFER);

    unsafe extern "system" fn failing(op: u32, d: *const Data) -> i32 {
        match op {
            OP_BEGIN => api().schedule((*d).block, 0),
            OP_DO_WORK => {
                api().complete((*d).block, E_FAIL, 16);
                0
            }
            _ => 0,
        }
    }
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, 0, 0, failing), 0);
    assert_eq!(a.status(&mut *b, true), E_FAIL);
    let mut size = 99usize;
    assert_eq!(a.result_size(&mut *b, &mut size), E_FAIL);
    assert_eq!(size, 0);
    a.close(q);
}

pub fn verify_cancel_leaves_completion_to_the_provider() {
    let a = api();
    let q = a.create(MANUAL, MANUAL);
    let case = begin_case(0, Completes::InDoWork);
    let mut b = Block::new(q);
    let done = counter();
    let d = done.clone();
    on_done(&mut b, move |_| {
        d.fetch_add(1, SeqCst);
    });
    assert_eq!(a.begin(&mut *b, &case as *const _ as usize, 0, begin_case_provider), 0);
    a.cancel(&mut *b);
    let after_cancel = (op_counts(&case)[OP_CANCEL as usize], a.status(&mut *b, false), a.dispatch(q, COMPLETION, 0), done.load(SeqCst));
    let work_ran = a.dispatch(q, WORK, 0);
    let after_work = (op_counts(&case)[OP_DO_WORK as usize], a.status(&mut *b, false), a.dispatch(q, COMPLETION, 0), done.load(SeqCst));
    a.cancel(&mut *b);
    let cancels = op_counts(&case)[OP_CANCEL as usize];
    assert_eq!(
        (after_cancel, work_ran, after_work, cancels),
        ((1, E_PENDING, false, 0), true, (1, 0, true, 1), 1),
        "scheduled call: [Cancel ops, status, completion queued, callbacks] after XAsyncCancel, DoWork dispatched, the same after DoWork, Cancel ops after a second XAsyncCancel on the completed call"
    );

    let idle = begin_case(0, Completes::BeginReturns);
    let mut b = Block::new(q);
    let done = counter();
    let d = done.clone();
    on_done(&mut b, move |_| {
        d.fetch_add(1, SeqCst);
    });
    assert_eq!(a.begin(&mut *b, &idle as *const _ as usize, 0, begin_case_provider), 0);
    a.cancel(&mut *b);
    let after_cancel = (op_counts(&idle)[OP_CANCEL as usize], a.status(&mut *b, false), a.dispatch(q, COMPLETION, 0), done.load(SeqCst));
    a.complete(&mut *b, E_ABORT, 0);
    let after_complete = (a.status(&mut *b, false), a.dispatch(q, COMPLETION, 0), done.load(SeqCst));
    assert_eq!(
        (after_cancel, after_complete),
        ((1, E_PENDING, false, 0), (E_ABORT, true, 1)),
        "unscheduled call: [Cancel ops, status, completion queued, callbacks] after XAsyncCancel, then [status, completion queued, callbacks] after the provider completes with E_ABORT"
    );
    assert!(!a.dispatch(q, WORK, 0));

    let mut seen = Vec::new();
    for with_callback in [false, true] {
        let probe = CancelProbe { ops: Default::default(), cleanups_in_cancel: AtomicU32::new(u32::MAX) };
        let mut b = Block::new(q);
        if with_callback {
            on_done(&mut b, |_| {});
        }
        assert_eq!(a.begin(&mut *b, &probe as *const _ as usize, 0, completes_in_cancel), 0);
        a.cancel(&mut *b);
        let after_cancel = probe.ops[OP_CLEANUP as usize].load(SeqCst);
        let queued = a.dispatch(q, COMPLETION, 0);
        seen.push((probe.cleanups_in_cancel.load(SeqCst), after_cancel, queued, probe.ops[OP_CLEANUP as usize].load(SeqCst), a.status(&mut *b, false)));
    }
    assert_eq!(
        seen,
        [(0, 1, false, 1, E_ABORT), (0, 0, true, 1, E_ABORT)],
        "provider completing inside its Cancel op, without then with a callback: [Cleanup ops seen inside Cancel, after XAsyncCancel returned, completion queued, Cleanup ops after the completion port, status]"
    );
    a.close(q);
}

struct CancelProbe {
    ops: [AtomicU32; 5],
    cleanups_in_cancel: AtomicU32,
}

unsafe extern "system" fn completes_in_cancel(op: u32, d: *const Data) -> i32 {
    let p = &*((*d).context as *const CancelProbe);
    p.ops[op as usize].fetch_add(1, SeqCst);
    if op == OP_CANCEL {
        api().complete((*d).block, E_ABORT, 0);
        p.cleanups_in_cancel.store(p.ops[OP_CLEANUP as usize].load(SeqCst), SeqCst);
    }
    0
}

unsafe extern "system" fn delayed_work_provider(op: u32, d: *const Data) -> i32 {
    let ops = &*((*d).context as *const [AtomicU32; 5]);
    ops[op as usize].fetch_add(1, SeqCst);
    if op == OP_BEGIN {
        return api().schedule((*d).block, 10000);
    }
    0
}

pub fn verify_canceled_work_cancels_the_provider() {
    let a = api();
    let q = thread_pool_queue();
    let ops: [AtomicU32; 5] = Default::default();
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, &ops as *const _ as usize, 0, delayed_work_provider), 0);
    let ended = Arc::new(Event::new(true));
    let e = ended.clone();
    assert_eq!(a.terminate(q, false, Some(Box::new(move || e.set()))), 0);
    assert_eq!(a.status(&mut *b, true), E_ABORT, "a call whose scheduled work the terminating queue canceled");
    assert!(ended.wait(5000), "termination did not finish");
    assert!(until(2000, || ops[OP_CLEANUP as usize].load(SeqCst) == 1));
    let seen: [u32; 5] = core::array::from_fn(|i| ops[i].load(SeqCst));
    assert_eq!(seen, [1, 0, 0, 1, 1], "ops seen as [Begin, DoWork, GetResult, Cancel, Cleanup]");
    a.close(q);
}

pub fn verify_status_of_a_block_never_started() {
    let a = api();
    let q = thread_pool_queue();
    let mut b = Block::new(q);
    let (mut size, mut used) = (7usize, 7usize);
    let zeroed = (a.status(&mut *b, false), a.status(&mut *b, true), a.result_size(&mut *b, &mut size), size, a.result(&mut *b, 0, 0, core::ptr::null_mut(), &mut used), used);
    let mut p = Block::new(q);
    p.internal[8..12].copy_from_slice(&E_PENDING.to_le_bytes());
    let pending = (a.status(&mut *p, false), a.status(&mut *p, true));
    assert_eq!(
        (zeroed, pending),
        ((0, 0, 0, 0, 0, 0), (E_PENDING, E_INVALIDARG)),
        "zeroed block: [status, status(wait), result size hr, size, result hr, used]; block whose status field reads E_PENDING: [status, status(wait)]"
    );
    a.close(q);
}

pub fn verify_result_errors_end_the_call() {
    let a = api();
    let q = thread_pool_queue();
    let mut seen = Vec::new();
    for case in 0..4 {
        let f = Factorial::new(5, 0);
        let mut b = Block::new(q);
        assert_eq!(factorial_async(&f, &mut b, false), 0);
        assert_eq!(a.status(&mut *b, true), 0);
        let mut size = 0usize;
        assert_eq!((a.result_size(&mut *b, &mut size), size), (0, 4));
        let (mut v, mut used) = (0u32, 0usize);
        let vp = &mut v as *mut u32 as *mut c_void;
        let hr = match case {
            0 => a.result(&mut *b, FACTORIAL_ID + 1, 4, vp, &mut used),
            1 => a.result(&mut *b, 0, 4, vp, &mut used),
            2 => a.result(&mut *b, FACTORIAL_ID, 4, core::ptr::null_mut(), &mut used),
            _ => a.result(&mut *b, FACTORIAL_ID, 2, vp, &mut used),
        };
        let cleaned = until(2000, || f.ops().contains(&OP_CLEANUP));
        let retry = factorial_result(&mut *b, &mut v);
        let mut after = 9usize;
        let size_hr = a.result_size(&mut *b, &mut after);
        seen.push((hr, cleaned, retry, size_hr, after));
    }
    let failed = (E_ILLEGAL_METHOD_CALL, 0, 0);
    let expected: Vec<(i32, bool, i32, i32, usize)> = [E_INVALIDARG, E_INVALIDARG, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER].iter().map(|hr| (*hr, true, failed.0, failed.1, failed.2)).collect();
    assert_eq!(seen, expected, "per case (other identity, null identity, null buffer, short buffer): [hr, Cleanup ran, hr of a correct retry, result size hr, size]");

    let case = begin_case(0, Completes::InDoWork);
    let mut b = Block::new(q);
    assert_eq!(a.begin(&mut *b, &case as *const _ as usize, 0x1234, begin_case_provider), 0);
    assert_eq!(a.status(&mut *b, true), 0);
    let mut used = 7usize;
    let zero_payload = (a.result(&mut *b, 0x9999, 0, core::ptr::null_mut(), &mut used), used);
    assert_eq!(zero_payload, (0, 0), "zero-payload call read with another identity: [hr, used]");
    a.close(q);
}


macro_rules! xtq_tests {
    ($setup:path) => {
        xtq_tests!(@ $setup;
            verify_stock_queue, verify_composite_queue, verify_duplicate_queue_handle, verify_dispatch,
            verify_submitted_callback, verify_submit_callback_with_wait, verify_register_callback_submitted,
            verify_immediate_dispatch, verify_serialized_thread_pool_dispatch,
            verify_serialized_thread_pool_does_not_strand_items, verify_multithread_manual_dispatch,
            verify_register_with_auto_reset, verify_shared_port_queue_rundown_keeps_other_waiters,
            verify_shared_port_queue_rundown_drops_its_wait_handles, verify_queue_termination,
            verify_empty_queue_termination, verify_global_queue, verify_close_in_termination_for_thread_pool,
            verify_close_in_termination_for_manual, verify_wait_termination, verify_manual_dispatch_at_termination,
            verify_termination_of_composite_queue, verify_termination_of_base_queue_keeps_composites,
            verify_queue_monitor_has_correct_ports,
            verify_nested_queue_dispatch_order, verify_terminate_waits_for_async_in_flight,
            verify_single_queue_delayed_poll_strand, verify_composite_queue_delayed_callback_starvation,
            verify_termination_cancels_delayed_callbacks, verify_termination_order_on_a_manual_queue,
            verify_close_keeps_queued_callbacks,
            verify_dispatch_on_pool_port, verify_thread_pool_callbacks_can_block,
            verify_serialized_queues_do_not_keep_threads, verify_serialized_port_final_release,
            verify_serialized_port_survives_one_release, verify_serialized_idle_port_delivers_termination,
            verify_serialized_termination_during_callback,
            verify_simple_async_call, verify_multiple_calls, verify_distributed_async_call, verify_async_block_reuse,
            verify_cancellation, verify_cleanup_waits_for_work, verify_cleanup_waits_for_work_distributed,
            verify_run_async, verify_custom_queue, verify_cant_schedule_twice, verify_wait_for_completion,
            verify_periodic_pattern, verify_run_alot_async, verify_get_async_status_no_deadlock,
            verify_failed_begin_completes, verify_pending_begin_stays_pending, verify_failed_begin_reports_its_failure,
            verify_cleanup_waits_for_the_callback_when_begin_completes, verify_zero_payload_cleans_up_fast, verify_complete_in_begin,
            verify_begin_after_terminate, verify_failure_in_do_work, verify_duplicate_result_calls_fail,
            verify_duplicate_pending_result_calls_succeed, verify_no_write_after_get_status_race,
            verify_result_buffer_checks, verify_cancel_leaves_completion_to_the_provider,
            verify_canceled_work_cancels_the_provider, verify_status_of_a_block_never_started,
            verify_result_errors_end_the_call);
    };
    (@ $setup:path; $($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                if $setup(stringify!($name)) {
                    xtq::$name();
                }
            }
        )*
    };
}
