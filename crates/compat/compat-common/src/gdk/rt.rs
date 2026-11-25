use super::streaming::E_NO_TASK_QUEUE;
use super::xasync::{ProviderData, XASYNC};
use super::{log, setting, Iface, This, E_ABORT, E_INVALIDARG, E_NOT_SUFFICIENT_BUFFER, E_PENDING, S_OK};
use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub type Job = Box<dyn FnOnce(bool) + Send>;
type TaskCb = unsafe extern "system" fn(*mut c_void, bool);
type AsyncWork = unsafe extern "system" fn(*mut c_void) -> i32;
type Provider = unsafe extern "system" fn(u32, *const ProviderData) -> i32;
type Completion = unsafe extern "system" fn(*mut c_void);
type TerminatedCb = unsafe extern "system" fn(*mut c_void);
type MonitorCb = unsafe extern "system" fn(*mut c_void, usize, u32);

pub const MANUAL: u32 = 0;
pub const THREAD_POOL: u32 = 1;
pub const SERIALIZED: u32 = 2;
pub const IMMEDIATE: u32 = 3;

pub fn active() -> bool {
    static A: OnceLock<bool> = OnceLock::new();
    *A.get_or_init(|| {
        let forced = setting("gdk_xasync").map(|s| s.to_ascii_lowercase());
        let a = match forced.as_deref() {
            Some("real") => unsafe { !real_xasync_ok() },
            _ => true,
        };
        log("xasync", if a { "XAsync/XTaskQueue: emulated implementation" } else { "XAsync/XTaskQueue: Gaming Runtime implementation" });
        a
    })
}

unsafe fn real_xasync_ok() -> bool {
    let Some(q) = super::real_query() else { return false };
    let mut out: *mut c_void = core::ptr::null_mut();
    q(&XASYNC, &XASYNC, &mut out) >= 0 && !out.is_null()
}

type Tagged = (usize, Job);

extern "system" {
    fn TrySubmitThreadpoolCallback(
        pfns: unsafe extern "system" fn(*mut c_void, *mut c_void),
        pv: *mut c_void,
        pcbe: *mut c_void,
    ) -> i32;
    fn CallbackMayRunLong(pci: *mut c_void) -> i32;
}

unsafe extern "system" fn tp_trampoline(instance: *mut c_void, ctx: *mut c_void) {
    CallbackMayRunLong(instance);
    let port = Arc::from_raw(ctx as *const Port);
    port.drain_one();
}

fn submit_to_thread_pool(port: Arc<Port>) {
    let raw = Arc::into_raw(port) as *mut c_void;
    let ok = unsafe { TrySubmitThreadpoolCallback(tp_trampoline, raw, core::ptr::null_mut()) };
    if ok == 0 {
        let port = unsafe { Arc::from_raw(raw as *const Port) };
        std::thread::spawn(move || port.drain_one());
    }
}

#[derive(Clone)]
struct Monitor {
    token: u64,
    ctx: usize,
    cb: MonitorCb,
    port: u32,
    target: usize,
    handle: usize,
    queue: std::sync::Weak<Queue>,
}

fn monitors() -> &'static Mutex<HashMap<usize, Vec<Monitor>>> {
    static M: OnceLock<Mutex<HashMap<usize, Vec<Monitor>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct Port {
    mode: u32,
    items: Mutex<VecDeque<Tagged>>,
    cv: Condvar,
    terminated: AtomicBool,
    serial_started: AtomicBool,
    users: AtomicUsize,
}

impl Port {
    fn new(mode: u32) -> Arc<Port> {
        Arc::new(Port { mode, items: Mutex::new(VecDeque::new()), cv: Condvar::new(), terminated: AtomicBool::new(false), serial_started: AtomicBool::new(false), users: AtomicUsize::new(0) })
    }

    #[cfg(test)]
    fn submit(self: &Arc<Port>, job: Job) {
        self.submit_tagged(0, 0, job)
    }

    fn drain_one(&self) {
        let job = self.items.lock().unwrap_or_else(|e| e.into_inner()).pop_front().map(|(_, j)| j);
        if let Some(j) = job {
            j(self.terminated.load(Ordering::Acquire));
        }
    }

    fn notify(&self, owner: usize, submit_port: Option<u32>) {
        if owner == 0 {
            return;
        }
        let me = self as *const Port as usize;
        let m: Vec<Monitor> = {
            let all = monitors().lock().unwrap_or_else(|e| e.into_inner());
            all.iter()
                .flat_map(|(&qid, list)| {
                    list.iter()
                        .filter(move |x| x.target == me && (qid != owner || submit_port.is_none_or(|sp| x.port == sp)))
                        .cloned()
                })
                .collect()
        };
        for x in m {
            let h = x.queue.upgrade().and_then(|qq| qq.live_handle(x.handle));
            if let Some(h) = h {
                unsafe { (x.cb)(x.ctx as *mut c_void, h, x.port) };
            }
        }
    }

    fn submit_tagged(self: &Arc<Port>, tag: usize, port: u32, job: Job) {
        if self.terminated.load(Ordering::Acquire) {
            job(true);
            return;
        }
        self.enqueue(tag, job);
        if self.mode != IMMEDIATE {
            self.notify(tag, Some(port));
        }
    }

    fn enqueue(self: &Arc<Port>, tag: usize, job: Job) {
        match self.mode {
            IMMEDIATE => job(false),
            THREAD_POOL => {
                self.items.lock().unwrap_or_else(|e| e.into_inner()).push_back((tag, job));
                self.cv.notify_one();
                submit_to_thread_pool(self.clone());
            }
            SERIALIZED => {
                self.items.lock().unwrap_or_else(|e| e.into_inner()).push_back((tag, job));
                self.cv.notify_all();
                self.ensure_serial();
            }
            _ => {
                self.items.lock().unwrap_or_else(|e| e.into_inner()).push_back((tag, job));
                self.cv.notify_all();
            }
        }
    }

    fn ensure_serial(self: &Arc<Port>) {
        if self.serial_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        std::thread::spawn(move || loop {
            let job = {
                let mut q = me.items.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some((_, j)) = q.pop_front() {
                        break Some(j);
                    }
                    if me.users.load(Ordering::Acquire) == 0 {
                        me.serial_started.store(false, Ordering::Release);
                        break None;
                    }
                    q = me.cv.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            let Some(job) = job else { return };
            job(me.terminated.load(Ordering::Acquire));
        });
    }

    fn submit_marker(self: &Arc<Port>, owner: usize, port: u32, job: Job) {
        match self.mode {
            IMMEDIATE => job(false),
            THREAD_POOL => {
                self.items.lock().unwrap_or_else(|e| e.into_inner()).push_back((0, job));
                self.cv.notify_one();
                submit_to_thread_pool(self.clone());
                self.notify(owner, Some(port));
            }
            _ => {
                self.items.lock().unwrap_or_else(|e| e.into_inner()).push_back((0, job));
                self.cv.notify_all();
                if self.mode == SERIALIZED {
                    self.ensure_serial();
                    self.notify(owner, Some(port));
                } else if self.users.load(Ordering::Acquire) != 0 {
                    self.notify(owner, Some(port));
                } else {
                    let drained: Vec<Job> = self.items.lock().unwrap_or_else(|e| e.into_inner()).drain(..).map(|(_, j)| j).collect();
                    for j in drained {
                        j(true);
                    }
                }
            }
        }
    }

    fn dispatch(&self, timeout_ms: u32, done: &AtomicBool) -> bool {
        let deadline = (timeout_ms != 0 && timeout_ms != u32::MAX).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
        let job = {
            let mut q = self.items.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some((_, j)) = q.pop_front() {
                    break Some(j);
                }
                if done.load(Ordering::Acquire) || timeout_ms == 0 {
                    break None;
                }
                q = match deadline {
                    Some(d) => {
                        let now = Instant::now();
                        if now >= d {
                            break None;
                        }
                        self.cv.wait_timeout(q, d - now).unwrap_or_else(|e| e.into_inner()).0
                    }
                    None => self.cv.wait(q).unwrap_or_else(|e| e.into_inner()),
                };
            }
        };
        match job {
            Some(j) => {
                j(self.terminated.load(Ordering::Acquire));
                true
            }
            None => false,
        }
    }

