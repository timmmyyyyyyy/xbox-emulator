use compat_common::record;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

#[repr(C)]
pub struct Guid(u32, u16, u16, [u8; 8]);
pub const IID_DEVICE: Guid = Guid(0x189819f1, 0x1db6, 0x4b57, [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7]);
pub const IID_DEVICE3: Guid = Guid(0x81dadc15, 0x2bad, 0x4392, [0x93, 0xc5, 0x10, 0x13, 0x45, 0xc4, 0xaa, 0x98]);
pub const IID_HEAP: Guid = Guid(0x6b3b2502, 0x6e51, 0x45b3, [0x90, 0xee, 0x98, 0x84, 0x26, 0x5e, 0x8d, 0xf3]);
pub const IID_RESOURCE: Guid = Guid(0x696442be, 0xa72e, 0x4059, [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad]);
const IID_UNKNOWN: Guid = Guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const WATCH_TAG: Guid = Guid(0x7a11_a5ed, 0x51ab, 0x4c3e, [0x9d, 0x27, 0x0b, 0x6e, 0x31, 0x84, 0xc2, 0x5f]);

pub const PLACEMENT_ALIGN: u64 = 0x1_0000;
const ALLOW_CROSS_ADAPTER: u32 = 0x10;
const MEM_COMMIT: u32 = 0x1000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_PRIVATE: u32 = 0x2_0000;
const NOTE_COMMITTED: u32 = 1;

extern "system" {
    fn VirtualQuery(addr: *const c_void, info: *mut Mbi, len: usize) -> usize;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Mbi {
    base: usize,
    alloc_base: usize,
    alloc_protect: u32,
    partition: u16,
    pad0: u16,
    size: usize,
    state: u32,
    protect: u32,
    kind: u32,
    pad1: u32,
}

unsafe fn vt(obj: *mut c_void) -> *const usize {
    unsafe { *(obj as *const *const usize) }
}

macro_rules! vcall {
    ($obj:expr, $slot:expr, fn($($at:ty),*) -> $rt:ty $(, $arg:expr)*) => {{
        let f: unsafe extern "system" fn(*mut c_void $(, $at)*) -> $rt = core::mem::transmute(*vt($obj).add($slot));
        f($obj $(, $arg)*)
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Title,
    Local,
    Shadow,
}

#[derive(Debug)]
pub struct Span {
    pub lo: u64,
    pub hi: u64,
    pub kind: Kind,
    pub heap: usize,
    pub retired: bool,
    pub live: HashSet<usize>,
}

#[derive(Default)]
pub struct Spans {
    by_lo: BTreeMap<u64, Vec<Span>>,
}

impl Spans {
    pub fn find(&self, kind: Kind, va: u64, len: u64, align: u64) -> Option<(u64, usize)> {
        let end = va.checked_add(len.max(1))?;
        for (lo, v) in self.by_lo.range(..=va).rev().take(64) {
            for (i, s) in v.iter().enumerate() {
                if s.kind == kind && !s.retired && end <= s.hi && (va - lo) % align.max(1) == 0 {
                    return Some((*lo, i));
                }
            }
        }
        None
    }

    pub fn get(&self, key: (u64, usize)) -> &Span {
        &self.by_lo[&key.0][key.1]
    }

    pub fn get_mut(&mut self, key: (u64, usize)) -> &mut Span {
        self.by_lo.get_mut(&key.0).unwrap().get_mut(key.1).unwrap()
    }

    pub fn insert(&mut self, s: Span) -> (u64, usize) {
        let lo = s.lo;
        let v = self.by_lo.entry(lo).or_default();
        v.push(s);
        (lo, v.len() - 1)
    }

    pub fn retire_overlapping(&mut self, kind: Kind, lo: u64, hi: u64) -> usize {
        let mut n = 0;
        for (_, v) in self.by_lo.range_mut(..hi) {
            for s in v.iter_mut() {
                if s.kind == kind && !s.retired && s.hi > lo {
                    s.retired = true;
                    n += 1;
                }
            }
        }
        n
    }

    pub fn resource_gone(&mut self, res: usize) {
        for v in self.by_lo.values_mut() {
            for s in v.iter_mut() {
                s.live.remove(&res);
            }
        }
    }

    pub fn take_idle(&mut self) -> Vec<usize> {
        let mut out = Vec::new();
        self.by_lo.retain(|_, v| {
            v.retain(|s| {
                let idle = s.live.is_empty();
                if idle {
                    out.push(s.heap);
                }
                !idle
            });
            !v.is_empty()
        });
        out
    }

    pub fn len(&self) -> usize {
        self.by_lo.values().map(|v| v.len()).sum()
    }
}

static SPANS: Mutex<Option<Spans>> = Mutex::new(None);
static NOTES: Mutex<Vec<(u64, u64)>> = Mutex::new(Vec::new());
static GONE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static ALIASED: Mutex<Option<HashSet<usize>>> = Mutex::new(None);
static DEVICE3: AtomicUsize = AtomicUsize::new(0);
static DEVICE3_OF: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Stats {
    title: AtomicU32,
    local: AtomicU32,
    shared: AtomicU32,
    fallback: AtomicU32,
    heaps: AtomicU32,
}
static STATS: Stats = Stats { title: AtomicU32::new(0), local: AtomicU32::new(0), shared: AtomicU32::new(0), fallback: AtomicU32::new(0), heaps: AtomicU32::new(0) };

fn env_on(name: &str) -> bool {
    std::env::var(name).map(|v| v != "0").unwrap_or(true)
}

thread_local! {
    static FORCE_TITLE: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

fn title_buffers_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    FORCE_TITLE.with(|f| f.get()) || *V.get_or_init(|| std::env::var("TOMBOX_ALIAS_TITLE").map(|v| v == "1").unwrap_or(false))
}

fn local_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| env_on("TOMBOX_ALIAS_LOCAL"))
}

pub fn note_title_memory(addr: usize, len: usize, kind: u32) {
    if kind == NOTE_COMMITTED && len != 0 {
        NOTES.lock().unwrap_or_else(|e| e.into_inner()).push((addr as u64, addr as u64 + len as u64));
    }
}

pub fn is_aliased(res: *mut c_void) -> bool {
    !res.is_null() && ALIASED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.contains(&(res as usize)))
}

