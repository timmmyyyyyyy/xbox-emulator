use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use compat_common::record;

use crate::device_wrap::{DeviceWrapper, Guid};

const DESC_SIZE: usize = 0x290;
const CS_DESC_SIZE: usize = 0x38;
const NULL_CS: &str = "[numthreads(1, 1, 1)] void main() {}";
const XBOX_SLOT_RESET: usize = 10;
const XBOX_SLOT_CLEAR_STATE: usize = 11;
const XBOX_SLOT_SET_PSO: usize = 25;
const IID_PSO: Guid = Guid { d1: 0x765a_30f3, d2: 0xf624, d3: 0x4c6f, d4: [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45] };
const IID_IUNKNOWN: Guid = Guid { d1: 0, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const GUID_TB_PLACEHOLDER: Guid = Guid { d1: 0x7b0b_a5c1, d2: 0x3d1e, d3: 0x4f62, d4: [0x9a, 0x11, 0x54, 0x0b, 0x5e, 0x21, 0x8c, 0x07] };
const NULL_VS: &str = "float4 main() : SV_Position { return float4(2.0, 2.0, -1.0, 1.0); }";

type CreateFn = unsafe extern "system" fn(*mut DeviceWrapper, *const u8, u32, *const u8, *const Guid, *mut *mut c_void) -> i32;

#[derive(Default)]
struct Done {
    finished: bool,
    hr: i32,
    real: usize,
    detached: bool,
    orphan: bool,
    started: Option<std::time::Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Graphics,
    Compute,
}

struct Slot {
    st: Mutex<Done>,
    cv: Condvar,
    real: AtomicUsize,
    kind: Kind,
    retry: Mutex<Option<Retry>>,
    aliased: AtomicBool,
    job: Mutex<Option<Job>>,
    refused: AtomicBool,
    bad_key: Option<u128>,
}

struct Retry {
    owned: Owned,
    sync: CreateFn,
    dev: usize,
    num_ext: u32,
    riid: Guid,
}

struct Owned {
    desc: Vec<u8>,
    _blobs: Vec<Vec<u8>>,
    _layout: Vec<u8>,
    _names: Vec<Vec<u8>>,
    _cached: Vec<u8>,
    ext: Vec<u8>,
    root: usize,
}

unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.root != 0 {
            unsafe { com_release(self.root as *mut c_void) };
        }
    }
}

static ASYNC_N: AtomicUsize = AtomicUsize::new(0);
static INLINE_N: AtomicUsize = AtomicUsize::new(0);
static PENDING_BINDS: AtomicU64 = AtomicU64::new(0);
static SWAPPED_BINDS: AtomicU64 = AtomicU64::new(0);
static LATE_FAIL: AtomicUsize = AtomicUsize::new(0);
static RETRY_OK: AtomicUsize = AtomicUsize::new(0);
static FINAL_FAIL: AtomicUsize = AtomicUsize::new(0);
static REFUSED_BINDS: AtomicU64 = AtomicU64::new(0);
static SYNC_AFTER_FAIL: AtomicUsize = AtomicUsize::new(0);
static COMPUTE_WAIT_US: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PREV_SET: AtomicUsize = AtomicUsize::new(0);
static PREV_RESET: AtomicUsize = AtomicUsize::new(0);
static PREV_CLEAR: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);

fn bad_descs() -> &'static Mutex<std::collections::HashSet<u128>> {
    static B: OnceLock<Mutex<std::collections::HashSet<u128>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

fn note_bad(k: Option<u128>) {
    if let Some(k) = k {
        bad_descs().lock().unwrap_or_else(|e| e.into_inner()).insert(k);
    }
}

fn known_bad(k: Option<u128>, kind: Kind) -> bool {
    let Some(k) = k else { return false };
    if !bad_descs().lock().unwrap_or_else(|e| e.into_inner()).contains(&k) {
        return false;
    }
    let n = SYNC_AFTER_FAIL.fetch_add(1, Ordering::Relaxed);
    if n < 20 {
        record("d3d12_xs", "pso_async", &format!("{kind:?} PSO desc {k:032x} already failed after an async S_OK; this creation runs synchronously so the title receives the real HRESULT (#{})", n + 1));
    }
    true
}

fn map() -> &'static Mutex<HashMap<usize, Arc<Slot>>> {
    static M: OnceLock<Mutex<HashMap<usize, Arc<Slot>>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn real_of(placeholder: usize) -> usize {
    map().lock().ok().and_then(|m| m.get(&placeholder).map(|s| s.real.load(Ordering::Acquire))).unwrap_or(0)
}

pub fn is_compute_placeholder(p: usize) -> bool {
    map().lock().ok().is_some_and(|m| m.get(&p).is_some_and(|s| s.kind == Kind::Compute))
}

pub fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        let on = std::env::var("TOMBOX_PSO_ASYNC").map(|v| v != "0").unwrap_or(true);
        record("d3d12_xs", "pso_async", if on {
            "graphics and compute PSOs not ready within the budget keep building on worker threads; the title gets a handle that never reaches the GPU: every SetPipelineState/Reset/ClearState with it waits for the real PSO and binds that, and a background failure is retried on the title thread at bind (TOMBOX_PSO_ASYNC=0 creates them on the title thread)"
        } else {
            "disabled (TOMBOX_PSO_ASYNC=0): PSOs are created on the title thread"
        });
        on
    })
}

fn budget() -> std::time::Duration {
    static B: OnceLock<u64> = OnceLock::new();
    std::time::Duration::from_micros(*B.get_or_init(|| std::env::var("TOMBOX_PSO_ASYNC_MS").ok().and_then(|v| v.parse::<f64>().ok()).map(|ms| (ms * 1000.0) as u64).unwrap_or(12_000)))
}

type Job = Box<dyn FnOnce() + Send>;

fn pool() -> &'static Mutex<std::sync::mpsc::Sender<Job>> {
    static P: OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> = OnceLock::new();
    P.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        for _ in 0..cores.saturating_sub(2).clamp(1, 6) {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name("tombox-pso-async".into()).stack_size(64 << 20).spawn(move || loop {
                let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                match job {
                    Ok(j) => j(),
                    Err(_) => break,
                }
            });
        }
        Mutex::new(tx)
    })
}