    fn wake(&self) {
        let _g = self.items.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
    }

    fn terminate(&self) {
        self.terminated.store(true, Ordering::Release);
        self.wake();
    }

    fn release(&self) {
        if self.users.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _g = self.items.lock().unwrap_or_else(|e| e.into_inner());
            self.cv.notify_all();
        }
    }

    #[cfg(test)]
    fn cancel_all(&self) {
        self.terminated.store(true, Ordering::Release);
        let drained: Vec<Job> = self.items.lock().unwrap_or_else(|e| e.into_inner()).drain(..).map(|(_, j)| j).collect();
        for j in drained {
            j(true);
        }
        self.cv.notify_all();
    }
}

pub struct Queue {
    work: Arc<Port>,
    completion: Arc<Port>,
    port_done: [Arc<AtomicBool>; 2],
    refs: AtomicUsize,
    process: bool,
    owns: bool,
    ended: Arc<AtomicBool>,
    live: Arc<Live>,
    handles: Mutex<Vec<usize>>,
}

impl Queue {
    fn id(&self) -> usize {
        self as *const Queue as usize
    }

    fn live_handle(&self, preferred: usize) -> Option<usize> {
        let hs = self.handles.lock().unwrap_or_else(|e| e.into_inner());
        if hs.contains(&preferred) { Some(preferred) } else { hs.first().copied() }
    }
}

type Idle = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct LiveState {
    n: [usize; 2],
    ended: bool,
    work_done: bool,
    on_idle: [Vec<Idle>; 2],
}

#[derive(Default)]
struct Live {
    st: Mutex<LiveState>,
}

impl Live {
    fn try_enter(&self, port: u32) -> bool {
        let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
        if s.ended && (port == 0 || s.work_done) {
            return false;
        }
        s.n[(port != 0) as usize] += 1;
        true
    }

    fn leave(&self, port: u32) {
        let run = {
            let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
            let p = (port != 0) as usize;
            s.n[p] = s.n[p].saturating_sub(1);
            if s.n[p] == 0 && s.ended { std::mem::take(&mut s.on_idle[p]) } else { Vec::new() }
        };
        for f in run {
            f();
        }
    }

    fn end(&self, ended: &AtomicBool, on_idle: Idle) {
        let now = {
            let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
            s.ended = true;
            ended.store(true, Ordering::Release);
            if s.n[0] == 0 {
                Some(on_idle)
            } else {
                s.on_idle[0].push(on_idle);
                None
            }
        };
        if let Some(f) = now {
            f();
        }
    }

    fn finish_work(&self, close_completion: impl FnOnce(), then: Idle) {
        self.st.lock().unwrap_or_else(|e| e.into_inner()).work_done = true;
        close_completion();
        let now = {
            let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
            if s.n[1] == 0 {
                Some(then)
            } else {
                s.on_idle[1].push(then);
                None
            }
        };
        if let Some(f) = now {
            f();
        }
    }
}

struct Signal {
    done: Mutex<bool>,
    cv: Condvar,
}

const QMAGIC: u64 = 0x5442_5851_5545_5545;

fn port_handles() -> &'static Mutex<HashMap<usize, Arc<Port>>> {
    static M: OnceLock<Mutex<HashMap<usize, Arc<Port>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn port_handle(p: &Arc<Port>) -> usize {
    let k = Arc::as_ptr(p) as usize;
    port_handles().lock().unwrap_or_else(|e| e.into_inner()).entry(k).or_insert_with(|| p.clone());
    k
}

fn handles() -> &'static Mutex<HashMap<usize, Arc<Queue>>> {
    static M: OnceLock<Mutex<HashMap<usize, Arc<Queue>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn alloc_handle(q: Arc<Queue>) -> usize {
    let h = Box::into_raw(Box::new(QMAGIC)) as usize;
    q.refs.fetch_add(1, Ordering::AcqRel);
    q.handles.lock().unwrap_or_else(|e| e.into_inner()).push(h);
    handles().lock().unwrap_or_else(|e| e.into_inner()).insert(h, q);
    h
}

fn close_handle(h: usize) {
    if h == 0 {
        return;
    }
    let Some(qq) = handles().lock().unwrap_or_else(|e| e.into_inner()).remove(&h) else {
        return;
    };
    {
        let mut hs = qq.handles.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = hs.iter().position(|x| *x == h) {
            hs.swap_remove(pos);
        }
    }
    unsafe { drop(Box::from_raw(h as *mut u64)) };
    let prev = qq.refs.fetch_sub(1, Ordering::AcqRel);
    if !qq.process && prev == 1 {
        monitors().lock().unwrap_or_else(|e| e.into_inner()).remove(&qq.id());
        drop_waiters(&qq, false);
        qq.work.release();
        qq.completion.release();
    }
}

fn new_queue(work: Arc<Port>, completion: Arc<Port>, process: bool, owns: bool) -> usize {
    work.users.fetch_add(1, Ordering::AcqRel);
    completion.users.fetch_add(1, Ordering::AcqRel);
    alloc_handle(Arc::new(Queue {
        work,
        completion,
        port_done: [Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false))],
        refs: AtomicUsize::new(0),
        process,
        owns,
        ended: Arc::new(AtomicBool::new(false)),
        live: Arc::new(Live::default()),
        handles: Mutex::new(Vec::new()),
    }))
}

fn default_process_queue() -> usize {
    static P: AtomicUsize = AtomicUsize::new(0);
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| P.store(new_queue(Port::new(THREAD_POOL), Port::new(THREAD_POOL), true, true), Ordering::Release));
    P.load(Ordering::Acquire)
}

fn process_queue() -> usize {
    match OVERRIDE.load(Ordering::Acquire) {
        DEFAULT_PROCESS => default_process_queue(),
        o => o,
    }
}

const DEFAULT_PROCESS: usize = usize::MAX;
static OVERRIDE: AtomicUsize = AtomicUsize::new(DEFAULT_PROCESS);

pub fn process_queue_available() -> bool {
    !active() || process_queue() != 0
}

fn queue(h: usize) -> Option<Arc<Queue>> {
    let h = if h == 0 { process_queue() } else { h };
    if h == 0 {
        return None;
    }
    handles().lock().unwrap_or_else(|e| e.into_inner()).get(&h).cloned()
}