#[repr(C)]
struct Watch {
    vtbl: *const usize,
    refs: AtomicU32,
    res: usize,
}

unsafe extern "system" fn watch_qi(this: *mut c_void, riid: *const Guid, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    let g = unsafe { &*riid };
    if g.0 == IID_UNKNOWN.0 && g.1 == IID_UNKNOWN.1 && g.2 == IID_UNKNOWN.2 && g.3 == IID_UNKNOWN.3 {
        unsafe { watch_addref(this) };
        unsafe { *out = this };
        return 0;
    }
    unsafe { *out = core::ptr::null_mut() };
    0x8000_4002u32 as i32
}

unsafe extern "system" fn watch_addref(this: *mut c_void) -> u32 {
    unsafe { (*(this as *mut Watch)).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn watch_release(this: *mut c_void) -> u32 {
    let w = this as *mut Watch;
    let n = unsafe { (*w).refs.fetch_sub(1, Ordering::AcqRel) } - 1;
    if n == 0 {
        let res = unsafe { (*w).res };
        crate::placed_offset::forget(res);
        crate::title_shadow::resource_gone(res);
        GONE.lock().unwrap_or_else(|e| e.into_inner()).push(res);
        if let Some(s) = ALIASED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            s.remove(&res);
        }
        drop(unsafe { Box::from_raw(w) });
        tick();
    }
    n
}

#[repr(C)]
struct WatchVtbl {
    qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    addref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

static WATCH_VTBL: WatchVtbl = WatchVtbl { qi: watch_qi, addref: watch_addref, release: watch_release };

unsafe fn watch(res: *mut c_void) -> bool {
    let w = Box::into_raw(Box::new(Watch { vtbl: &WATCH_VTBL as *const WatchVtbl as *const usize, refs: AtomicU32::new(1), res: res as usize }));
    let hr = unsafe { vcall!(res, 5, fn(*const Guid, *mut c_void) -> i32, &WATCH_TAG, w as *mut c_void) };
    unsafe { watch_release(w as *mut c_void) };
    hr >= 0
}

unsafe fn device3(real: *mut c_void) -> *mut c_void {
    if DEVICE3_OF.load(Ordering::Acquire) == real as usize {
        return DEVICE3.load(Ordering::Acquire) as *mut c_void;
    }
    let mut d3: *mut c_void = core::ptr::null_mut();
    if unsafe { vcall!(real, 0, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE3, &mut d3) } < 0 {
        d3 = core::ptr::null_mut();
    } else {
        unsafe { vcall!(d3, 2, fn() -> u32) };
    }
    DEVICE3.store(d3 as usize, Ordering::Release);
    DEVICE3_OF.store(real as usize, Ordering::Release);
    d3
}

fn query(a: u64) -> Option<Mbi> {
    let mut m = Mbi::default();
    if unsafe { VirtualQuery(a as *const c_void, &mut m, core::mem::size_of::<Mbi>()) } == 0 {
        None
    } else {
        Some(m)
    }
}

pub fn committed_writable(m: &Mbi) -> bool {
    m.state == MEM_COMMIT && m.protect & 0xcc != 0 && m.protect & 0x100 == 0
}

fn title_allocation(va: u64, len: u64) -> Option<(u64, u64)> {
    let first = query(va)?;
    let base = first.alloc_base as u64;
    if base == 0 || base % PLACEMENT_ALIGN != 0 || first.kind != MEM_PRIVATE {
        return None;
    }
    let mut p = base;
    let mut end = base;
    for _ in 0..4096 {
        let m = match query(p) {
            Some(m) if m.alloc_base as u64 == base && m.state != MEM_FREE => m,
            _ => break,
        };
        if !committed_writable(&m) {
            return None;
        }
        end = m.base as u64 + m.size as u64;
        p = end;
    }
    if va + len > end { None } else { Some((base, end)) }
}

const LOCAL_SPAN_CAP: u64 = 64 << 20;
const LOCAL_WINDOW: u64 = 4 << 20;
const TITLE_HEAP_CAP: u64 = 1 << 30;

pub fn local_span(va: u64, need: u64, reservation: Option<(u64, u64)>) -> (u64, u64) {
    let need = need.max(1);
    let lo = va & !(PLACEMENT_ALIGN - 1);
    let hi_min = round_up(va + need, PLACEMENT_ALIGN);
    let Some((base, end)) = reservation.filter(|(b, e)| *b <= va && hi_min <= *e && b % PLACEMENT_ALIGN == 0) else {
        return (lo, hi_min);
    };
    if end - base <= LOCAL_SPAN_CAP {
        return (base, end);
    }
    (lo, (lo + need.max(LOCAL_WINDOW)).min(end).max(hi_min))
}

pub fn oversize_span(va: u64, pc_size: u64, xbox_size: u64) -> Option<(u64, u64)> {
    let title = round_up(xbox_size, PLACEMENT_ALIGN);
    (xbox_size != 0 && pc_size > title).then(|| (va, va + title))
}

fn reservation(va: u64) -> Option<(u64, u64)> {
    let first = query(va)?;
    if first.state == MEM_FREE {
        return None;
    }
    let base = first.alloc_base as u64;
    let mut p = base;
    let mut end = base;
    for _ in 0..65536 {
        match query(p) {
            Some(m) if m.alloc_base as u64 == base && m.state != MEM_FREE => {
                end = m.base as u64 + m.size as u64;
                p = end;
            }
            _ => break,
        }
    }
    if end > base { Some((base, end)) } else { None }
}

unsafe fn heap_size(heap: *mut c_void) -> u64 {
    let mut d = [0u64; 6];
    unsafe { vcall!(heap, 8, fn(*mut u64) -> *mut u64, d.as_mut_ptr()) };
    d[0]
}

unsafe fn allocation_info(real: *mut c_void, desc: &[u8; 0x38]) -> (u64, u64) {
    let mut r = [0u64; 2];
    unsafe { vcall!(real, 25, fn(*mut u64, u32, u32, *const u8) -> *mut u64, r.as_mut_ptr(), 0, 1, desc.as_ptr()) };
    (r[0], r[1])
}

fn heap_properties(desc: &[u64; 6]) -> [u32; 5] {
    let b: Vec<u8> = desc.iter().flat_map(|w| w.to_le_bytes()).collect();
    core::array::from_fn(|i| u32::from_le_bytes(b[8 + i * 4..12 + i * 4].try_into().unwrap()))
}

pub(crate) unsafe fn committed_when_too_big(
    real: *mut c_void,
    heap: *mut c_void,
    offset: u64,
    desc: &[u8; 0x38],
    init_state: u32,
    pclear: *const c_void,
    ppv: *mut *mut c_void,
) -> Option<i32> {
    if heap.is_null() || ppv.is_null() || u32::from_le_bytes(desc[0..4].try_into().unwrap()) < 2 {
        return None;
    }
    let (pc_size, _) = unsafe { allocation_info(real, desc) };
    let mut hd = [0u64; 6];
    unsafe { vcall!(heap, 8, fn(*mut u64) -> *mut u64, hd.as_mut_ptr()) };
    let heap_bytes = hd[0];
    if pc_size == u64::MAX || pc_size == 0 || offset.saturating_add(pc_size) <= heap_bytes {
        return None;
    }
    let props = heap_properties(&hd);
    let hr = unsafe {
        vcall!(real, 27, fn(*const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32,
            props.as_ptr(), 0, desc.as_ptr(), init_state, pclear, &IID_RESOURCE, ppv)
    };
    static N: AtomicUsize = AtomicUsize::new(0);
    if hr < 0 || N.fetch_add(1, Ordering::Relaxed) < 8 {
        record("d3d12_xs", "CreatePlacedResource", &format!(
            "texture needs {pc_size:#x} bytes on PC but the title heap has {heap_bytes:#x} at offset {offset:#x} (console size); created committed on heap type {} instead, not aliased with the title heap -> hr={hr:#010x}",
            props[0]));
    }
    Some(hr)
}

unsafe fn open_title_heap(real: *mut c_void, base: u64) -> Option<(*mut c_void, u64)> {
    let d3 = unsafe { device3(real) };
    if d3.is_null() {
        return None;
    }
    if let Some((b, e)) = reservation(base) {
        if b == base && e - b > TITLE_HEAP_CAP {
            static C: AtomicUsize = AtomicUsize::new(0);
            if C.fetch_add(1, Ordering::Relaxed) < 8 {
                record("d3d12_xs", "alias", &format!("title allocation {base:#x}+{:#x} is over the {TITLE_HEAP_CAP:#x} pin cap; placement falls back to a committed resource", e - b));
            }
            return None;
        }
    }
    let mut h: *mut c_void = core::ptr::null_mut();
    let t0 = std::time::Instant::now();
    let hr = unsafe { vcall!(d3, 48, fn(*const c_void, *const Guid, *mut *mut c_void) -> i32, base as *const c_void, &IID_HEAP, &mut h) };
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    if hr >= 0 && !h.is_null() {
        static L: AtomicUsize = AtomicUsize::new(0);
        let size = unsafe { heap_size(h) };
        if L.fetch_add(1, Ordering::Relaxed) < 24 || ms > 5.0 {
            record("d3d12_xs", "alias", &format!("opened title heap over {base:#x}+{size:#x} in {ms:.2} ms (pages pinned until its last placed resource is released)"));
        }
    }
    if hr < 0 || h.is_null() {
        static N: AtomicUsize = AtomicUsize::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 8 {
            record("d3d12_xs", "alias", &format!("OpenExistingHeapFromAddress({base:#x}) hr={hr:#010x}; placement falls back to a committed resource"));
        }
        return None;
    }
    let size = unsafe { heap_size(h) };
    Some((h, size))
}

unsafe fn create_local_heap(real: *mut c_void, size: u64, msaa: bool) -> Option<*mut c_void> {
    let (size, heap_align) = crate::msaa_place::heap_layout(size, msaa);
    let mut d = [0u8; 0x30];
    d[0..8].copy_from_slice(&size.to_le_bytes());
    d[0x20..0x28].copy_from_slice(&heap_align.to_le_bytes());
    d[8..12].copy_from_slice(&1u32.to_le_bytes());
    d[0x14..0x18].copy_from_slice(&1u32.to_le_bytes());
    d[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    let mut h: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { vcall!(real, 28, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &IID_HEAP, &mut h) };
    if hr < 0 || h.is_null() {
        static N: AtomicUsize = AtomicUsize::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 8 {
            record("d3d12_xs", "alias", &format!("CreateHeap({size:#x}) for a GPU-local placement hr={hr:#010x}; placement falls back to a committed resource"));
        }
        return None;
    }
    crate::msaa_place::note_heap(h as usize, heap_align);
    Some(h)
}

pub fn round_up(v: u64, a: u64) -> u64 {
    (v + a - 1) / a * a
}

fn housekeeping(sp: &mut Spans) {
    let notes = core::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()));
    for (lo, hi) in notes {
        sp.retire_overlapping(Kind::Title, lo, hi);
    }
    let gone = core::mem::take(&mut *GONE.lock().unwrap_or_else(|e| e.into_inner()));
    for r in gone {
        sp.resource_gone(r);
    }
    for h in sp.take_idle() {
        crate::msaa_place::forget_heap(h);
        crate::title_shadow::forget_heap(h);
        unsafe { vcall!(h as *mut c_void, 2, fn() -> u32) };
    }
}

#[cfg(test)]
pub(crate) fn flush_spans() {
    if let Some(sp) = SPANS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        housekeeping(sp);
    }
}

pub fn tick() {
    let pending = !GONE.lock().unwrap_or_else(|e| e.into_inner()).is_empty() || !NOTES.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
    if !pending {
        return;
    }
    if let Ok(mut g) = SPANS.try_lock() {
        if let Some(sp) = g.as_mut() {
            housekeeping(sp);
        }
    }
}

unsafe fn migrate_shadow(real: *mut c_void, sp: &mut Spans, va: u64, size: u64) -> bool {
    let Some(k) = sp.find(Kind::Shadow, va, size, 1) else { return false };
    let (lo, hi, shadow) = { let s = sp.get(k); (s.lo, s.hi, s.heap) };
    let Some(h) = (unsafe { create_local_heap(real, hi - lo, false) }) else { return false };
    if !unsafe { crate::title_shadow::migrate(real, shadow, h as usize) } {
        crate::msaa_place::forget_heap(h as usize);
        unsafe { vcall!(h, 2, fn() -> u32) };
        return false;
    }
    STATS.heaps.fetch_add(1, Ordering::Relaxed);
    let s = sp.get_mut(k);
    s.kind = Kind::Local;
    s.heap = h as usize;
    unsafe { vcall!(shadow as *mut c_void, 2, fn() -> u32) };
    true
}

pub struct Made {
    pub hr: i32,
    pub kind: Kind,
    pub heap: usize,
    pub offset: u64,
}

pub unsafe fn create_placed_x(real: *mut c_void, va: u64, pc_desc: &[u8; 0x38], xbox_size: u64, init_state: u32, pclear: *const c_void, ppv: *mut *mut c_void) -> Option<Made> {
    if va == 0 || ppv.is_null() {
        return None;
    }
    let dim = u32::from_le_bytes(pc_desc[0..4].try_into().unwrap());
    let samples = u32::from_le_bytes(pc_desc[0x24..0x28].try_into().unwrap());
    let mut desc = *pc_desc;
    let mut g = SPANS.lock().unwrap_or_else(|e| e.into_inner());
    let sp = g.get_or_insert_with(Spans::default);
    housekeeping(sp);
    let mut chosen: Option<((u64, usize), u64, bool)> = None;
    let mut msaa_align = 0u64;
    if dim == 1 && va % PLACEMENT_ALIGN == 0 && title_buffers_enabled() {
        let width = u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap());
        let flags = u32::from_le_bytes(desc[0x30..0x34].try_into().unwrap());
        desc[0x30..0x34].copy_from_slice(&(flags | ALLOW_CROSS_ADAPTER).to_le_bytes());
        if let Some((base, _end)) = title_allocation(va, width) {
            let key = match sp.find(Kind::Title, va, width, PLACEMENT_ALIGN) {
                Some(k) if sp.get(k).lo == base => Some((k, false)),
                _ => unsafe { open_title_heap(real, base) }.filter(|(_, size)| base + size >= va + width).map(|(h, size)| {
                    STATS.heaps.fetch_add(1, Ordering::Relaxed);
                    (sp.insert(Span { lo: base, hi: base + size, kind: Kind::Title, heap: h as usize, retired: false, live: HashSet::new() }), true)
                }),
            };
            if let Some((k, fresh)) = key {
                chosen = Some((k, va - base, fresh));
            }
        }
        if chosen.is_none() {
            desc = *pc_desc;
        }
    }
    if chosen.is_none() && dim == 1 && va % PLACEMENT_ALIGN == 0 && crate::title_shadow::enabled() {
        let width = u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap());
        let local_covers = local_enabled() && sp.find(Kind::Local, va, width, PLACEMENT_ALIGN).is_some();
        if let Some(r) = reservation(va).filter(|_| !local_covers) {
            let key = match sp.find(Kind::Shadow, va, width, PLACEMENT_ALIGN) {
                Some(k) => Some((k, false)),
                None => {
                    let (lo, hi) = local_span(va, width, Some(r));
                    unsafe { crate::title_shadow::create_heap(real, lo, hi) }.map(|h| {
                        STATS.heaps.fetch_add(1, Ordering::Relaxed);
                        (sp.insert(Span { lo, hi, kind: Kind::Shadow, heap: h, retired: false, live: HashSet::new() }), true)
                    })
                }
            };
            if let Some((k, fresh)) = key {
                chosen = Some((k, va - k.0, fresh));
            }
        }
    }
    if chosen.is_none() && local_enabled() && (samples <= 1 || dim >= 2) && (dim >= 2 || va % PLACEMENT_ALIGN == 0) {
        let flags = u32::from_le_bytes(desc[0x30..0x34].try_into().unwrap());
        let mut info = (0u64, 0u64);
        if dim >= 2 && flags & 0x3 == 0 && va % 0x1000 == 0 {
            let mut small = desc;
            small[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
            let si = unsafe { allocation_info(real, &small) };
            if si.1 == 0x1000 && si.0 != u64::MAX {
                desc = small;
                info = si;
            }
        }
        if info.1 == 0 {
            info = unsafe { allocation_info(real, &desc) };
        }
        let (size, align) = info;
        if size != 0 && size != u64::MAX && align != 0 && align <= crate::msaa_place::max_align(samples) && va % align.min(PLACEMENT_ALIGN) == 0 {
            let tiles = samples > 1 && crate::msaa_place::tiles_usable();
            if samples <= 1 || tiles {
                unsafe { migrate_shadow(real, sp, va, size) };
            }
            let private = if dim >= 2 { oversize_span(va, size, xbox_size) } else { None };
            let key = match (private, sp.find(Kind::Local, va, size, crate::msaa_place::span_align(samples, align))) {
                (Some((lo, hi)), _) => unsafe { create_local_heap(real, size, samples > 1) }.map(|h| {
                    STATS.heaps.fetch_add(1, Ordering::Relaxed);
                    static N: AtomicUsize = AtomicUsize::new(0);
                    if N.fetch_add(1, Ordering::Relaxed) < 16 {
                        record("d3d12_xs", "alias", &format!("va={va:#x} PC texture needs {size:#x} but the title layout is {xbox_size:#x}: own heap, title VA range [{lo:#x}, {hi:#x}) only, so later placements after it never share its PC tail"));
                    }
                    (sp.insert(Span { lo, hi, kind: Kind::Local, heap: h as usize, retired: false, live: HashSet::new() }), true)
                }),
                (None, Some(k)) if samples > 1 && !tiles && !crate::msaa_place::capable(sp.get(k).heap) => None,
                (None, Some(k)) => Some((k, false)),
                (None, None) => {
                    let (lo, hi) = local_span(va, size.max(xbox_size), reservation(va));
                    let fits = tiles || crate::msaa_place::span_fits(samples, va, lo, align);
                    if fits { unsafe { create_local_heap(real, hi - lo, samples > 1) } } else { None }.map(|h| {
                        STATS.heaps.fetch_add(1, Ordering::Relaxed);
                        (sp.insert(Span { lo, hi, kind: Kind::Local, heap: h as usize, retired: false, live: HashSet::new() }), true)
                    })
                }
            };
            if let Some((k, fresh)) = key {
                chosen = Some((k, va - k.0, fresh));
                msaa_align = align;
            }
        }
    }
    let Some((key, offset, fresh)) = chosen else {
        STATS.fallback.fetch_add(1, Ordering::Relaxed);
        return None;
    };
    let (heap, kind) = { let s = sp.get(key); (s.heap, s.kind) };
    let tiled = kind == Kind::Local && crate::msaa_place::needs_tiles(samples, offset, msaa_align, crate::msaa_place::capable(heap));
    let hr = if kind == Kind::Shadow {
        unsafe { crate::title_shadow::place_reserved(real, heap, offset, &desc, init_state, ppv) }
    } else if tiled {
        let hr = unsafe { crate::title_shadow::place_tiled(real, heap, offset, &desc, init_state, pclear, ppv) };
        if hr < 0 && hr != 0x8007_000eu32 as i32 {
            crate::msaa_place::note_tiles_refused(hr);
        }
        hr
    } else {
        unsafe {
            vcall!(real, 29, fn(*mut c_void, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32,
                heap as *mut c_void, offset, desc.as_ptr(), init_state, pclear, &IID_RESOURCE, ppv)
        }
    };
    if hr < 0 || unsafe { *ppv }.is_null() {
        static N: AtomicUsize = AtomicUsize::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 16 {
            record("d3d12_xs", "alias", &format!("CreatePlacedResource({kind:?} heap {heap:#x} +{offset:#x}) for va={va:#x} hr={hr:#010x}; falling back to a committed resource"));
        }
        let _ = fresh;
        housekeeping(sp);
        STATS.fallback.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    let res = unsafe { *ppv };
    if !unsafe { watch(res) } {
        record("d3d12_xs", "alias", &format!("SetPrivateDataInterface on {res:p} failed; its heap stays cached for the process"));
    }
    let pending = core::mem::take(&mut *GONE.lock().unwrap_or_else(|e| e.into_inner()));
    for r in pending {
        sp.resource_gone(r);
    }
    sp.get_mut(key).live.insert(res as usize);
    ALIASED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(res as usize);
    if kind == Kind::Shadow {
        crate::title_shadow::add_resource(heap, res as usize, va, u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap()));
    }
    match kind {
        Kind::Title | Kind::Shadow => STATS.title.fetch_add(1, Ordering::Relaxed),
        Kind::Local => STATS.local.fetch_add(1, Ordering::Relaxed),
    };
    if !fresh {
        STATS.shared.fetch_add(1, Ordering::Relaxed);
    }
    let total = STATS.title.load(Ordering::Relaxed) + STATS.local.load(Ordering::Relaxed);
    if total <= 16 || total.is_power_of_two() {
        record("d3d12_xs", "alias", &format!(
            "va={va:#x} dim={dim} -> {kind:?} heap {heap:#x} +{offset:#x}{} | totals: title={} local={} sharedWithEarlier={} fallback={} heapsMade={} heapsLive={}",
            if fresh { " (new heap)" } else { " (shares an existing heap)" },
            STATS.title.load(Ordering::Relaxed), STATS.local.load(Ordering::Relaxed), STATS.shared.load(Ordering::Relaxed),
            STATS.fallback.load(Ordering::Relaxed), STATS.heaps.load(Ordering::Relaxed), sp.len()));
    }
    Some(Made { hr, kind, heap, offset })
}

pub fn heap_count() -> usize {
    SPANS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |s| s.len())
}