unsafe fn vslot(obj: *mut c_void, i: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(i) }
}

unsafe fn com_addref(obj: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(vslot(obj, 1)) };
    unsafe { f(obj) };
}

unsafe fn com_release(obj: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(vslot(obj, 2)) };
    unsafe { f(obj) };
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

fn wr64(d: &mut [u8], o: usize, v: u64) {
    d[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

fn wr32(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

fn eligible(d: &[u8]) -> bool {
    let no_tess_gs = [0x28usize, 0x38, 0x48].iter().all(|&o| rd64(d, o) == 0 || rd64(d, o + 8) == 0);
    let no_so = rd64(d, 0x58) == 0 || rd32(d, 0x60) == 0;
    let topo = rd32(d, 0x23c);
    no_tess_gs && no_so && (1..=3).contains(&topo) && rd64(d, 0x08) != 0 && rd64(d, 0x10) != 0 && rd64(d, 0) != 0
}

pub unsafe fn sync_reason(pdesc: *const u8, riid: *const Guid, compute: bool) -> &'static str {
    if !enabled() {
        return "TOMBOX_PSO_ASYNC=0";
    }
    if !HOOKED.load(Ordering::Acquire) {
        return "async PSO hooks not installed yet";
    }
    if pdesc.is_null() || riid.is_null() || unsafe { *riid != IID_PSO } {
        return "riid is not ID3D12PipelineState";
    }
    if compute {
        let d = unsafe { core::slice::from_raw_parts(pdesc, CS_DESC_SIZE) };
        return if rd64(d, 0) == 0 { "null root signature" } else if rd64(d, 8) == 0 || rd64(d, 16) == 0 { "no compute shader" } else { "shader over 4 MB" };
    }
    let d = unsafe { core::slice::from_raw_parts(pdesc, DESC_SIZE) };
    if ![0x28usize, 0x38, 0x48].iter().all(|&o| rd64(d, o) == 0 || rd64(d, o + 8) == 0) {
        "has a hull, domain or geometry shader"
    } else if rd64(d, 0x58) != 0 && rd32(d, 0x60) != 0 {
        "stream output"
    } else if !(1..=3).contains(&rd32(d, 0x23c)) {
        "topology type is not point, line or triangle"
    } else if rd64(d, 0x08) == 0 || rd64(d, 0x10) == 0 {
        "no vertex shader"
    } else if rd64(d, 0) == 0 {
        "null root signature"
    } else {
        "snapshot refused (shader over 4 MB or more than 64 input elements)"
    }
}

unsafe fn snapshot(pdesc: *const u8, num_ext: u32, pext: *const u8) -> Option<Owned> {
    let mut desc = unsafe { core::slice::from_raw_parts(pdesc, DESC_SIZE) }.to_vec();
    let mut blobs = Vec::new();
    for o in [0x08usize, 0x18] {
        let (p, n) = (rd64(&desc, o) as *const u8, rd64(&desc, o + 8) as usize);
        if p.is_null() || n == 0 {
            continue;
        }
        if n > 0x40_0000 {
            return None;
        }
        let b = unsafe { core::slice::from_raw_parts(p, n) }.to_vec();
        wr64(&mut desc, o, b.as_ptr() as u64);
        blobs.push(b);
    }
    let (lp, ln) = (rd64(&desc, 0x228) as *const u8, rd32(&desc, 0x230) as usize);
    let mut layout = Vec::new();
    let mut names = Vec::new();
    if !lp.is_null() && ln > 0 {
        if ln > 64 {
            return None;
        }
        layout = unsafe { core::slice::from_raw_parts(lp, ln * 32) }.to_vec();
        for i in 0..ln {
            let np = rd64(&layout, i * 32) as *const u8;
            if np.is_null() {
                continue;
            }
            let mut s = Vec::new();
            let mut k = 0;
            while k < 256 && unsafe { *np.add(k) } != 0 {
                s.push(unsafe { *np.add(k) });
                k += 1;
            }
            s.push(0);
            wr64(&mut layout, i * 32, s.as_ptr() as u64);
            names.push(s);
        }
        wr64(&mut desc, 0x228, layout.as_ptr() as u64);
    }
    let (cp, cn) = (rd64(&desc, 0x278) as *const u8, rd64(&desc, 0x280) as usize);
    let mut cached = Vec::new();
    if !cp.is_null() && cn > 0 && cn < 0x100_0000 {
        cached = unsafe { core::slice::from_raw_parts(cp, cn) }.to_vec();
        wr64(&mut desc, 0x278, cached.as_ptr() as u64);
    } else {
        wr64(&mut desc, 0x278, 0);
        wr64(&mut desc, 0x280, 0);
    }
    let ext = if pext.is_null() || num_ext == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(pext, num_ext as usize * crate::pso_ext::EXT_STRIDE) }.to_vec() };
    let root = rd64(&desc, 0) as usize;
    unsafe { com_addref(root as *mut c_void) };
    Some(Owned { desc, _blobs: blobs, _layout: layout, _names: names, _cached: cached, ext, root })
}

fn null_cs() -> Option<&'static [u8]> {
    static V: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    V.get_or_init(|| match tombox_xshader::dxc::compile(NULL_CS, "cs_6_0") {
        Ok(v) => Some(v),
        Err(e) => {
            record("d3d12_xs", "pso_async", &format!("placeholder compute shader does not compile ({e}); compute PSOs stay synchronous"));
            None
        }
    })
    .as_deref()
}

unsafe fn create_compute_placeholder(real_dev: *mut c_void, root: *mut c_void) -> Result<*mut c_void, i32> {
    let cs = null_cs().ok_or(-1)?;
    let mut d = vec![0u8; CS_DESC_SIZE];
    wr64(&mut d, 0, root as u64);
    wr64(&mut d, 8, cs.as_ptr() as u64);
    wr64(&mut d, 16, cs.len() as u64);
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(real_dev, 11)) };
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { f(real_dev, d.as_ptr(), &IID_PSO, &mut out) };
    if hr < 0 || out.is_null() {
        return Err(hr);
    }
    Ok(out)
}