fn port_of(h: usize, port: u32) -> Option<Arc<Port>> {
    let q = queue(h)?;
    Some(if port == 0 { q.work.clone() } else { q.completion.clone() })
}

enum Queued {
    Ready(Arc<Port>, usize, Job),
    Aborted(Job),
    Invalid(Job),
}

unsafe fn queued(h: usize, port: u32, job: Job) -> Queued {
    let Some(q) = queue(h) else { return Queued::Invalid(job) };
    queued_on(&q, port, job)
}

fn queued_on(q: &Arc<Queue>, port: u32, job: Job) -> Queued {
    let p =if port == 0 { q.work.clone() } else { q.completion.clone() };
    if p.terminated.load(Ordering::Acquire) || !q.live.try_enter(port) {
        return Queued::Aborted(job);
    }
    let tag = q.id();
    let ended = q.ended.clone();
    let live = q.live.clone();
    Queued::Ready(
        p,
        tag,
        Box::new(move |canceled| {
            let key = Arc::as_ptr(&live) as usize;
            FRAMES.with(|f| f.borrow_mut().push((key, port, false)));
            job(canceled || ended.load(Ordering::Acquire));
            let released = FRAMES.with(|f| f.borrow_mut().pop().map(|x| x.2).unwrap_or(false));
            if !released {
                live.leave(port);
            }
        }),
    )
}

thread_local! {
    static FRAMES: RefCell<Vec<(usize, u32, bool)>> = const { RefCell::new(Vec::new()) };
}

fn release_own_frames(live: &Arc<Live>) -> Vec<u32> {
    let key = Arc::as_ptr(live) as usize;
    FRAMES.with(|f| {
        let mut ports = Vec::new();
        for fr in f.borrow_mut().iter_mut() {
            if fr.0 == key && !fr.2 {
                fr.2 = true;
                ports.push(fr.1);
            }
        }
        ports
    })
}

pub unsafe fn submit(queue_h: usize, port: u32, job: Job) -> i32 {
    match queued(queue_h, port, job) {
        Queued::Ready(p, tag, j) => {
            p.submit_tagged(tag, port, j);
            S_OK
        }
        Queued::Aborted(_) => E_ABORT,
        Queued::Invalid(_) => E_INVALIDARG,
    }
}

type Timer = (Instant, usize, u32, Arc<Port>, Option<Job>);

fn timers() -> &'static (Mutex<Vec<Timer>>, Condvar) {
    static T: OnceLock<(Mutex<Vec<Timer>>, Condvar)> = OnceLock::new();
    static START: std::sync::Once = std::sync::Once::new();
    let t = T.get_or_init(|| (Mutex::new(Vec::new()), Condvar::new()));
    START.call_once(|| {
        std::thread::spawn(|| {
            let (m, cv) = timers();
            loop {
                let mut due: Vec<(Arc<Port>, usize, u32, Job)> = Vec::new();
                {
                    let mut v = m.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        let now = Instant::now();
                        let mut i = 0;
                        while i < v.len() {
                            if v[i].0 <= now {
                                let (_, tag, port, p, j) = v.swap_remove(i);
                                if let Some(j) = j {
                                    due.push((p, tag, port, j));
                                }
                            } else {
                                i += 1;
                            }
                        }
                        if !due.is_empty() {
                            break;
                        }
                        let next = v.iter().map(|x| x.0).min();
                        v = match next {
                            Some(n) => cv.wait_timeout(v, n.saturating_duration_since(Instant::now())).unwrap_or_else(|e| e.into_inner()).0,
                            None => cv.wait(v).unwrap_or_else(|e| e.into_inner()),
                        };
                    }
                }
                for (p, tag, port, j) in due {
                    p.submit_tagged(tag, port, j);
                }
            }
        });
    });
    t
}

pub unsafe fn submit_delayed(queue_h: usize, port: u32, delay_ms: u32, job: Job) -> i32 {
    let (p, tag, job) = match queued(queue_h, port, job) {
        Queued::Ready(p, tag, j) => (p, tag, j),
        Queued::Aborted(_) => return E_ABORT,
        Queued::Invalid(_) => return E_INVALIDARG,
    };
    if delay_ms == 0 {
        p.submit_tagged(tag, port, job);
        return S_OK;
    }
    let (m, cv) = timers();
    m.lock().unwrap_or_else(|e| e.into_inner()).push((Instant::now() + Duration::from_millis(delay_ms as u64), tag, (port != 0) as u32, p, Some(job)));
    cv.notify_all();
    S_OK
}

fn flush_timers(tag: usize, port: u32, whole: Option<&Arc<Port>>) {
    let mut due: Vec<(Arc<Port>, usize, u32, Job)> = Vec::new();
    {
        let (m, cv) = timers();
        let mut v = m.lock().unwrap_or_else(|e| e.into_inner());
        let mut i = 0;
        while i < v.len() {
            if (v[i].1 == tag && v[i].2 == port) || whole.is_some_and(|p| Arc::ptr_eq(p, &v[i].3)) {
                let (_, tg, pt, p, j) = v.swap_remove(i);
                if let Some(j) = j {
                    due.push((p, tg, pt, j));
                }
            } else {
                i += 1;
            }
        }
        cv.notify_all();
    }
    for (p, tg, port, j) in due {
        p.submit_tagged(tg, port, j);
    }
}

struct SendPtr(usize);
unsafe impl Send for SendPtr {}

fn callback_job(ctx: *mut c_void, cb: TaskCb) -> Job {
    let c = SendPtr(ctx as usize);
    Box::new(move |canceled| unsafe { cb(c.0 as *mut c_void, canceled) })
}

unsafe extern "system" fn q_create(_t: This, work: u32, completion: u32, out: *mut usize) -> i32 {
    if out.is_null() || work > 3 || completion > 3 {
        return E_INVALIDARG;
    }
    *out = new_queue(Port::new(work), Port::new(completion), false, true);
    S_OK
}

unsafe extern "system" fn q_get_port(_t: This, q: usize, port: u32, out: *mut usize) -> i32 {
    if out.is_null() || port > 1 {
        return E_INVALIDARG;
    }
    match port_of(q, port) {
        Some(p) => {
            *out = port_handle(&p);
            S_OK
        }
        None => E_INVALIDARG,
    }
}

unsafe extern "system" fn q_create_composite(_t: This, work: usize, completion: usize, out: *mut usize) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let m = port_handles().lock().unwrap_or_else(|e| e.into_inner());
    let (Some(w), Some(c)) = (m.get(&work).cloned(), m.get(&completion).cloned()) else { return E_INVALIDARG };
    drop(m);
    if w.terminated.load(Ordering::Acquire) || c.terminated.load(Ordering::Acquire) {
        return E_ABORT;
    }
    *out = new_queue(w, c, false, false);
    S_OK
}

unsafe extern "system" fn q_duplicate(_t: This, q: usize, out: *mut usize) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let Some(qq) = queue(q) else { return E_INVALIDARG };
    *out = alloc_handle(qq);
    S_OK
}

unsafe extern "system" fn q_dispatch(_t: This, q: usize, port: u32, timeout: u32) -> bool {
    if port > 1 {
        return false;
    }
    let Some(qq) = queue(q) else { return false };
    let p = if port == 0 { &qq.work } else { &qq.completion };
    if p.mode != IMMEDIATE { p.dispatch(timeout, &qq.port_done[port as usize]) } else { false }
}