#[allow(dead_code)]
pub fn spans_starting_at(lo: u64) -> usize {
    SPANS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(0, |s| s.by_lo.get(&lo).map_or(0, |v| v.len()))
}

#[allow(dead_code)]
pub fn live_by_heap() -> HashMap<usize, usize> {
    let g = SPANS.lock().unwrap_or_else(|e| e.into_inner());
    let mut m = HashMap::new();
    if let Some(sp) = g.as_ref() {
        for v in sp.by_lo.values() {
            for s in v {
                m.insert(s.heap, s.live.len());
            }
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, ty: u32, protect: u32) -> *mut c_void;
    }

    pub(crate) fn flush() {
        super::flush_spans();
    }

    fn span(lo: u64, hi: u64, kind: Kind, heap: usize) -> Span {
        Span { lo, hi, kind, heap, retired: false, live: HashSet::new() }
    }

    #[test]
    fn same_va_resolves_to_same_heap_and_offset() {
        let mut s = Spans::default();
        let k = s.insert(span(0x10_0000, 0x50_0000, Kind::Local, 0xaa));
        assert_eq!(s.find(Kind::Local, 0x10_0000, 0x1000, 0x10000), Some(k));
        assert_eq!(s.find(Kind::Local, 0x20_0000, 0x10000, 0x10000), Some(k));
        assert_eq!(s.find(Kind::Local, 0x20_1000, 0x10000, 0x10000), None);
        assert_eq!(s.find(Kind::Local, 0x4f_0000, 0x20000, 0x10000), None);
        assert_eq!(s.find(Kind::Title, 0x10_0000, 0x1000, 0x10000), None);
        s.get_mut(k).live.insert(7);
        s.get_mut(k).live.insert(8);
        s.resource_gone(7);
        assert!(s.take_idle().is_empty());
        assert_eq!(s.retire_overlapping(Kind::Local, 0x30_0000, 0x30_1000), 1);
        assert_eq!(s.find(Kind::Local, 0x10_0000, 0x1000, 0x10000), None);
        s.resource_gone(8);
        assert_eq!(s.take_idle(), vec![0xaa]);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn local_span_covers_the_reservation() {
        assert_eq!(local_span(0x7828_0000, 0x50_0000, Some((0x7800_0000, 0x7c00_0000))), (0x7800_0000, 0x7c00_0000));
        assert_eq!(local_span(0x4_1234_0000, 0x1_0000, Some((0x4_0000_0000, 0x5_0000_0000))), (0x4_1234_0000, 0x4_1274_0000));
        assert_eq!(local_span(0x4_1234_0000, 0x80_0000, Some((0x4_0000_0000, 0x5_0000_0000))), (0x4_1234_0000, 0x4_12b4_0000));
        assert_eq!(local_span(0x5691_1000, 0x2000, None), (0x5691_0000, 0x5692_0000));
        assert_eq!(local_span(0x5691_0000, 0x2_0000, Some((0x5691_0000, 0x5691_8000))), (0x5691_0000, 0x5693_0000));
    }

    unsafe fn device() -> *mut c_void {
        let p = crate::host_proc(b"D3D12CreateDevice\0");
        assert!(p != 0);
        let f: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
        let mut d: *mut c_void = core::ptr::null_mut();
        assert!(f(core::ptr::null_mut(), 0xc000, &IID_DEVICE, &mut d) >= 0);
        d
    }

    fn buf_desc(width: u64, flags: u32) -> [u8; 0x38] {
        let mut bd = [0u8; 0x38];
        bd[0..4].copy_from_slice(&1u32.to_le_bytes());
        bd[0x10..0x18].copy_from_slice(&width.to_le_bytes());
        bd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
        bd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
        bd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
        bd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        bd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
        bd[0x30..0x34].copy_from_slice(&flags.to_le_bytes());
        bd
    }

    fn tex_desc(w: u64, h: u32, fmt: u32, flags: u32) -> [u8; 0x38] {
        let mut d = [0u8; 0x38];
        d[0..4].copy_from_slice(&3u32.to_le_bytes());
        d[0x10..0x18].copy_from_slice(&w.to_le_bytes());
        d[0x18..0x1c].copy_from_slice(&h.to_le_bytes());
        d[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
        d[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
        d[0x20..0x24].copy_from_slice(&fmt.to_le_bytes());
        d[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        d[0x30..0x34].copy_from_slice(&flags.to_le_bytes());
        d
    }

    #[test]
    fn placed_texture_that_outgrows_its_heap_becomes_committed() {
        const BC1_UNORM: u32 = 71;
        const HEAP_ALLOW_ONLY_NON_RT_DS_TEXTURES: u32 = 0x44;
        unsafe {
            let dev = device();
            let desc = tex_desc(4096, 4096, BC1_UNORM, 0);
            let (pc_size, _) = allocation_info(dev, &desc);
            assert!(pc_size > PLACEMENT_ALIGN && pc_size != u64::MAX);
            let mk_heap = |size: u64| {
                let mut hd = [0u8; 48];
                hd[0..8].copy_from_slice(&size.to_le_bytes());
                hd[8..12].copy_from_slice(&1u32.to_le_bytes());
                hd[40..44].copy_from_slice(&HEAP_ALLOW_ONLY_NON_RT_DS_TEXTURES.to_le_bytes());
                let mut h: *mut c_void = core::ptr::null_mut();
                let hr = vcall!(dev, 28, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, hd.as_ptr(), &IID_HEAP, &mut h);
                assert!(hr >= 0 && !h.is_null(), "CreateHeap hr={hr:#010x}");
                h
            };
            let release = |o: *mut c_void| {
                vcall!(o, 2, fn() -> u32);
            };

            let small = mk_heap(pc_size - PLACEMENT_ALIGN);
            let mut res: *mut c_void = core::ptr::null_mut();
            let hr = committed_when_too_big(dev, small, 0, &desc, 0, core::ptr::null(), &mut res).expect("texture does not fit");
            assert!(hr >= 0 && !res.is_null(), "committed fallback hr={hr:#010x}");
            release(res);

            let big = mk_heap(pc_size);
            let mut none: *mut c_void = core::ptr::null_mut();
            assert!(committed_when_too_big(dev, big, 0, &desc, 0, core::ptr::null(), &mut none).is_none());
            assert!(none.is_null());

            assert!(committed_when_too_big(dev, small, 0, &buf_desc(0x1000, 0), 0, core::ptr::null(), &mut none).is_none());
            release(small);
            release(big);
            release(dev);
        }
    }

    struct Gpu {
        dev: *mut c_void,
        queue: *mut c_void,
        alloc: *mut c_void,
        list: *mut c_void,
        fence: *mut c_void,
        value: u64,
    }

    const IID_ALLOCATOR: Guid = Guid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
    const IID_GFX_LIST: Guid = Guid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
    const IID_FENCE: Guid = Guid(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);
    const IID_QUEUE: Guid = Guid(0x0ec870a6, 0x5d7e, 0x4c22, [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed]);

    impl Gpu {
        unsafe fn new() -> Gpu {
            let dev = device();
            let mut g = Gpu { dev, queue: core::ptr::null_mut(), alloc: core::ptr::null_mut(), list: core::ptr::null_mut(), fence: core::ptr::null_mut(), value: 0 };
            let qd = [0u32; 4];
            assert!(vcall!(dev, 8, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &IID_QUEUE, &mut g.queue) >= 0);
            assert!(vcall!(dev, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut g.alloc) >= 0);
            assert!(vcall!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, g.alloc, core::ptr::null_mut(), &IID_GFX_LIST, &mut g.list) >= 0);
            assert!(vcall!(dev, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut g.fence) >= 0);
            vcall!(g.list, 9, fn() -> i32);
            g
        }

        unsafe fn run(&mut self, rec: impl FnOnce(*mut c_void)) {
            vcall!(self.alloc, 8, fn() -> i32);
            vcall!(self.list, 10, fn(*mut c_void, *mut c_void) -> i32, self.alloc, core::ptr::null_mut());
            rec(self.list);
            assert!(vcall!(self.list, 9, fn() -> i32) >= 0);
            let l = [self.list];
            vcall!(self.queue, 10, fn(u32, *const *mut c_void) -> (), 1, l.as_ptr());
            self.value += 1;
            vcall!(self.queue, 14, fn(*mut c_void, u64) -> i32, self.fence, self.value);
            let mut n = 0;
            while vcall!(self.fence, 8, fn() -> u64) < self.value {
                std::thread::sleep(std::time::Duration::from_millis(1));
                n += 1;
                assert!(n < 5000);
            }
        }

        unsafe fn committed(&self, heap_type: u32, size: u64, state: u32) -> *mut c_void {
            let heap: [u32; 5] = [heap_type, 0, 0, 1, 1];
            let bd = buf_desc(size, 0);
            let mut r: *mut c_void = core::ptr::null_mut();
            assert!(vcall!(self.dev, 27, fn(*const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32, heap.as_ptr(), 0, bd.as_ptr(), state, core::ptr::null(), &IID_RESOURCE, &mut r) >= 0);
            r
        }
    }

    #[test]
    fn two_placements_at_one_va_share_title_memory() {
        unsafe {
            FORCE_TITLE.with(|f| f.set(true));
            let mut g = Gpu::new();
            let base = VirtualAlloc(core::ptr::null_mut(), 1 << 20, 0x3000, 4) as u64;
            let va = base + 0x2_0000;
            let d = buf_desc(0x1_0000, 0);
            let mut a: *mut c_void = core::ptr::null_mut();
            let mut b: *mut c_void = core::ptr::null_mut();
            let ma = create_placed_x(g.dev, va, &d, 0x1_0000, 0, core::ptr::null(), &mut a).expect("first placement aliased");
            let mb = create_placed_x(g.dev, va, &d, 0x1_0000, 0, core::ptr::null(), &mut b).expect("second placement aliased");
            assert_eq!((ma.kind, mb.kind), (Kind::Title, Kind::Title));
            assert_eq!((ma.heap, ma.offset), (mb.heap, mb.offset));
            assert_eq!(ma.offset, 0x2_0000);
            assert!(is_aliased(a) && is_aliased(b));
            assert_eq!(vcall!(a, 11, fn() -> u64), vcall!(b, 11, fn() -> u64));
            let cpu = va as *mut u32;
            *cpu.add(3) = 0xfeed_beef;
            let rb = g.committed(3, 0x1_0000, 0x400);
            g.run(|l| { vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, b, 0, 0x100); });
            let mut m: *mut u32 = core::ptr::null_mut();
            assert!(vcall!(rb, 8, fn(u32, *const c_void, *mut *mut u32) -> i32, 0, core::ptr::null(), &mut m) >= 0);
            assert_eq!(*m.add(3), 0xfeed_beef);
            let up = g.committed(2, 0x1_0000, 0xac3);
            let mut u: *mut u32 = core::ptr::null_mut();
            assert!(vcall!(up, 8, fn(u32, *const c_void, *mut *mut u32) -> i32, 0, core::ptr::null(), &mut u) >= 0);
            *u.add(5) = 0x1234_5678;
            g.run(|l| { vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), a, 0, up, 0, 0x100); });
            assert_eq!(*cpu.add(5), 0x1234_5678);
            let mut mapped: *mut u8 = core::ptr::null_mut();
            assert!(vcall!(a, 8, fn(u32, *const c_void, *mut *mut u8) -> i32, 0, core::ptr::null(), &mut mapped) >= 0);
            assert_eq!(mapped as u64, va);
            assert_eq!(*(mapped as *const u32).add(5), 0x1234_5678);
            vcall!(a, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
            let live = live_by_heap();
            assert_eq!(live.get(&ma.heap), Some(&2));
            vcall!(a, 2, fn() -> u32);
            vcall!(b, 2, fn() -> u32);
            let mut c: *mut c_void = core::ptr::null_mut();
            let mc = create_placed_x(g.dev, va + 0x1_0000, &d, 0x1_0000, 0, core::ptr::null(), &mut c).expect("third placement aliased");
            assert_eq!(mc.offset, 0x3_0000);
            assert_eq!(spans_starting_at(base), 1);
            assert_eq!(live_by_heap().get(&mc.heap), Some(&1));
            let unaligned = base + 0x3_2000;
            let mut e: *mut c_void = core::ptr::null_mut();
            assert!(create_placed_x(g.dev, unaligned, &d, 0x1_0000, 0, core::ptr::null(), &mut e).is_none());
            vcall!(c, 2, fn() -> u32);
        }
    }

    extern "system" {
        fn CreateFileMappingW(file: isize, attrs: *const c_void, protect: u32, hi: u32, lo: u32, name: *const u16) -> *mut c_void;
        fn MapViewOfFileEx(h: *mut c_void, access: u32, hi: u32, lo: u32, n: usize, at: *mut c_void) -> *mut c_void;
        fn UnmapViewOfFile(p: *mut c_void) -> i32;
        fn VirtualFree(p: *mut c_void, n: usize, ty: u32) -> i32;
    }

    #[test]
    fn title_heap_does_not_block_unmap_or_free() {
        unsafe {
            FORCE_TITLE.with(|f| f.set(true));
            let mut g = Gpu::new();
            let fm = CreateFileMappingW(-1, core::ptr::null(), 0x04 | 0x800_0000, 0, 0x4_0000, core::ptr::null());
            assert!(!fm.is_null());
            let v = MapViewOfFileEx(fm, 0x6, 0, 0, 0x4_0000, core::ptr::null_mut());
            let d = buf_desc(0x1_0000, 0);
            let mut a: *mut c_void = core::ptr::null_mut();
            let ma = create_placed_x(g.dev, v as u64 + 0x1_0000, &d, 0x1_0000, 0, core::ptr::null(), &mut a);
            assert!(ma.as_ref().map_or(true, |m| m.kind != Kind::Title), "a mapped view must never be pinned by a title heap");
            assert_eq!(UnmapViewOfFile(v), 1);
            if !a.is_null() {
                vcall!(a, 2, fn() -> u32);
            }
            let mut d3: *mut c_void = core::ptr::null_mut();
            assert!(vcall!(g.dev, 0, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE3, &mut d3) >= 0);
            let v2 = MapViewOfFileEx(fm, 0x6, 0, 0, 0x4_0000, core::ptr::null_mut());
            let mut h: *mut c_void = core::ptr::null_mut();
            assert!(vcall!(d3, 48, fn(*const c_void, *const Guid, *mut *mut c_void) -> i32, v2, &IID_HEAP, &mut h) >= 0);
            assert_eq!(UnmapViewOfFile(v2), 0, "the PC runtime pins a view it opened a heap on, which is why mapped views are excluded");
            vcall!(h, 2, fn() -> u32);
            vcall!(d3, 2, fn() -> u32);
            let p = VirtualAlloc(core::ptr::null_mut(), 0x2_0000, 0x3000, 4);
            let mut b: *mut c_void = core::ptr::null_mut();
            let mb = create_placed_x(g.dev, p as u64, &d, 0x1_0000, 0, core::ptr::null(), &mut b).expect("private placement aliased");
            assert_eq!(mb.kind, Kind::Title);
            assert_eq!(VirtualFree(p, 0, 0x8000), 0, "the PC runtime pins private memory under an open heap");
            let heap = mb.heap as *mut c_void;
            vcall!(heap, 1, fn() -> u32);
            vcall!(b, 2, fn() -> u32);
            flush();
            assert_eq!(spans_starting_at(p as u64), 0, "the flush must drop the title span once its last placed resource is released");
            assert_eq!(vcall!(heap, 2, fn() -> u32), 0, "after the flush nothing but the test may still reference the title heap");
            let t = std::time::Instant::now();
            while VirtualFree(p, 0, 0x8000) == 0 {
                assert!(t.elapsed() < std::time::Duration::from_secs(10), "the OS still pins the memory 10 s after its title heap was destroyed");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn two_textures_at_one_va_share_gpu_local_heap() {
        unsafe {
            let dev = device();
            let va = 0x7000_0000_0000u64;
            let rt = tex_desc(256, 256, 28, 0x1);
            let other = tex_desc(128, 128, 10, 0x1);
            let mut a: *mut c_void = core::ptr::null_mut();
            let mut b: *mut c_void = core::ptr::null_mut();
            let ma = create_placed_x(dev, va, &rt, 0x4_0000, 0x4, core::ptr::null(), &mut a).expect("rt placed");
            let mb = create_placed_x(dev, va, &other, 0x2_0000, 0x4, core::ptr::null(), &mut b).expect("alias placed");
            assert_eq!(ma.kind, Kind::Local);
            assert_eq!((ma.heap, ma.offset), (mb.heap, mb.offset));
            let mut c: *mut c_void = core::ptr::null_mut();
            let mc = create_placed_x(dev, va + 0x1_0000, &other, 0x2_0000, 0x4, core::ptr::null(), &mut c).expect("inner placed");
            assert_eq!((mc.heap, mc.offset), (ma.heap, 0x1_0000));
            for r in [a, b, c] {
                vcall!(r, 2, fn() -> u32);
            }
            let mut d: *mut c_void = core::ptr::null_mut();
            create_placed_x(dev, va + 0x100_0000, &other, 0x2_0000, 0x4, core::ptr::null(), &mut d).expect("later placement");
            assert_eq!(spans_starting_at(va), 0);
            vcall!(d, 2, fn() -> u32);
        }
    }

    #[test]
    fn placement_uses_the_layout_the_title_was_told() {
        let mut depth = tex_desc(1920, 1080, 19, 0x2);
        depth[0x24..0x28].copy_from_slice(&2u32.to_le_bytes());
        let xg = |_: &[u8]| -> Result<(u64, u64), i32> { Ok((0x13e_c000, 0x2000)) };
        assert_eq!(crate::device_wrap::title_layout_size_with(&depth, Some(&xg)), 0x13e_c000);
        assert_eq!(crate::device_wrap::title_layout_size_with(&depth, None), 0x1fe_0000);
        let pc = 0x1fe_0000u64;
        let told = crate::device_wrap::title_layout_size_with(&depth, Some(&xg));
        assert_eq!(oversize_span(0x64f9_0000, pc, told), Some((0x64f9_0000, 0x6638_0000)));
        assert_eq!(oversize_span(0x64f9_0000, pc, crate::device_wrap::title_layout_size_with(&depth, None)), None);
    }

    #[test]
    fn oversize_span_only_when_the_pc_layout_outgrows_the_title_layout() {
        assert_eq!(oversize_span(0x64f9_0000, 0x1fe_0000, 0x13e_c000), Some((0x64f9_0000, 0x64f9_0000 + 0x13f_0000)));
        assert_eq!(oversize_span(0x64f9_0000, 0x13f_0000, 0x13e_c000), None);
        assert_eq!(oversize_span(0x64f9_0000, 0x1fe_0000, 0x1fe_0000), None);
        assert_eq!(oversize_span(0x64f9_0000, 0x1fe_0000, 0), None);
    }

    #[test]
    fn msaa_depth_with_a_larger_pc_layout_never_overlaps_the_next_title_placement() {
        unsafe {
            let dev = device();
            let r = VirtualAlloc(core::ptr::null_mut(), 0x400_0000, 0x2000, 0x01) as u64;
            assert!(r != 0);
            let small = tex_desc(256, 256, 28, 0x1);
            let small_size = allocation_info(dev, &small).0.max(0x4_0000);
            let mut first: *mut c_void = core::ptr::null_mut();
            let m0 = create_placed_x(dev, r, &small, small_size, 0x4, core::ptr::null(), &mut first).expect("first placement opens the reservation-wide span");
            assert_eq!(m0.kind, Kind::Local);
            let mut depth = tex_desc(1920, 1080, 19, 0x2);
            depth[0x24..0x28].copy_from_slice(&2u32.to_le_bytes());
            let pc = allocation_info(dev, &depth);
            let console = 0x13e_c000u64;
            assert!(pc.0 > round_up(console, PLACEMENT_ALIGN), "this adapter's R32G8X24 2x layout {:#x} is not larger than the console layout", pc.0);
            let dva = r + 0x40_0000;
            let mut clear = [0u8; 0x18];
            clear[0..4].copy_from_slice(&20u32.to_le_bytes());
            clear[4..8].copy_from_slice(&1f32.to_le_bytes());
            let mut d: *mut c_void = core::ptr::null_mut();
            let md = create_placed_x(dev, dva, &depth, console, 0x10, clear.as_ptr() as *const c_void, &mut d).expect("MSAA depth placed");
            assert_eq!(md.kind, Kind::Local);
            assert_eq!(md.offset, 0);
            assert_ne!(md.heap, m0.heap, "the oversize depth must not sit in the shared reservation heap");
            let next_va = dva + round_up(console, PLACEMENT_ALIGN);
            let mut n: *mut c_void = core::ptr::null_mut();
            let mn = create_placed_x(dev, next_va, &small, small_size, 0x4, core::ptr::null(), &mut n).expect("next title placement");
            assert!(mn.heap != md.heap || mn.offset >= md.offset + pc.0, "next placement {:#x}+{:#x} overlaps the depth's PC tail", mn.heap, mn.offset);
            assert_eq!((mn.heap, mn.offset), (m0.heap, next_va - r));
            let mut a: *mut c_void = core::ptr::null_mut();
            let ma = create_placed_x(dev, dva, &small, small_size, 0x4, core::ptr::null(), &mut a).expect("alias inside the console extent");
            assert_eq!((ma.heap, ma.offset), (md.heap, 0), "title aliases inside the console extent share the depth's memory");
            for x in [first, d, n, a] {
                vcall!(x, 2, fn() -> u32);
            }
            flush();
            VirtualFree(r as *mut c_void, 0, 0x8000);
            vcall!(dev, 2, fn() -> u32);
        }
    }
}