fn null_vs() -> Option<&'static [u8]> {
    static V: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    V.get_or_init(|| match tombox_xshader::dxc::compile(NULL_VS, "vs_6_0") {
        Ok(v) => Some(v),
        Err(e) => {
            record("d3d12_xs", "pso_async", &format!("placeholder vertex shader does not compile ({e}); PSOs stay synchronous"));
            None
        }
    })
    .as_deref()
}

unsafe fn create_placeholder(real_dev: *mut c_void, head: &[u8], riid: *const Guid) -> Result<*mut c_void, i32> {
    let vs = null_vs().ok_or(-1)?;
    let mut d = head.to_vec();
    wr64(&mut d, 0x08, vs.as_ptr() as u64);
    wr64(&mut d, 0x10, vs.len() as u64);
    wr64(&mut d, 0x18, 0);
    wr64(&mut d, 0x20, 0);
    wr64(&mut d, 0x228, 0);
    wr32(&mut d, 0x230, 0);
    wr32(&mut d, 0x1dc, 1);
    wr32(&mut d, 0x1ec, 0);
    wr32(&mut d, 0x1f4, 0);
    wr32(&mut d, 0x1fc, 0);
    wr64(&mut d, 0x278, 0);
    wr64(&mut d, 0x280, 0);
    wr32(&mut d, 0x288, 0);
    unsafe { crate::cl2_state::patch_gfx_pso_desc(real_dev, &mut d) };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(real_dev, 10)) };
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { f(real_dev, d.as_ptr(), riid, &mut out) };
    if hr < 0 || out.is_null() {
        return Err(hr);
    }
    Ok(out)
}

#[repr(C)]
struct Watch {
    vt: &'static WatchVt,
    refs: AtomicU32,
    key: usize,
}

#[repr(C)]
struct WatchVt {
    qi: unsafe extern "system" fn(*mut Watch, *const Guid, *mut *mut c_void) -> i32,
    add: unsafe extern "system" fn(*mut Watch) -> u32,
    rel: unsafe extern "system" fn(*mut Watch) -> u32,
}

static WATCH_VT: WatchVt = WatchVt { qi: watch_qi, add: watch_add, rel: watch_rel };