unsafe extern "system" fn q_close(_t: This, q: usize) {
    close_handle(q);
}

unsafe extern "system" fn q_submit(_t: This, q: usize, port: u32, ctx: *mut c_void, cb: Option<TaskCb>) -> i32 {
    let Some(cb) = cb else { return E_INVALIDARG };
    submit(q, port, callback_job(ctx, cb))
}

unsafe extern "system" fn q_submit_delayed(_t: This, q: usize, port: u32, delay: u32, ctx: *mut c_void, cb: Option<TaskCb>) -> i32 {
    let Some(cb) = cb else { return E_INVALIDARG };
    submit_delayed(q, port, delay, callback_job(ctx, cb))
}

pub const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;

unsafe extern "system" fn q_terminate(_t: This, q: usize, wait: bool, ctx: *mut c_void, cb: Option<TerminatedCb>) -> i32 {
    let Some(qq) = queue(q) else { return E_INVALIDARG };
    if qq.process {
        return E_ACCESSDENIED;
    }
    let tag = qq.id();
    let sig = Arc::new(Signal { done: Mutex::new(false), cv: Condvar::new() });
    let c = SendPtr(ctx as usize);
    let s2 = sig.clone();
    let completion2 = qq.completion.clone();
    let comp_done = qq.port_done[1].clone();
    let done = move || {
        if let Some(cb) = cb {
            cb(c.0 as *mut c_void);
        }
        comp_done.store(true, Ordering::Release);
        completion2.wake();
        *s2.done.lock().unwrap_or_else(|e| e.into_inner()) = true;
        s2.cv.notify_all();
    };
    let work = qq.work.clone();
    let work2 = qq.work.clone();
    let work_done = qq.port_done[0].clone();
    let completion = qq.completion.clone();
    let live = qq.live.clone();
    let owns = qq.owns;
    drop_waiters(&qq, true);
    let route: Idle = Box::new(move || {
        let deliver: Job = Box::new(move |_| done());
        let forward: Job = Box::new(move |_| {
            work_done.store(true, Ordering::Release);
            work2.wake();
            let closing = completion.clone();
            live.finish_work(
                move || {
                    flush_timers(tag, 1, owns.then_some(&closing));
                    if owns {
                        closing.terminate();
                    }
                },
                Box::new(move || completion.submit_marker(tag, 1, deliver)),
            )
        });
        work.submit_marker(tag, 0, forward);
    });
    let own = release_own_frames(&qq.live);
    qq.live.end(&qq.ended, route);
    for port in own {
        qq.live.leave(port);
    }
    flush_timers(tag, 0, qq.owns.then_some(&qq.work));
    if qq.owns {
        qq.work.terminate();
    }
    if wait {
        let mut d = sig.done.lock().unwrap_or_else(|e| e.into_inner());
        while !*d {
            d = sig.cv.wait(d).unwrap_or_else(|e| e.into_inner());
        }
    }
    S_OK
}

unsafe extern "system" fn q_get_process(_t: This, out: *mut usize) -> bool {
    if out.is_null() {
        return false;
    }
    let p = process_queue();
    if p == 0 {
        *out = 0;
        return false;
    }
    match queue(p) {
        Some(qq) => {
            *out = alloc_handle(qq);
            true
        }
        None => {
            *out = 0;
            false
        }
    }
}

extern "system" {
    fn RaiseFailFastException(r: *const c_void, c: *const c_void, flags: u32);
}

fn fail_fast(api: &str, q: usize) -> ! {
    log(api, &format!("invalid queue handle {q:#x}: fail-fast like the console (taskqueue.cpp line 0x831)"));
    unsafe { RaiseFailFastException(core::ptr::null(), core::ptr::null(), 0) };
    std::process::abort()
}

unsafe extern "system" fn q_set_process(_t: This, q: usize) {
    let next = if q == 0 {
        0
    } else {
        match queue(q) {
            Some(qq) => alloc_handle(qq),
            None => fail_fast("XTaskQueueSetCurrentProcessTaskQueue", q),
        }
    };
    let prev = OVERRIDE.swap(next, Ordering::AcqRel);
    if prev != 0 && prev != DEFAULT_PROCESS {
        close_handle(prev);
    }
}

pub const E_POINTER: i32 = 0x8000_4003u32 as i32;
pub const E_OUTOFMEMORY: i32 = 0x8007_000Eu32 as i32;
const MONITOR_MAX: usize = 32;
static NEXT_MONITOR: AtomicUsize = AtomicUsize::new(1);

unsafe extern "system" fn q_register_monitor(_t: This, q: usize, ctx: *mut c_void, cb: Option<MonitorCb>, token: *mut u64) -> i32 {
    let Some(qq) = queue(q) else { return E_INVALIDARG };
    let Some(cb) = cb else { return E_POINTER };
    if token.is_null() {
        return E_POINTER;
    }
    let handle = if q == 0 { process_queue() } else { q };
    let id = qq.id();
    let mut all = monitors().lock().unwrap_or_else(|e| e.into_inner());
    let list = all.entry(id).or_default();
    if list.len() / 2 >= MONITOR_MAX {
        return E_OUTOFMEMORY;
    }
    let t = NEXT_MONITOR.fetch_add(1, Ordering::AcqRel) as u64;
    for (p, port) in [(&qq.work, 0u32), (&qq.completion, 1u32)] {
        list.push(Monitor { token: t, ctx: ctx as usize, cb, port, target: Arc::as_ptr(p) as usize, handle, queue: Arc::downgrade(&qq) });
    }
    *token = t;
    S_OK
}

unsafe extern "system" fn q_unregister_monitor(_t: This, q: usize, token: u64) {
    let Some(qq) = queue(q) else { return };
    if let Some(list) = monitors().lock().unwrap_or_else(|e| e.into_inner()).get_mut(&qq.id()) {
        list.retain(|m| m.token != token);
    }
}

type TpWaitCb = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, u32);

extern "system" {
    fn CreateThreadpoolWait(cb: TpWaitCb, ctx: *mut c_void, env: *const c_void) -> *mut c_void;
    fn SetThreadpoolWait(wait: *mut c_void, handle: usize, timeout: *const i64);
    fn WaitForThreadpoolWaitCallbacks(wait: *mut c_void, cancel_pending: i32);
    fn CloseThreadpoolWait(wait: *mut c_void);
}

#[derive(Default)]
struct WaiterState {
    appended: bool,
    deleted: bool,
}

struct Waiter {
    queue: std::sync::Weak<Queue>,
    qid: usize,
    port: u32,
    handle: usize,
    ctx: usize,
    cb: TaskCb,
    tp: usize,
    st: Mutex<WaiterState>,
}

impl Waiter {
    fn rearm(&self) {
        let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
        s.appended = false;
        if !s.deleted {
            unsafe { SetThreadpoolWait(self.tp as *mut c_void, self.handle, core::ptr::null()) };
        }
    }

    fn close(&self) -> bool {
        {
            let mut s = self.st.lock().unwrap_or_else(|e| e.into_inner());
            s.deleted = true;
            unsafe { SetThreadpoolWait(self.tp as *mut c_void, 0, core::ptr::null()) };
        }
        if IN_WAIT_CB.with(|c| c.get()) != self.tp {
            unsafe { WaitForThreadpoolWaitCallbacks(self.tp as *mut c_void, 1) };
        }
        unsafe { CloseThreadpoolWait(self.tp as *mut c_void) };
        self.st.lock().unwrap_or_else(|e| e.into_inner()).appended
    }
}

thread_local! {
    static IN_WAIT_CB: Cell<usize> = const { Cell::new(0) };
}

fn waiters() -> &'static Mutex<HashMap<u64, Arc<Waiter>>> {
    static W: OnceLock<Mutex<HashMap<u64, Arc<Waiter>>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_WAITER: AtomicUsize = AtomicUsize::new(1);

unsafe extern "system" fn waiter_fired(_inst: *mut c_void, ctx: *mut c_void, _wait: *mut c_void, result: u32) {
    if result != 0 {
        return;
    }
    let Some(w) = waiters().lock().unwrap_or_else(|e| e.into_inner()).get(&(ctx as u64)).cloned() else { return };
    {
        let mut s = w.st.lock().unwrap_or_else(|e| e.into_inner());
        if s.deleted || s.appended {
            return;
        }
        s.appended = true;
    }
    let w2 = w.clone();
    let job: Job = Box::new(move |canceled| {
        (w2.cb)(w2.ctx as *mut c_void, canceled);
        w2.rearm();
    });
    let prev = IN_WAIT_CB.with(|c| c.replace(w.tp));
    let r = match w.queue.upgrade() {
        Some(q) => queued_on(&q, w.port, job),
        None => Queued::Invalid(job),
    };
    match r {
        Queued::Ready(p, tag, j) => p.submit_tagged(tag, w.port, j),
        Queued::Aborted(_) | Queued::Invalid(_) => w.st.lock().unwrap_or_else(|e| e.into_inner()).appended = false,
    }
    IN_WAIT_CB.with(|c| c.set(prev));
}

fn drop_waiters(qq: &Arc<Queue>, cancel: bool) {
    let qid = qq.id();
    let mine: Vec<Arc<Waiter>> = {
        let mut all = waiters().lock().unwrap_or_else(|e| e.into_inner());
        let tokens: Vec<u64> = all.iter().filter(|(_, w)| w.qid == qid).map(|(t, _)| *t).collect();
        tokens.iter().filter_map(|t| all.remove(t)).collect()
    };
    for w in mine {
        let appended = w.close();
        if cancel && !appended {
            let c = SendPtr(w.ctx as usize);
            let cb = w.cb;
            if let Queued::Ready(p, tag, j) = queued_on(qq, w.port, Box::new(move |_| unsafe { cb(c.0 as *mut c_void, true) })) {
                p.submit_tagged(tag, w.port, j);
            }
        }
    }
}

unsafe extern "system" fn q_register_waiter(_t: This, q: usize, port: u32, handle: usize, ctx: *mut c_void, cb: Option<TaskCb>, token: *mut u64) -> i32 {
    let Some(qq) = queue(q) else { return E_INVALIDARG };
    let Some(cb) = cb else { return E_POINTER };
    if token.is_null() {
        return E_POINTER;
    }
    if port > 1 || handle == 0 {
        return E_INVALIDARG;
    }
    let p = if port == 0 { &qq.work } else { &qq.completion };
    if p.terminated.load(Ordering::Acquire) || qq.ended.load(Ordering::Acquire) {
        return E_ABORT;
    }
    let t = NEXT_WAITER.fetch_add(1, Ordering::AcqRel) as u64;
    let tp = CreateThreadpoolWait(waiter_fired, t as usize as *mut c_void, core::ptr::null());
    if tp.is_null() {
        return E_OUTOFMEMORY;
    }
    let w = Arc::new(Waiter { queue: Arc::downgrade(&qq), qid: qq.id(), port, handle, ctx: ctx as usize, cb, tp: tp as usize, st: Mutex::new(WaiterState::default()) });
    waiters().lock().unwrap_or_else(|e| e.into_inner()).insert(t, w.clone());
    *token = t;
    if !w.st.lock().unwrap_or_else(|e| e.into_inner()).deleted {
        SetThreadpoolWait(tp, handle, core::ptr::null());
    }
    S_OK
}

unsafe extern "system" fn q_unregister_waiter(_t: This, q: usize, token: u64) {
    let Some(qq) = queue(q) else { return };
    let qid = qq.id();
    let w = {
        let mut all = waiters().lock().unwrap_or_else(|e| e.into_inner());
        match all.get(&token) {
            Some(w) if w.qid == qid => all.remove(&token),
            _ => None,
        }
    };
    if let Some(w) = w {
        w.close();
    }
}