unsafe extern "system" fn watch_qi(this: *mut Watch, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() {
        return 0x8000_4003u32 as i32;
    }
    if !riid.is_null() && unsafe { *riid == IID_IUNKNOWN } {
        unsafe { watch_add(this) };
        unsafe { *ppv = this as *mut c_void };
        return 0;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    0x8000_4002u32 as i32
}

unsafe extern "system" fn watch_add(this: *mut Watch) -> u32 {
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn watch_rel(this: *mut Watch) -> u32 {
    let n = unsafe { (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1 };
    if n == 0 {
        let key = unsafe { (*this).key };
        drop(unsafe { Box::from_raw(this) });
        placeholder_destroyed(key);
    }
    n
}

fn placeholder_destroyed(key: usize) {
    crate::dred_marks::forget(key);
    let Some(slot) = crate::frame_ev::lock(map()).remove(&key) else { return };
    LIVE.fetch_sub(1, Ordering::Relaxed);
    let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
    if g.finished {
        if g.real != 0 {
            unsafe { com_release(g.real as *mut c_void) };
            g.real = 0;
            slot.real.store(0, Ordering::Release);
        }
    } else {
        g.orphan = true;
    }
}

unsafe fn watch(placeholder: *mut c_void) -> bool {
    let w = Box::into_raw(Box::new(Watch { vt: &WATCH_VT, refs: AtomicU32::new(1), key: placeholder as usize }));
    let f: unsafe extern "system" fn(*mut c_void, *const Guid, *mut c_void) -> i32 = unsafe { core::mem::transmute(vslot(placeholder, 5)) };
    let hr = unsafe { f(placeholder, &GUID_TB_PLACEHOLDER, w as *mut c_void) };
    unsafe { watch_rel(w) };
    hr >= 0
}

pub unsafe fn create_gfx(this: *mut DeviceWrapper, real_dev: *mut c_void, pdesc: *const u8, num_ext: u32, pext: *const u8, riid: *const Guid, ppv: *mut *mut c_void, sync: CreateFn) -> Option<i32> {
    if !enabled() || pdesc.is_null() || ppv.is_null() || riid.is_null() || unsafe { *riid != IID_PSO } {
        return None;
    }
    let head = unsafe { core::slice::from_raw_parts(pdesc, DESC_SIZE) };
    if !eligible(head) || !HOOKED.load(Ordering::Acquire) {
        return None;
    }
    let key = unsafe { crate::pso_lib::gfx_key(head, rd64(head, 0), "pso_async") };
    if known_bad(key, Kind::Graphics) {
        return None;
    }
    let owned = unsafe { snapshot(pdesc, num_ext, pext) }?;
    let rd = real_dev as usize;
    let ph_head = head.to_vec();
    unsafe { launch(this, owned, num_ext, riid, ppv, sync, Kind::Graphics, key, &mut || create_placeholder(rd as *mut c_void, &ph_head, &IID_PSO)) }
}

pub unsafe fn create_compute(this: *mut DeviceWrapper, real_dev: *mut c_void, pdesc: *const u8, num_ext: u32, pext: *const u8, riid: *const Guid, ppv: *mut *mut c_void, sync: CreateFn) -> Option<i32> {
    if !enabled() || pdesc.is_null() || ppv.is_null() || riid.is_null() || unsafe { *riid != IID_PSO } || !HOOKED.load(Ordering::Acquire) {
        return None;
    }
    let head = unsafe { core::slice::from_raw_parts(pdesc, CS_DESC_SIZE) };
    let key = unsafe { crate::pso_lib::compute_key(head, rd64(head, 0), "pso_async") };
    if known_bad(key, Kind::Compute) {
        return None;
    }
    let owned = unsafe { snapshot_compute(pdesc, num_ext, pext) }?;
    let rd = real_dev as usize;
    let root = owned.root;
    unsafe { launch(this, owned, num_ext, riid, ppv, sync, Kind::Compute, key, &mut || create_compute_placeholder(rd as *mut c_void, root as *mut c_void)) }
}

unsafe fn snapshot_compute(pdesc: *const u8, num_ext: u32, pext: *const u8) -> Option<Owned> {
    let mut desc = unsafe { core::slice::from_raw_parts(pdesc, CS_DESC_SIZE) }.to_vec();
    let root = rd64(&desc, 0) as usize;
    let (p, n) = (rd64(&desc, 8) as *const u8, rd64(&desc, 16) as usize);
    if root == 0 || p.is_null() || n == 0 || n > 0x40_0000 {
        return None;
    }
    let blob = unsafe { core::slice::from_raw_parts(p, n) }.to_vec();
    wr64(&mut desc, 8, blob.as_ptr() as u64);
    let (cp, cn) = (rd64(&desc, 0x20) as *const u8, rd64(&desc, 0x28) as usize);
    let mut cached = Vec::new();
    if !cp.is_null() && cn > 0 && cn < 0x100_0000 {
        cached = unsafe { core::slice::from_raw_parts(cp, cn) }.to_vec();
        wr64(&mut desc, 0x20, cached.as_ptr() as u64);
    } else {
        wr64(&mut desc, 0x20, 0);
        wr64(&mut desc, 0x28, 0);
    }
    let ext = if pext.is_null() || num_ext == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(pext, num_ext as usize * crate::pso_ext::COMPUTE_EXT_STRIDE) }.to_vec() };
    unsafe { com_addref(root as *mut c_void) };
    Some(Owned { desc, _blobs: vec![blob], _layout: Vec::new(), _names: Vec::new(), _cached: cached, ext, root })
}

unsafe fn launch(this: *mut DeviceWrapper, owned: Owned, num_ext: u32, riid: *const Guid, ppv: *mut *mut c_void, sync: CreateFn, kind: Kind, bad_key: Option<u128>, placeholder: &mut dyn FnMut() -> Result<*mut c_void, i32>) -> Option<i32> {
    let marks = if !crate::dred_marks::enabled() {
        crate::dred_marks::Hashes::default()
    } else if kind == Kind::Graphics {
        unsafe { crate::dred_marks::gfx_hashes(owned.desc.as_ptr()) }
    } else {
        unsafe { crate::dred_marks::compute_hashes(owned.desc.as_ptr()) }
    };
    let slot = Arc::new(Slot { st: Mutex::new(Done { started: Some(std::time::Instant::now()), ..Default::default() }), cv: Condvar::new(), real: AtomicUsize::new(0), kind, retry: Mutex::new(None), aliased: AtomicBool::new(false), job: Mutex::new(None), refused: AtomicBool::new(false), bad_key });
    let riid_v: Guid = unsafe { core::ptr::read(riid) };
    let (dev, ws) = (this as usize, slot.clone());
    let job: Job = Box::new(move || {
        let owned = owned;
        let mut out: *mut c_void = core::ptr::null_mut();
        let ext = if owned.ext.is_empty() { core::ptr::null() } else { owned.ext.as_ptr() };
        let hr = unsafe { sync(dev as *mut DeviceWrapper, owned.desc.as_ptr(), num_ext, ext, &riid_v, &mut out) };
        let real = if hr >= 0 { out as usize } else { 0 };
        if real == 0 {
            *ws.retry.lock().unwrap_or_else(|e| e.into_inner()) = Some(Retry { owned, sync, dev, num_ext, riid: riid_v });
        } else {
            drop(owned);
        }
        let mut g = ws.st.lock().unwrap_or_else(|e| e.into_inner());
        g.finished = true;
        g.hr = hr;
        g.real = real;
        if g.detached {
            let ms = g.started.map(|t| t.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
            if g.orphan {
                if real != 0 {
                    unsafe { com_release(real as *mut c_void) };
                }
                g.real = 0;
            } else {
                ws.real.store(real, Ordering::Release);
            }
            if hr < 0 {
                note_bad(ws.bad_key);
                let n = LATE_FAIL.fetch_add(1, Ordering::Relaxed);
                if n < 40 {
                    record("d3d12_xs", "pso_async", &format!("background {:?} PSO creation FAILED hr={hr:#010x} after {ms:.0} ms after S_OK went to the title; its first bind retries the creation synchronously on the title thread once, and identical descs are created synchronously from now on (failure #{})", ws.kind, n + 1));
                }
            } else {
                static R: AtomicUsize = AtomicUsize::new(0);
                if R.fetch_add(1, Ordering::Relaxed) < 40 {
                    record("d3d12_xs", "pso_async", &format!("background {:?} PSO ready after {ms:.0} ms; binds while pending so far {}, swapped binds {}", ws.kind, PENDING_BINDS.load(Ordering::Relaxed), SWAPPED_BINDS.load(Ordering::Relaxed)));
                }
            }
        }
        ws.cv.notify_all();
    });
    *slot.job.lock().unwrap_or_else(|e| e.into_inner()) = Some(job);
    let queued = slot.clone();
    let run: Job = Box::new(move || {
        run_queued(&queued);
    });
    if pool().lock().unwrap_or_else(|e| e.into_inner()).send(run).is_err() {
        drop(slot.job.lock().unwrap_or_else(|e| e.into_inner()).take());
        return None;
    }
    let deadline = std::time::Instant::now() + budget();
    let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
    while !g.finished {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        g = slot.cv.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
    }
    if g.finished {
        INLINE_N.fetch_add(1, Ordering::Relaxed);
        unsafe { *ppv = g.real as *mut c_void };
        return Some(g.hr);
    }
    drop(g);
    let ph = match placeholder() {
        Ok(p) => p,
        Err(hr) => {
            static PF: AtomicUsize = AtomicUsize::new(0);
            if PF.fetch_add(1, Ordering::Relaxed) < 8 {
                record("d3d12_xs", "pso_async", &format!("placeholder {kind:?} PSO creation failed hr={hr:#010x}; waiting for the real PSO on the title thread"));
            }
            let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
            while !g.finished {
                g = slot.cv.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            unsafe { *ppv = g.real as *mut c_void };
            return Some(g.hr);
        }
    };
    let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
    if g.finished {
        let (hr, real) = (g.hr, g.real);
        drop(g);
        unsafe { com_release(ph) };
        INLINE_N.fetch_add(1, Ordering::Relaxed);
        unsafe { *ppv = real as *mut c_void };
        return Some(hr);
    }
    g.detached = true;
    drop(g);
    crate::dred_marks::note(ph, marks, true);
    crate::frame_ev::lock(map()).insert(ph as usize, slot.clone());
    LIVE.fetch_add(1, Ordering::Relaxed);
    if !unsafe { watch(ph) } {
        static WF: AtomicUsize = AtomicUsize::new(0);
        if WF.fetch_add(1, Ordering::Relaxed) < 4 {
            record("d3d12_xs", "pso_async", "SetPrivateDataInterface on a placeholder failed; its real PSO stays alive for the process lifetime");
        }
    }
    let n = ASYNC_N.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 40 || n % 100 == 0 {
        record("d3d12_xs", "pso_async", &format!("{kind:?} PSO #{n} not ready after {:.0} ms: handle {ph:p} returned (never bound itself: every bind waits for and binds the real PSO), real PSO keeps building in the background (inline {})", budget().as_secs_f64() * 1000.0, INLINE_N.load(Ordering::Relaxed)));
    }
    crate::frame_ev::mark_async();
    unsafe { *ppv = ph };
    Some(0)
}

fn run_queued(slot: &Slot) -> bool {
    let job = slot.job.lock().unwrap_or_else(|e| e.into_inner()).take();
    match job {
        Some(j) => {
            j();
            true
        }
        None => false,
    }
}

fn take_if_queued(slot: &Slot) {
    if run_queued(slot) {
        let n = STOLEN.fetch_add(1, Ordering::Relaxed);
        if n < 20 {
            record("d3d12_xs", "pso_async", &format!("a bind found its {:?} PSO still queued behind other builds and built it on the binding thread instead of waiting (#{})", slot.kind, n + 1));
        }
    }
}

static STOLEN: AtomicUsize = AtomicUsize::new(0);

pub fn swap(p: *mut c_void) -> *mut c_void {
    if p.is_null() || LIVE.load(Ordering::Relaxed) == 0 {
        return p;
    }
    let slot = crate::frame_ev::lock(map()).get(&(p as usize)).cloned();
    let Some(slot) = slot else { return p };
    let r = slot.real.load(Ordering::Acquire);
    if r != 0 {
        return bound(&slot, p, r);
    }
    let _ev = crate::frame_ev::begin(crate::frame_ev::PSO_BIND);
    let t0 = std::time::Instant::now();
    take_if_queued(&slot);
    let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
    while !g.finished {
        g = slot.cv.wait(g).unwrap_or_else(|e| e.into_inner());
    }
    let real = g.real;
    drop(g);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    COMPUTE_WAIT_US.fetch_add((ms * 1000.0) as u64, Ordering::Relaxed);
    if ms > 5.0 {
        static W: AtomicUsize = AtomicUsize::new(0);
        if W.fetch_add(1, Ordering::Relaxed) < 20 {
            record("d3d12_xs", "pso_async", &format!("first bind of a {:?} PSO waited {ms:.0} ms for its background build (draws and dispatches are never skipped)", slot.kind));
        }
    }
    if real != 0 {
        return bound(&slot, p, real);
    }
    match retry_on_title_thread(&slot) {
        Ok(real) => bound(&slot, p, real),
        Err(hr) => refuse(&slot, p, hr),
    }
}

fn refuse(slot: &Slot, p: *mut c_void, hr: i32) -> *mut c_void {
    note_bad(slot.bad_key);
    let binds = REFUSED_BINDS.fetch_add(1, Ordering::Relaxed) + 1;
    if !slot.refused.swap(true, Ordering::AcqRel) {
        let n = FINAL_FAIL.fetch_add(1, Ordering::Relaxed);
        record("d3d12_xs", "pso_async", &format!("ERROR: REFUSED {:?} PSO handle {p:p}: the background build and the synchronous title-thread retry both failed (hr={hr:#010x}) after the creation had returned S_OK; every bind of this handle binds the placeholder, whose work draws and dispatches nothing (unrecoverable PSO #{}, refused binds so far {binds}); identical descs now create synchronously and return the real HRESULT; see BLOCKERS.md 'async PSO failure after S_OK'", slot.kind, n + 1));
        crate::dred::on_failure(hr, "deferred PSO creation");
    }
    p
}

fn bound(slot: &Slot, handle: *mut c_void, real: usize) -> *mut c_void {
    SWAPPED_BINDS.fetch_add(1, Ordering::Relaxed);
    if !slot.aliased.swap(true, Ordering::AcqRel) {
        crate::cbv_copy::alias_pso(handle as usize, real);
    }
    real as *mut c_void
}

fn retry_on_title_thread(slot: &Slot) -> Result<usize, i32> {
    let mut rg = slot.retry.lock().unwrap_or_else(|e| e.into_inner());
    let r = slot.real.load(Ordering::Acquire);
    if r != 0 {
        return Ok(r);
    }
    {
        let g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
        if g.real != 0 {
            return Ok(g.real);
        }
    }
    let Some(rt) = rg.take() else {
        let hr = slot.st.lock().unwrap_or_else(|e| e.into_inner()).hr;
        return Err(if hr < 0 { hr } else { 0x8000_4005u32 as i32 });
    };
    let mut out: *mut c_void = core::ptr::null_mut();
    let ext = if rt.owned.ext.is_empty() { core::ptr::null() } else { rt.owned.ext.as_ptr() };
    let hr = unsafe { (rt.sync)(rt.dev as *mut DeviceWrapper, rt.owned.desc.as_ptr(), rt.num_ext, ext, &rt.riid, &mut out) };
    if hr < 0 || out.is_null() {
        let hr = if hr < 0 { hr } else { 0x8000_4005u32 as i32 };
        slot.st.lock().unwrap_or_else(|e| e.into_inner()).hr = hr;
        record("d3d12_xs", "pso_async", &format!("{:?} PSO: synchronous title-thread retry at its first bind failed hr={hr:#010x}", slot.kind));
        return Err(hr);
    }
    let real = out as usize;
    let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
    g.hr = hr;
    if g.orphan {
        drop(g);
        unsafe { com_release(out) };
        return Err(0x8000_4005u32 as i32);
    }
    g.real = real;
    slot.real.store(real, Ordering::Release);
    drop(g);
    let n = RETRY_OK.fetch_add(1, Ordering::Relaxed);
    if n < 40 {
        record("d3d12_xs", "pso_async", &format!("{:?} PSO background build had failed; the title-thread retry at its first bind succeeded, so the draw uses the real PSO (retry #{})", slot.kind, n + 1));
    }
    Ok(real)
}

pub fn wait_real(p: *mut c_void) -> *mut c_void {
    if p.is_null() || LIVE.load(Ordering::Relaxed) == 0 {
        return p;
    }
    let slot = crate::frame_ev::lock(map()).get(&(p as usize)).cloned();
    let Some(slot) = slot else { return p };
    let _ev = crate::frame_ev::begin(crate::frame_ev::PSO_BIND);
    let t0 = std::time::Instant::now();
    take_if_queued(&slot);
    let mut g = slot.st.lock().unwrap_or_else(|e| e.into_inner());
    while !g.finished {
        g = slot.cv.wait(g).unwrap_or_else(|e| e.into_inner());
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if ms > 5.0 {
        static W: AtomicUsize = AtomicUsize::new(0);
        if W.fetch_add(1, Ordering::Relaxed) < 20 {
            record("d3d12_xs", "pso_async", &format!("derived PSO waited {ms:.0} ms for its base PSO"));
        }
    }
    let real = g.real;
    drop(g);
    if real != 0 {
        return real as *mut c_void;
    }
    match retry_on_title_thread(&slot) {
        Ok(r) => r as *mut c_void,
        Err(hr) => refuse(&slot, p, hr),
    }
}

pub fn summary() -> String {
    format!("async {} inline {} live {} pending binds {} swapped binds {} late failures {} (title-thread retries ok {}, unrecoverable {}, refused binds {}, sync after a failed desc {}) first-bind wait {:.0} ms, builds taken over by their bind {}", ASYNC_N.load(Ordering::Relaxed), INLINE_N.load(Ordering::Relaxed), LIVE.load(Ordering::Relaxed), PENDING_BINDS.load(Ordering::Relaxed), SWAPPED_BINDS.load(Ordering::Relaxed), LATE_FAIL.load(Ordering::Relaxed), RETRY_OK.load(Ordering::Relaxed), FINAL_FAIL.load(Ordering::Relaxed), REFUSED_BINDS.load(Ordering::Relaxed), SYNC_AFTER_FAIL.load(Ordering::Relaxed), COMPUTE_WAIT_US.load(Ordering::Relaxed) as f64 / 1000.0, STOLEN.load(Ordering::Relaxed))
}

unsafe extern "system" fn set_pso(list: *mut c_void, pso: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(PREV_SET.load(Ordering::Relaxed)) };
    unsafe { f(list, swap(pso)) }
}

unsafe extern "system" fn reset(list: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(PREV_RESET.load(Ordering::Relaxed)) };
    unsafe { f(list, alloc, swap(pso)) }
}

unsafe extern "system" fn clear_state(list: *mut c_void, pso: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(PREV_CLEAR.load(Ordering::Relaxed)) };
    unsafe { f(list, swap(pso)) }
}

pub fn install(vt: &[AtomicUsize]) {
    if !enabled() {
        return;
    }
    let (s, r, c) = (vt[XBOX_SLOT_SET_PSO].load(Ordering::Relaxed), vt[XBOX_SLOT_RESET].load(Ordering::Relaxed), vt[XBOX_SLOT_CLEAR_STATE].load(Ordering::Relaxed));
    if s == 0 || r == 0 || c == 0 {
        record("d3d12_xs", "pso_async", "command-list slots 10/11/25 missing; PSOs stay synchronous");
        return;
    }
    PREV_SET.store(s, Ordering::Relaxed);
    PREV_RESET.store(r, Ordering::Relaxed);
    PREV_CLEAR.store(c, Ordering::Relaxed);
    vt[XBOX_SLOT_SET_PSO].store(set_pso as usize, Ordering::Relaxed);
    vt[XBOX_SLOT_RESET].store(reset as usize, Ordering::Relaxed);
    vt[XBOX_SLOT_CLEAR_STATE].store(clear_state as usize, Ordering::Relaxed);
    HOOKED.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc() -> Vec<u8> {
        let mut d = vec![0u8; DESC_SIZE];
        wr64(&mut d, 0, 0x1000);
        wr64(&mut d, 0x08, 0x2000);
        wr64(&mut d, 0x10, 64);
        wr32(&mut d, 0x23c, 3);
        d
    }

    #[test]
    fn only_plain_triangle_pipelines_go_async() {
        assert!(eligible(&desc()));
        let mut d = desc();
        wr64(&mut d, 0x28, 0x3000);
        wr64(&mut d, 0x30, 64);
        assert!(!eligible(&d));
        let mut d = desc();
        wr32(&mut d, 0x23c, 4);
        assert!(!eligible(&d));
        let mut d = desc();
        wr64(&mut d, 0x58, 0x4000);
        wr32(&mut d, 0x60, 2);
        assert!(!eligible(&d));
        let mut d = desc();
        wr64(&mut d, 0, 0);
        assert!(!eligible(&d));
    }

    fn slot_with_job(ran: Arc<AtomicUsize>) -> Arc<Slot> {
        let slot = Arc::new(Slot { st: Mutex::new(Done::default()), cv: Condvar::new(), real: AtomicUsize::new(0), kind: Kind::Graphics, retry: Mutex::new(None), aliased: AtomicBool::new(false), job: Mutex::new(None), refused: AtomicBool::new(false), bad_key: None });
        let ws = slot.clone();
        *slot.job.lock().unwrap() = Some(Box::new(move || {
            ran.fetch_add(1, Ordering::SeqCst);
            let mut g = ws.st.lock().unwrap();
            g.finished = true;
            g.real = 0x1234;
            ws.cv.notify_all();
        }));
        slot
    }

    #[test]
    fn a_queued_build_runs_exactly_once_on_whichever_thread_reaches_it_first() {
        let ran = Arc::new(AtomicUsize::new(0));
        let slot = slot_with_job(ran.clone());
        take_if_queued(&slot);
        assert!(slot.st.lock().unwrap().finished);
        assert_eq!(slot.st.lock().unwrap().real, 0x1234);
        assert!(!run_queued(&slot));
        assert_eq!(ran.load(Ordering::SeqCst), 1);
        let ran = Arc::new(AtomicUsize::new(0));
        let slot = slot_with_job(ran.clone());
        let threads: Vec<_> = (0..8).map(|_| {
            let s = slot.clone();
            std::thread::spawn(move || run_queued(&s))
        }).collect();
        let took = threads.into_iter().map(|t| t.join().unwrap()).filter(|&b| b).count();
        assert_eq!(took, 1);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_desc_that_failed_after_s_ok_is_created_synchronously_next_time() {
        let k = Some(0x5eed_0000_0000_0000_0000_0000_0000_0001u128);
        assert!(!known_bad(k, Kind::Graphics));
        note_bad(k);
        assert!(known_bad(k, Kind::Graphics));
        assert!(!known_bad(None, Kind::Graphics));
        assert!(!known_bad(Some(0x5eed_0000_0000_0000_0000_0000_0000_0002), Kind::Compute));
    }

    #[test]
    fn unknown_pointers_pass_through_unchanged() {
        let p = 0x1234usize as *mut c_void;
        assert_eq!(swap(p), p);
        assert_eq!(wait_real(p), p);
        assert!(swap(core::ptr::null_mut()).is_null());
    }

    #[repr(C)]
    struct FakeVt {
        qi: unsafe extern "system" fn(*mut Fake, *const Guid, *mut *mut c_void) -> i32,
        add: unsafe extern "system" fn(*mut Fake) -> u32,
        rel: unsafe extern "system" fn(*mut Fake) -> u32,
        s3: usize,
        s4: usize,
        spdi: unsafe extern "system" fn(*mut Fake, *const Guid, *mut c_void) -> i32,
    }

    #[repr(C)]
    struct Fake {
        vt: &'static FakeVt,
        refs: AtomicU32,
    }

    unsafe extern "system" fn f_qi(_: *mut Fake, _: *const Guid, ppv: *mut *mut c_void) -> i32 {
        if !ppv.is_null() {
            unsafe { *ppv = core::ptr::null_mut() };
        }
        0x8000_4002u32 as i32
    }

    unsafe extern "system" fn f_add(t: *mut Fake) -> u32 {
        unsafe { (*t).refs.fetch_add(1, Ordering::AcqRel) + 1 }
    }

    unsafe extern "system" fn f_rel(t: *mut Fake) -> u32 {
        unsafe { (*t).refs.fetch_sub(1, Ordering::AcqRel) - 1 }
    }

    unsafe extern "system" fn f_spdi(_: *mut Fake, _: *const Guid, iface: *mut c_void) -> i32 {
        if !iface.is_null() {
            unsafe { com_addref(iface) };
        }
        0
    }

    static FAKE_VT: FakeVt = FakeVt { qi: f_qi, add: f_add, rel: f_rel, s3: 0, s4: 0, spdi: f_spdi };

    fn fake() -> *mut c_void {
        Box::into_raw(Box::new(Fake { vt: &FAKE_VT, refs: AtomicU32::new(1) })) as *mut c_void
    }

    fn refs(p: *mut c_void) -> u32 {
        unsafe { (*(p as *mut Fake)).refs.load(Ordering::Acquire) }
    }

    const CTRL_AT: usize = 0x100;

    struct Ctrl {
        gate: Mutex<bool>,
        cv: Condvar,
        calls: AtomicU32,
        results: Vec<(i32, usize)>,
        threads: Mutex<Vec<std::thread::ThreadId>>,
    }

    impl Ctrl {
        fn new(results: Vec<(i32, usize)>) -> &'static Ctrl {
            Box::leak(Box::new(Ctrl { gate: Mutex::new(false), cv: Condvar::new(), calls: AtomicU32::new(0), results, threads: Mutex::new(Vec::new()) }))
        }

        fn open(&self) {
            *self.gate.lock().unwrap() = true;
            self.cv.notify_all();
        }
    }

    unsafe extern "system" fn gated_create(_: *mut DeviceWrapper, desc: *const u8, _: u32, _: *const u8, _: *const Guid, ppv: *mut *mut c_void) -> i32 {
        let ctrl = unsafe { &*(core::ptr::read_unaligned(desc.add(CTRL_AT) as *const usize) as *const Ctrl) };
        let n = ctrl.calls.fetch_add(1, Ordering::AcqRel) as usize;
        ctrl.threads.lock().unwrap().push(std::thread::current().id());
        if n == 0 {
            let mut g = ctrl.gate.lock().unwrap();
            while !*g {
                g = ctrl.cv.wait(g).unwrap();
            }
        }
        let (hr, real) = ctrl.results[n.min(ctrl.results.len() - 1)];
        unsafe { *ppv = real as *mut c_void };
        hr
    }

    fn launch_gated(ctrl: &'static Ctrl) -> (*mut c_void, *mut c_void, i32) {
        let root = fake();
        let mut desc = vec![0u8; DESC_SIZE];
        wr64(&mut desc, 0, root as u64);
        wr64(&mut desc, CTRL_AT, ctrl as *const Ctrl as u64);
        let owned = Owned { desc, _blobs: Vec::new(), _layout: Vec::new(), _names: Vec::new(), _cached: Vec::new(), ext: Vec::new(), root: root as usize };
        unsafe { com_addref(root) };
        let handle = fake();
        let mut out: *mut c_void = core::ptr::null_mut();
        let key = Some(ctrl as *const Ctrl as u128 | 0xc7_0000_0000_0000_0000_0000_0000_0000);
        let hr = unsafe { launch(core::ptr::null_mut(), owned, 0, &IID_PSO, &mut out, gated_create, Kind::Graphics, key, &mut || Ok(handle)) }.unwrap();
        (out, handle, hr)
    }

    fn ctrl_key(ctrl: &'static Ctrl) -> Option<u128> {
        Some(ctrl as *const Ctrl as u128 | 0xc7_0000_0000_0000_0000_0000_0000_0000)
    }

    fn open_later(ctrl: &'static Ctrl, ms: u64) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            ctrl.open();
        })
    }

    #[test]
    fn bind_of_a_not_ready_pso_waits_and_binds_the_real_one() {
        let real = fake();
        let ctrl = Ctrl::new(vec![(0, real as usize)]);
        let (out, handle, hr) = launch_gated(ctrl);
        assert_eq!(hr, 0);
        assert_eq!(out, handle, "budget passed, the title holds the handle");
        let opener = open_later(ctrl, 80);
        let t0 = std::time::Instant::now();
        let bound = swap(out);
        let waited = t0.elapsed();
        opener.join().unwrap();
        assert_eq!(bound, real, "the draw must use the real PSO, never the handle");
        assert!(waited >= std::time::Duration::from_millis(40), "bind returned before the real PSO existed ({waited:?})");
        assert_eq!(swap(out), real);
        assert_eq!(wait_real(out), real);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 1);
        assert_eq!(real_of(out as usize), real as usize);
        unsafe { com_release(handle) };
    }

    #[test]
    fn failed_background_build_is_retried_on_the_binding_thread() {
        let real = fake();
        let ctrl = Ctrl::new(vec![(0x8000_4005u32 as i32, 0), (0, real as usize)]);
        let (out, handle, _) = launch_gated(ctrl);
        assert_eq!(out, handle);
        ctrl.open();
        let bound = swap(out);
        assert_eq!(bound, real, "a retry that succeeds must be the PSO that gets bound");
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2);
        let th = ctrl.threads.lock().unwrap().clone();
        assert_eq!(th[1], std::thread::current().id(), "the retry runs on the binding thread");
        assert_eq!(swap(out), real);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2, "a resolved slot is never rebuilt");
        unsafe { com_release(handle) };
    }

    #[test]
    fn unrecoverable_pso_is_reported_not_silently_bound() {
        let ctrl = Ctrl::new(vec![(0x8000_4005u32 as i32, 0), (0x8007_000Eu32 as i32, 0)]);
        let (out, handle, _) = launch_gated(ctrl);
        assert_eq!(out, handle);
        ctrl.open();
        let before = FINAL_FAIL.load(Ordering::Acquire);
        let bound = swap(out);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2, "the failure was retried once");
        assert!(FINAL_FAIL.load(Ordering::Acquire) > before, "an unrecoverable bind is counted and reported");
        assert_eq!(bound, handle);
        let _ = swap(out);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2);
        assert!(summary().contains("unrecoverable"));
        unsafe { com_release(handle) };
    }

    #[test]
    fn invalid_arg_after_s_ok_is_retried_once_then_refused_visibly() {
        let e_invalidarg = 0x8007_0057u32 as i32;
        let ctrl = Ctrl::new(vec![(e_invalidarg, 0), (e_invalidarg, 0)]);
        let (out, handle, hr) = launch_gated(ctrl);
        assert_eq!(hr, 0);
        assert_eq!(out, handle);
        ctrl.open();
        let refused_before = REFUSED_BINDS.load(Ordering::Acquire);
        let bound = swap(out);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2, "E_INVALIDARG after S_OK gets one synchronous retry at the first bind");
        let th = ctrl.threads.lock().unwrap().clone();
        assert_eq!(th[1], std::thread::current().id(), "the retry runs on the binding thread");
        assert_eq!(bound, handle);
        let slot = crate::frame_ev::lock(map()).get(&(out as usize)).cloned().unwrap();
        assert!(slot.refused.load(Ordering::Acquire), "the handle is marked refused");
        let _ = swap(out);
        let _ = wait_real(out);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2, "a refused handle is never rebuilt per bind");
        assert!(REFUSED_BINDS.load(Ordering::Acquire) >= refused_before + 3);
        assert!(bad_descs().lock().unwrap().contains(&ctrl_key(ctrl).unwrap()), "the desc is remembered so the next identical creation is synchronous");
        assert!(summary().contains("refused binds"));
        unsafe { com_release(handle) };
    }

    #[test]
    fn invalid_arg_after_s_ok_that_succeeds_on_retry_binds_the_real_pso() {
        let real = fake();
        let ctrl = Ctrl::new(vec![(0x8007_0057u32 as i32, 0), (0, real as usize)]);
        let (out, handle, _) = launch_gated(ctrl);
        ctrl.open();
        assert_eq!(swap(out), real);
        assert_eq!(ctrl.calls.load(Ordering::Acquire), 2);
        unsafe { com_release(handle) };
    }

    #[test]
    fn destroying_the_handle_releases_the_real_pso() {
        let real = fake();
        let ctrl = Ctrl::new(vec![(0, real as usize)]);
        let (out, handle, _) = launch_gated(ctrl);
        ctrl.open();
        assert_eq!(swap(out), real);
        assert_eq!(refs(real), 1);
        placeholder_destroyed(handle as usize);
        assert_eq!(refs(real), 0);
        assert_eq!(swap(out), out);
    }

    #[test]
    fn placeholder_vertex_shader_compiles() {
        if tombox_xshader::dxc::available() {
            assert!(null_vs().is_some_and(|v| v.len() > 64));
        }
    }

    #[test]
    fn placeholder_compute_shader_compiles() {
        if tombox_xshader::dxc::available() {
            assert!(null_cs().is_some_and(|v| v.len() > 64));
        }
    }
}