thread_local! {
    static TIME_SENSITIVE: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "system" fn thread_set_time_sensitive(_t: This, v: bool) -> i32 {
    TIME_SENSITIVE.with(|c| c.set(v));
    S_OK
}

unsafe extern "system" fn thread_is_time_sensitive(_t: This) -> bool {
    TIME_SENSITIVE.with(|c| c.get())
}

unsafe extern "system" fn thread_assert_not_time_sensitive(_t: This) {
    if TIME_SENSITIVE.with(|c| c.get()) {
        log("XThreadAssertNotTimeSensitive", "a time-sensitive thread called a blocking API");
    }
}

#[repr(C)]
pub struct Block {
    pub queue: usize,
    pub context: *mut c_void,
    pub callback: Option<Completion>,
    pub internal: [u8; 32],
}

const BMAGIC: u32 = 0x4158_4254;
const E_UNEXPECTED: i32 = 0x8000_FFFFu32 as i32;

struct AState {
    provider: Provider,
    ctx: usize,
    identity: usize,
    status: Mutex<(i32, usize)>,
    cv: Condvar,
    signaled: AtomicBool,
    cleaned: AtomicBool,
    queue: usize,
    in_cancel: AtomicBool,
    cleanup_after_cancel: AtomicBool,
    scheduled: AtomicBool,
    in_begin: AtomicBool,
    data: Box<core::cell::UnsafeCell<ProviderData>>,
}

unsafe impl Send for AState {}
unsafe impl Sync for AState {}

impl AState {
    fn signal(&self) {
        let _st = self.status.lock().unwrap_or_else(|e| e.into_inner());
        self.signaled.store(true, Ordering::Release);
        self.cv.notify_all();
    }
}

fn states() -> &'static Mutex<HashMap<usize, Arc<AState>>> {
    static M: OnceLock<Mutex<HashMap<usize, Arc<AState>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state(block: usize) -> Option<Arc<AState>> {
    states().lock().unwrap_or_else(|e| e.into_inner()).get(&block).cloned()
}

unsafe fn mark(block: *mut Block, hr: i32, live: bool) {
    let i = &mut (*block).internal;
    i[0..4].copy_from_slice(&BMAGIC.to_le_bytes());
    i[4..8].copy_from_slice(&hr.to_le_bytes());
    i[8] = live as u8;
    i[9] = 0;
}

unsafe fn marked(block: *const Block) -> Option<i32> {
    let i = &(*block).internal;
    if u32::from_le_bytes(i[0..4].try_into().ok()?) == BMAGIC {
        Some(i32::from_le_bytes(i[4..8].try_into().ok()?))
    } else {
        None
    }
}

unsafe fn call(s: &AState, op: u32, block: *mut Block, size: usize, buf: *mut c_void) -> i32 {
    let d = s.data.get();
    (*d).block = block as *mut c_void;
    (*d).buffer_size = size;
    (*d).buffer = buf as *mut u8;
    (*d).context = s.ctx as *mut c_void;
    (s.provider)(op, d)
}

unsafe fn cleanup(s: &AState, block: *mut Block) {
    if !s.cleaned.swap(true, Ordering::AcqRel) {
        call(s, 4, block, 0, core::ptr::null_mut());
    }
}

pub unsafe fn begin(block: *mut Block, ctx: *mut c_void, identity: usize, _name: *const u8, provider: Provider) -> i32 {
    if block.is_null() {
        return E_INVALIDARG;
    }
    if let Some(s) = state(block as usize) {
        if s.status.lock().unwrap_or_else(|e| e.into_inner()).0 == E_PENDING {
            return E_INVALIDARG;
        }
    }
    let qh = (*block).queue;
    let early = if qh == 0 && process_queue() == 0 {
        E_NO_TASK_QUEUE
    } else if queue(qh).is_some_and(|q| q.ended.load(Ordering::Acquire) || q.work.terminated.load(Ordering::Acquire)) {
        E_ABORT
    } else {
        S_OK
    };
    if early < 0 {
        states().lock().unwrap_or_else(|e| e.into_inner()).remove(&(block as usize));
        mark(block, early, false);
        return early;
    }
    let s = Arc::new(AState {
        provider,
        ctx: ctx as usize,
        identity,
        status: Mutex::new((E_PENDING, 0)),
        cv: Condvar::new(),
        signaled: AtomicBool::new(false),
        cleaned: AtomicBool::new(false),
        queue: (*block).queue,
        in_cancel: AtomicBool::new(false),
        cleanup_after_cancel: AtomicBool::new(false),
        scheduled: AtomicBool::new(false),
        in_begin: AtomicBool::new(true),
        data: Box::new(core::cell::UnsafeCell::new(ProviderData { block: block as *mut c_void, buffer_size: 0, buffer: core::ptr::null_mut(), context: ctx })),
    });
    states().lock().unwrap_or_else(|e| e.into_inner()).insert(block as usize, s.clone());
    mark(block, E_PENDING, true);
    let hr = call(&s, 0, block, 0, core::ptr::null_mut());
    if hr < 0 && hr != E_PENDING {
        complete_state(&s, block, hr, 0);
    }
    s.in_begin.store(false, Ordering::Release);
    S_OK
}

pub unsafe fn schedule(block: *mut Block, delay: u32) -> i32 {
    if block.is_null() {
        return E_INVALIDARG;
    }
    let Some(s) = state(block as usize) else {
        return marked(block).filter(|hr| *hr < 0 && *hr != E_PENDING).unwrap_or(E_INVALIDARG);
    };
    match *s.status.lock().unwrap_or_else(|e| e.into_inner()) {
        (E_PENDING, _) => {}
        (hr, _) if hr < 0 => return hr,
        (_, 0) => return E_INVALIDARG,
        _ => {}
    }
    if s.scheduled.swap(true, Ordering::AcqRel) {
        return E_UNEXPECTED;
    }
    let owner = s.clone();
    let b = SendPtr(block as usize);
    let job: Job = Box::new(move |canceled| {
        owner.scheduled.store(false, Ordering::Release);
        let block = b.0 as *mut Block;
        let Some(s) = state(block as usize).filter(|c| Arc::ptr_eq(c, &owner)) else { return };
        if canceled {
            cancel(block);
            complete(block, E_ABORT, 0);
            return;
        }
        let hr = call(&s, 1, block, 0, core::ptr::null_mut());
        if hr != E_PENDING {
            complete_state(&s, block, if hr >= 0 { E_UNEXPECTED } else { hr }, 0);
        }
    });
    submit_delayed(s.queue, 0, delay, job)
}

pub unsafe fn complete(block: *mut Block, hr: i32, size: usize) {
    if hr == E_PENDING {
        return;
    }
    let Some(s) = state(block as usize) else { return };
    complete_state(&s, block, hr, size)
}

unsafe fn complete_state(s: &Arc<AState>, block: *mut Block, hr: i32, size: usize) {
    if hr == E_PENDING {
        return;
    }
    let keep = hr >= 0 && size > 0;
    let (callback, in_cancel) = {
        let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        if st.0 != E_PENDING {
            return;
        }
        *st = (hr, size);
        mark(block, hr, keep);
        if !keep {
            let mut m = states().lock().unwrap_or_else(|e| e.into_inner());
            if m.get(&(block as usize)).is_some_and(|c| Arc::ptr_eq(c, &s)) {
                m.remove(&(block as usize));
            }
        }
        let in_cancel = s.in_cancel.load(Ordering::Acquire);
        if !keep && in_cancel && (*block).callback.is_none() {
            s.cleanup_after_cancel.store(true, Ordering::Release);
        }
        ((*block).callback, in_cancel)
    };
    let after_callback = !keep && callback.is_some() && (in_cancel || s.in_begin.load(Ordering::Acquire));
    if !keep && !after_callback && !in_cancel {
        cleanup(&s, block);
    }
    match callback {
        Some(cb) => {
            let b = SendPtr(block as usize);
            let done = s.clone();
            let job: Job = Box::new(move |_| {
                cb(b.0 as *mut c_void);
                if after_callback {
                    cleanup(&done, b.0 as *mut Block);
                }
                done.signal();
            });
            match queued(s.queue, 1, job) {
                Queued::Ready(p, tag, j) => p.submit_tagged(tag, 1, j),
                Queued::Aborted(j) => j(true),
                Queued::Invalid(_) => {
                    if after_callback {
                        cleanup(s, block);
                    }
                    s.signal()
                }
            }
        }
        None => s.signal(),
    }
}

pub unsafe fn get_status(block: *mut Block, wait: bool) -> i32 {
    if block.is_null() {
        return E_INVALIDARG;
    }
    match state(block as usize) {
        Some(s) => {
            let mut st = s.status.lock().unwrap_or_else(|e| e.into_inner());
            while wait && !s.signaled.load(Ordering::Acquire) {
                st = s.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            st.0
        }
        None => match marked(block).unwrap_or_else(|| raw_status(block)) {
            E_PENDING if wait => E_INVALIDARG,
            hr => hr,
        },
    }
}

unsafe fn raw_status(block: *const Block) -> i32 {
    let i = &(*block).internal;
    i32::from_le_bytes([i[8], i[9], i[10], i[11]])
}

pub unsafe fn get_result_size(block: *mut Block, out: *mut usize) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    match state(block as usize) {
        Some(s) => {
            let st = *s.status.lock().unwrap_or_else(|e| e.into_inner());
            if st.0 < 0 || st.0 == E_PENDING {
                *out = 0;
                return st.0;
            }
            *out = st.1;
            S_OK
        }
        None => {
            *out = 0;
            marked(block).unwrap_or_else(|| raw_status(block))
        }
    }
}

pub unsafe fn get_result(block: *mut Block, identity: usize, size: usize, buf: *mut c_void, used: *mut usize) -> i32 {
    let Some(s) = state(block as usize) else {
        let mark = marked(block);
        let hr = mark.unwrap_or_else(|| raw_status(block));
        if hr >= 0 && mark.is_some() && (*block).internal[9] == 1 {
            return 0x8000_000Eu32 as i32;
        }
        if hr >= 0 && !used.is_null() {
            *used = 0;
        }
        return hr;
    };
    let (hr, need) = *s.status.lock().unwrap_or_else(|e| e.into_inner());
    if hr < 0 || hr == E_PENDING {
        return hr;
    }
    let r = if identity != s.identity || buf.is_null() {
        E_INVALIDARG
    } else if size < need {
        E_NOT_SUFFICIENT_BUFFER
    } else {
        if !used.is_null() {
            *used = need;
        }
        call(&s, 2, block, need, buf)
    };
    if r == E_PENDING {
        return r;
    }
    cleanup(&s, block);
    states().lock().unwrap_or_else(|e| e.into_inner()).remove(&(block as usize));
    mark(block, hr, false);
    (*block).internal[9] = 1;
    r
}

pub unsafe fn cancel(block: *mut Block) {
    let Some(s) = state(block as usize) else { return };
    if s.status.lock().unwrap_or_else(|e| e.into_inner()).0 != E_PENDING {
        return;
    }
    if s.in_cancel.swap(true, Ordering::AcqRel) {
        return;
    }
    call(&s, 3, block, 0, core::ptr::null_mut());
    let deferred = {
        let _st = s.status.lock().unwrap_or_else(|e| e.into_inner());
        s.in_cancel.store(false, Ordering::Release);
        s.cleanup_after_cancel.load(Ordering::Acquire)
    };
    if deferred {
        cleanup(&s, block);
    }
}

unsafe extern "system" fn run_provider(op: u32, d: *const ProviderData) -> i32 {
    if op == 1 {
        let w: AsyncWork = core::mem::transmute((*d).context);
        complete((*d).block as *mut Block, w((*d).block), 0);
    }
    S_OK
}

unsafe extern "system" fn a_get_status(_t: This, b: *mut Block, wait: bool) -> i32 {
    get_status(b, wait)
}
unsafe extern "system" fn a_get_result_size(_t: This, b: *mut Block, out: *mut usize) -> i32 {
    get_result_size(b, out)
}
unsafe extern "system" fn a_cancel(_t: This, b: *mut Block) {
    cancel(b)
}
unsafe extern "system" fn a_run(_t: This, b: *mut Block, work: Option<AsyncWork>) -> i32 {
    let Some(w) = work else { return E_INVALIDARG };
    let hr = begin(b, w as *mut c_void, a_run as usize, core::ptr::null(), run_provider);
    if hr < 0 {
        return hr;
    }
    schedule(b, 0)
}
unsafe extern "system" fn a_begin(_t: This, b: *mut Block, ctx: *mut c_void, identity: usize, name: *const u8, p: Option<Provider>) -> i32 {
    let Some(p) = p else { return E_INVALIDARG };
    begin(b, ctx, identity, name, p)
}
unsafe extern "system" fn a_schedule(_t: This, b: *mut Block, delay: u32) -> i32 {
    schedule(b, delay)
}
unsafe extern "system" fn a_complete(_t: This, b: *mut Block, hr: i32, size: usize) {
    complete(b, hr, size)
}
unsafe extern "system" fn a_get_result(_t: This, b: *mut Block, identity: usize, size: usize, buf: *mut c_void, used: *mut usize) -> i32 {
    get_result(b, identity, size, buf, used)
}

pub fn interfaces() -> Vec<Iface> {
    vec![Iface {
        clsid: XASYNC,
        iid: XASYNC,
        name: "XAsync/XTaskQueue/XThread(073b7dcb, emulated)",
        slots: vec![
            (3, a_get_status as usize),
            (4, a_get_result_size as usize),
            (5, a_cancel as usize),
            (6, a_run as usize),
            (7, a_begin as usize),
            (9, a_schedule as usize),
            (10, a_complete as usize),
            (11, a_get_result as usize),
            (12, q_create as usize),
            (13, q_create_composite as usize),
            (14, q_get_port as usize),
            (15, q_duplicate as usize),
            (16, q_dispatch as usize),
            (17, q_close as usize),
            (18, q_submit as usize),
            (19, q_submit_delayed as usize),
            (20, q_register_waiter as usize),
            (21, q_unregister_waiter as usize),
            (22, q_terminate as usize),
            (23, q_register_monitor as usize),
            (24, q_unregister_monitor as usize),
            (25, q_get_process as usize),
            (26, q_set_process as usize),
            (27, thread_set_time_sensitive as usize),
            (29, thread_assert_not_time_sensitive as usize),
            (30, thread_is_time_sensitive as usize),
        ],
    }]
}

#[cfg(test)]
mod port_tests {
    use super::*;
    use std::sync::mpsc;

    fn running_serial_port(users: usize) -> Arc<Port> {
        let port = Port::new(SERIALIZED);
        port.users.store(users, Ordering::Release);
        let (tx, rx) = mpsc::channel();
        port.submit(Box::new(move |canceled| tx.send(canceled).unwrap()));
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        port
    }

    fn assert_worker_stopped(port: &Arc<Port>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(port) != 1 {
            assert!(Instant::now() < deadline, "serialized worker still holds its port");
            std::thread::yield_now();
        }
        assert!(!port.serial_started.load(Ordering::Acquire));
    }

    #[test]
    fn serialized_worker_exits_after_last_user_releases_the_port() {
        let port = running_serial_port(1);
        port.release();
        assert_worker_stopped(&port);
        assert!(!port.terminated.load(Ordering::Acquire));
    }

    #[test]
    fn released_serialized_port_runs_queued_callbacks_then_exits() {
        let port = Port::new(SERIALIZED);
        port.users.store(1, Ordering::Release);
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        port.submit(Box::new(move |_| {
            started_tx.send(()).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (queued_tx, queued_rx) = mpsc::channel();
        port.submit(Box::new(move |canceled| queued_tx.send(canceled).unwrap()));
        port.release();
        finish_tx.send(()).unwrap();
        assert!(!queued_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        assert_worker_stopped(&port);
    }

    #[test]
    fn serialized_worker_survives_release_of_one_shared_user() {
        let port = running_serial_port(2);
        port.release();
        assert!(!port.terminated.load(Ordering::Acquire));
        let (tx, rx) = mpsc::channel();
        port.submit(Box::new(move |canceled| tx.send(canceled).unwrap()));
        assert!(!rx.recv_timeout(Duration::from_secs(2)).unwrap());
        port.release();
        assert_worker_stopped(&port);
    }

    #[test]
    fn serialized_completion_marker_restarts_a_released_port_worker() {
        let port = running_serial_port(1);
        port.release();
        assert_worker_stopped(&port);
        let (tx, rx) = mpsc::channel();
        port.submit_marker(0, 0, Box::new(move |_| tx.send(()).unwrap()));
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_worker_stopped(&port);
    }

    #[test]
    fn terminating_a_live_serialized_port_still_delivers_its_marker() {
        let port = Port::new(SERIALIZED);
        port.users.store(1, Ordering::Release);
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        port.submit(Box::new(move |_| {
            started_tx.send(()).unwrap();
            finish_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let (cancel_tx, cancel_rx) = mpsc::channel();
        port.submit(Box::new(move |canceled| cancel_tx.send(canceled).unwrap()));
        port.cancel_all();
        assert!(cancel_rx.recv_timeout(Duration::from_secs(2)).unwrap());

        let (marker_tx, marker_rx) = mpsc::channel();
        port.submit_marker(0, 0, Box::new(move |_| marker_tx.send(()).unwrap()));
        assert!(matches!(marker_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        finish_tx.send(()).unwrap();
        marker_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        port.release();
        assert_worker_stopped(&port);
    }

    extern "system" {
        fn CreateEventW(attrs: *const c_void, manual: i32, initial: i32, name: *const u16) -> usize;
        fn SetEvent(h: usize) -> i32;
        fn ResetEvent(h: usize) -> i32;
        fn CloseHandle(h: usize) -> i32;
    }

    struct Fired {
        n: AtomicUsize,
        event: usize,
        stop_at: usize,
        queue: AtomicUsize,
        token: AtomicUsize,
    }

    unsafe extern "system" fn reset_at_limit(ctx: *mut c_void, canceled: bool) {
        let f = &*(ctx as *const Fired);
        if !canceled && f.n.fetch_add(1, Ordering::AcqRel) + 1 == f.stop_at {
            ResetEvent(f.event);
        }
    }

    unsafe extern "system" fn unregister_self(ctx: *mut c_void, _canceled: bool) {
        let f = &*(ctx as *const Fired);
        f.n.fetch_add(1, Ordering::AcqRel);
        q_unregister_waiter(core::ptr::null(), f.queue.load(Ordering::Acquire), f.token.load(Ordering::Acquire) as u64);
    }

    fn wait_count(f: &Fired, want: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while f.n.load(Ordering::Acquire) < want {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    #[test]
    fn manual_reset_waiter_fires_again_after_each_callback_until_reset() {
        unsafe {
            let ev = CreateEventW(core::ptr::null(), 1, 0, core::ptr::null());
            let f = Box::new(Fired { n: AtomicUsize::new(0), event: ev, stop_at: 3, queue: AtomicUsize::new(0), token: AtomicUsize::new(0) });
            let mut q = 0usize;
            assert_eq!(q_create(core::ptr::null(), THREAD_POOL, THREAD_POOL, &mut q), S_OK);
            let mut token = 0u64;
            assert_eq!(q_register_waiter(core::ptr::null(), q, 0, ev, &*f as *const Fired as *mut c_void, Some(reset_at_limit), &mut token), S_OK);
            SetEvent(ev);
            assert!(wait_count(&f, 3));
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(f.n.load(Ordering::Acquire), 3);
            q_unregister_waiter(core::ptr::null(), q, token);
            SetEvent(ev);
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(f.n.load(Ordering::Acquire), 3);
            q_close(core::ptr::null(), q);
            CloseHandle(ev);
        }
    }

    #[test]
    fn waiter_unregistered_from_its_own_immediate_callback_stops() {
        unsafe {
            let ev = CreateEventW(core::ptr::null(), 0, 0, core::ptr::null());
            let f = Box::new(Fired { n: AtomicUsize::new(0), event: ev, stop_at: 0, queue: AtomicUsize::new(0), token: AtomicUsize::new(0) });
            let mut q = 0usize;
            assert_eq!(q_create(core::ptr::null(), IMMEDIATE, IMMEDIATE, &mut q), S_OK);
            f.queue.store(q, Ordering::Release);
            let mut token = 0u64;
            assert_eq!(q_register_waiter(core::ptr::null(), q, 0, ev, &*f as *const Fired as *mut c_void, Some(unregister_self), &mut token), S_OK);
            f.token.store(token as usize, Ordering::Release);
            SetEvent(ev);
            assert!(wait_count(&f, 1));
            SetEvent(ev);
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(f.n.load(Ordering::Acquire), 1);
            assert!(waiters().lock().unwrap().get(&token).is_none());
            q_close(core::ptr::null(), q);
            CloseHandle(ev);
        }
    }

    #[test]
    fn manual_completion_infinite_dispatch_waits_for_termination_marker() {
        unsafe extern "system" fn noop_task(_ctx: *mut c_void, _canceled: bool) {}
        unsafe extern "system" fn on_terminated(ctx: *mut c_void) {
            let fired = Arc::from_raw(ctx as *const AtomicBool);
            fired.store(true, Ordering::Release);
        }

        let mut q = 0usize;
        assert_eq!(unsafe { q_create(core::ptr::null_mut(), MANUAL, MANUAL, &mut q) }, S_OK);
        assert_eq!(unsafe { q_submit(core::ptr::null_mut(), q, 0, core::ptr::null_mut(), Some(noop_task)) }, S_OK);

        let comp_thread = std::thread::spawn(move || {
            while unsafe { q_dispatch(core::ptr::null_mut(), q, 1, u32::MAX) } {}
        });

        let fired = Arc::new(AtomicBool::new(false));
        let fired_raw = Arc::into_raw(fired.clone()) as *mut c_void;
        assert_eq!(unsafe { q_terminate(core::ptr::null_mut(), q, false, fired_raw, Some(on_terminated)) }, S_OK);

        std::thread::sleep(Duration::from_millis(50));
        assert!(!fired.load(Ordering::Acquire));
        assert!(!comp_thread.is_finished());

        while unsafe { q_dispatch(core::ptr::null_mut(), q, 0, 0) } {}

        let deadline = Instant::now() + Duration::from_secs(2);
        while !comp_thread.is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(comp_thread.is_finished());
        comp_thread.join().unwrap();
        assert!(fired.load(Ordering::Acquire));
        unsafe { q_close(core::ptr::null_mut(), q) };
    }

    #[test]
    fn composite_infinite_dispatch_exits_when_composite_terminates_without_stopping_base() {
        let mut q = 0usize;
        assert_eq!(unsafe { q_create(core::ptr::null_mut(), MANUAL, IMMEDIATE, &mut q) }, S_OK);
        let (mut wp, mut cp) = (0usize, 0usize);
        assert_eq!(unsafe { q_get_port(core::ptr::null_mut(), q, 0, &mut wp) }, S_OK);
        assert_eq!(unsafe { q_get_port(core::ptr::null_mut(), q, 1, &mut cp) }, S_OK);
        let mut comp = 0usize;
        assert_eq!(unsafe { q_create_composite(core::ptr::null_mut(), wp, cp, &mut comp) }, S_OK);

        let t_base = std::thread::spawn(move || {
            while unsafe { q_dispatch(core::ptr::null_mut(), q, 0, u32::MAX) } {}
        });
        let t_comp = std::thread::spawn(move || {
            while unsafe { q_dispatch(core::ptr::null_mut(), comp, 0, u32::MAX) } {}
        });

        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(unsafe { q_terminate(core::ptr::null_mut(), comp, true, core::ptr::null_mut(), None) }, S_OK);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !t_comp.is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(t_comp.is_finished());
        t_comp.join().unwrap();

        std::thread::sleep(Duration::from_millis(50));
        assert!(!t_base.is_finished());

        assert_eq!(unsafe { q_terminate(core::ptr::null_mut(), q, true, core::ptr::null_mut(), None) }, S_OK);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !t_base.is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(t_base.is_finished());
        t_base.join().unwrap();

        unsafe {
            q_close(core::ptr::null_mut(), comp);
            q_close(core::ptr::null_mut(), q);
        }
    }
}
