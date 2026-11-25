//! Texel transfer for placed (unified-memory) textures.
//!
//! On console a texture created with `CreatePlacedResourceX` lives at a title-chosen
//! address and the title fills it by writing memory in the console's tiled layout; there is
//! no API call when texels change. This module mirrors that memory into the PC texture:
//!
//!   * layout: the *real* `xg_xs.dll` shipped with the title (it runs natively) is asked for
//!     a texture address computer for the title's own resource desc. Verified with
//!     `tools/dev/xgprobe.rs`: vtable slot 2 = GetResourceLayout, 3 = size, 6 =
//!     GetTexelElementOffsetBytes(plane, mip, x: u64, y, zOrSlice, sample, u32* bitPos),
//!     8 = CopyIntoSubresource; slot 6 offsets matched slot 8's output for every element of
//!     five texture shapes (BC1/BC3/BGRA8/R8, 2D + cube, layout 261). Slot 9
//!     (CopyFromSubresource) refuses to run off-console (needs an exclusive-lock object),
//!     so de-tiling is done here from the slot-6 element offsets.
//!   * change detection: each `PresentX` hashes the title memory of tracked subresources
//!     (never-uploaded ones first, the rest round-robin under a byte budget) and re-uploads
//!     the ones whose hash changed.
//!   * lifetime: the registry holds one reference per placed texture; when it is the only
//!     one left the title has released the texture and tracking stops.
//!
//! Assumptions / limits (see docs/TECHNICAL_DEBT.md):
//!   * the PC texture is assumed to be in its creation state when the upload list runs
//!     (the title's own barriers are not tracked);
//!   * memory that is not committed/readable is skipped and retried later;
//!   * Texture3D, multi-plane and render-target/depth placed textures are not mirrored.

use core::ffi::c_void;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use compat_common::record;
use crate::placed::Placed;

extern "system" {
    fn VirtualQuery(addr: *const c_void, mbi: *mut Mbi, len: usize) -> usize;
    fn GetModuleHandleA(n: *const u8) -> *mut c_void;
    fn LoadLibraryA(n: *const u8) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
}

#[repr(C)]
struct Mbi {
    base: usize,
    alloc_base: usize,
    alloc_protect: u32,
    partition: u32,
    region_size: usize,
    state: u32,
    protect: u32,
    ty: u32,
    _pad: u32,
}

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);
const IID_DEVICE: Guid = Guid(0x189819f1, 0x1db6, 0x4b57, [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7]);
const IID_ALLOCATOR: Guid = Guid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
const IID_GFX_LIST: Guid = Guid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
const IID_FENCE: Guid = Guid(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);
const IID_RESOURCE: Guid = Guid(0x696442be, 0xa72e, 0x4059, [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad]);

unsafe fn vt(obj: *mut c_void) -> *const usize {
    *(obj as *const *const usize)
}
macro_rules! vcall {
    ($obj:expr, $slot:expr, fn($($at:ty),*) -> $rt:ty $(, $arg:expr)*) => {{
        let f: unsafe extern "system" fn(*mut c_void $(, $at)*) -> $rt =
            core::mem::transmute(*vt($obj).add($slot));
        f($obj $(, $arg)*)
    }};
}

struct Scan {
    ok: Vec<(usize, usize)>,
    bad: Option<(usize, u32, u32, u32, usize, usize)>,
    bads: Vec<(usize, usize, u32, u32, u32, usize, usize)>,
}

fn scan(addr: usize, len: usize) -> Scan {
    let end = addr + len;
    let mut p = addr;
    let mut s = Scan { ok: Vec::new(), bad: None, bads: Vec::new() };
    while p < end {
        let mut m: Mbi = unsafe { core::mem::zeroed() };
        rs_add(RS_QUERIES, 1);
        if unsafe { VirtualQuery(p as *const c_void, &mut m, core::mem::size_of::<Mbi>()) } == 0 {
            if s.bad.is_none() {
                s.bad = Some((p, 0, 0, 0, 0, 0));
            }
            s.bads.push((p, end, 0, 0, 0, 0, 0));
            break;
        }
        let next = (m.base + m.region_size).min(end).max(p + 1);
        let prot = m.protect;
        let ok = m.state == 0x1000 && prot & 0x100 == 0 && matches!(prot & 0xff, 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80);
        if ok {
            match s.ok.last_mut() {
                Some(last) if last.1 == p => last.1 = next,
                _ => s.ok.push((p, next)),
            }
        } else {
            if s.bad.is_none() {
                s.bad = Some((p, m.state, m.protect, m.ty, m.base, m.region_size));
            }
            s.bads.push((p, next, m.state, m.protect, m.ty, m.base, m.region_size));
        }
        p = next;
    }
    s
}

impl Scan {
    fn clip(&self, lo: usize, len: usize) -> Scan {
        let hi = lo + len;
        let mut s = Scan { ok: Vec::new(), bad: None, bads: Vec::new() };
        let mut covered = lo;
        let mut gap: Option<usize> = None;
        for &(a, b) in &self.ok {
            let (a, b) = (a.max(lo), b.min(hi));
            if a >= b {
                continue;
            }
            if a > covered && gap.is_none() {
                gap = Some(covered);
            }
            s.ok.push((a, b));
            covered = covered.max(b);
        }
        if covered < hi && gap.is_none() {
            gap = Some(covered);
        }
        if let Some(g) = gap {
            s.bad = Some(self.bads.iter().find(|b| b.0 <= g && g < b.1)
                .map(|b| (g, b.2, b.3, b.4, b.5, b.6))
                .unwrap_or((g, 0, 0, 0, 0, 0)));
        }
        s
    }
}

fn sub_range(t: &Tracked, sub: usize) -> Option<(usize, usize)> {
    let fp = t.fps[sub];
    let bpe = t.layout.bpe as usize;
    let map = unsafe { t.layout.submap(sub as u32, (fp.row_size / bpe as u64) as u32, fp.rows) }?;
    Some((t.p.va as usize + map.lo, map.hi - map.lo))
}

const RS_QUERIES: usize = 0;
const RS_HITS: usize = 1;
const RS_MISSES: usize = 2;
const RS_DROPS: usize = 3;
static RS: [core::sync::atomic::AtomicU64; 4] = [const { core::sync::atomic::AtomicU64::new(0) }; 4];
static RS_TOTAL: [core::sync::atomic::AtomicU64; 4] = [const { core::sync::atomic::AtomicU64::new(0) }; 4];

fn rs_add(i: usize, n: u64) {
    RS[i].fetch_add(n, core::sync::atomic::Ordering::Relaxed);
    RS_TOTAL[i].fetch_add(n, core::sync::atomic::Ordering::Relaxed);
}

fn take_rs() -> [u64; 4] {
    core::array::from_fn(|i| RS[i].swap(0, core::sync::atomic::Ordering::Relaxed))
}

fn rs_total() -> [u64; 4] {
    core::array::from_fn(|i| RS_TOTAL[i].load(core::sync::atomic::Ordering::Relaxed))
}

type EpochFn = unsafe extern "system" fn() -> u64;
type ChangedFn = unsafe extern "system" fn(u64, usize, usize) -> i32;
static EPOCH_FN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static CHANGED_FN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static EPOCH_LAST_TRY_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn epoch_fns() -> Option<(EpochFn, ChangedFn)> {
    use core::sync::atomic::Ordering;
    let (e, c) = (EPOCH_FN.load(Ordering::Acquire), CHANGED_FN.load(Ordering::Acquire));
    if e != 0 && c != 0 {
        return Some(unsafe { (core::mem::transmute::<usize, EpochFn>(e), core::mem::transmute::<usize, ChangedFn>(c)) });
    }
    extern "system" {
        fn GetTickCount64() -> u64;
    }
    let now = unsafe { GetTickCount64() };
    let last = EPOCH_LAST_TRY_MS.load(Ordering::Relaxed);
    if last == u64::MAX || (last != 0 && now.wrapping_sub(last) < 250) {
        return None;
    }
    EPOCH_LAST_TRY_MS.store(now.max(1), Ordering::Relaxed);
    let (e, c) = unsafe {
        let m = GetModuleHandleA(b"xmem.dll\0".as_ptr());
        if m.is_null() {
            return None;
        }
        (GetProcAddress(m, b"TomboxAddressSpaceEpoch\0".as_ptr()) as usize, GetProcAddress(m, b"TomboxAddressSpaceChangedSince\0".as_ptr()) as usize)
    };
    if e == 0 || c == 0 {
        return None;
    }
    EPOCH_FN.store(e, Ordering::Release);
    CHANGED_FN.store(c, Ordering::Release);
    Some(unsafe { (core::mem::transmute::<usize, EpochFn>(e), core::mem::transmute::<usize, ChangedFn>(c)) })
}

#[cfg(test)]
pub(crate) fn test_epoch_fns(f: Option<(usize, usize)>) {
    let (e, c) = f.unwrap_or((0, 0));
    EPOCH_FN.store(e, core::sync::atomic::Ordering::Release);
    CHANGED_FN.store(c, core::sync::atomic::Ordering::Release);
    EPOCH_LAST_TRY_MS.store(if f.is_some() { 0 } else { u64::MAX }, core::sync::atomic::Ordering::Relaxed);
}

fn residency_cache_sw() -> bool {
    static SW: crate::live_sw::Sw = crate::live_sw::Sw::new("TOMBOX_RESIDENCY_CACHE", "residency_cache", true);
    SW.on()
}

fn residency_cache_state() -> &'static str {
    if !residency_cache_sw() {
        "off (TOMBOX_RESIDENCY_CACHE=0)"
    } else if poll_mode() {
        "off (poll mode rescans every visit)"
    } else if EPOCH_FN.load(core::sync::atomic::Ordering::Acquire) == 0 {
        "off (xmem.dll address-space change log not found)"
    } else {
        "on"
    }
}

fn drop_rcache(t: &mut Tracked) {
    if t.rcache.take().is_some() {
        rs_add(RS_DROPS, 1);
    }
}

fn scan_subs(t: &mut Tracked, subs: &[usize]) -> Option<Scan> {
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for &sub in subs {
        if let Some((a, n)) = sub_range(t, sub) {
            lo = lo.min(a);
            hi = hi.max(a + n);
        }
    }
    if lo >= hi {
        return None;
    }
    let (va, total) = (t.p.va as usize, t.layout.total as usize);
    let fns = if residency_cache_sw() && !poll_mode() && lo >= va && hi <= va + total { epoch_fns() } else { None };
    let Some((epoch, changed)) = fns else {
        drop_rcache(t);
        rs_add(RS_MISSES, 1);
        return Some(scan(lo, hi - lo));
    };
    let now = unsafe { epoch() };
    let hit = match t.rcache.as_mut() {
        Some((c, seq)) if *seq == now || unsafe { changed(*seq, va, va + total) } == 0 => {
            *seq = now;
            Some(c.clip(lo, hi - lo))
        }
        _ => None,
    };
    if let Some(h) = hit {
        rs_add(RS_HITS, 1);
        return Some(h);
    }
    drop_rcache(t);
    rs_add(RS_MISSES, 1);
    let c = scan(va, total);
    let out = c.clip(lo, hi - lo);
    if c.bad.is_none() {
        t.rcache = Some((c, now));
    }
    Some(out)
}

fn sparse_zero_enabled() -> bool {
    static SW: crate::live_sw::Sw = crate::live_sw::Sw::new("TOMBOX_SPARSE_ZERO", "sparse_zero", true);
    SW.on()
}

struct HashJob {
    addr: usize,
    len: usize,
    zero_test: bool,
    hash: u64,
    all_zero: bool,
}

struct NonzeroJob {
    addr: usize,
    len: usize,
    found: bool,
}

struct CopyJob {
    src: usize,
    dst: usize,
    len: usize,
}

struct DetileJob {
    dst0: usize,
    pitch: usize,
    src0: usize,
    offs: *const u32,
    offs_len: usize,
    w_e: usize,
    y0: usize,
    y1: usize,
    bpe: usize,
}

const _: () = assert!(!core::mem::needs_drop::<HashJob>());
const _: () = assert!(!core::mem::needs_drop::<NonzeroJob>());
const _: () = assert!(!core::mem::needs_drop::<CopyJob>());
const _: () = assert!(!core::mem::needs_drop::<DetileJob>());

unsafe extern "C" fn hash_job(ctx: *mut c_void) {
    let j = &mut *(ctx as *mut HashJob);
    let s = core::slice::from_raw_parts(j.addr as *const u8, j.len);
    j.hash = hash_bytes(s);
    j.all_zero = j.zero_test && s.iter().all(|&b| b == 0);
}

unsafe extern "C" fn nonzero_job(ctx: *mut c_void) {
    let j = &mut *(ctx as *mut NonzeroJob);
    j.found = core::slice::from_raw_parts(j.addr as *const u8, j.len).iter().any(|&x| x != 0);
}

unsafe extern "C" fn copy_job(ctx: *mut c_void) {
    let j = &*(ctx as *const CopyJob);
    core::ptr::copy_nonoverlapping(j.src as *const u8, j.dst as *mut u8, j.len);
}

unsafe extern "C" fn detile_job(ctx: *mut c_void) {
    let j = &*(ctx as *const DetileJob);
    detile_span(j.dst0, j.pitch, j.src0, core::slice::from_raw_parts(j.offs, j.offs_len), j.w_e, j.y0..j.y1, j.bpe);
}

#[track_caller]
fn hash_title(addr: usize, len: usize, zero_test: bool) -> Option<(u64, bool)> {
    let mut j = HashJob { addr, len, zero_test, hash: 0, all_zero: false };
    crate::guard::guarded(addr, len, hash_job, &mut j as *mut HashJob as *mut c_void).then_some((j.hash, j.all_zero))
}

#[track_caller]
fn nonzero_title(addr: usize, len: usize) -> Option<bool> {
    let mut j = NonzeroJob { addr, len, found: false };
    crate::guard::guarded(addr, len, nonzero_job, &mut j as *mut NonzeroJob as *mut c_void).then_some(j.found)
}

#[track_caller]
fn copy_title(src: usize, dst: usize, len: usize) -> bool {
    let j = CopyJob { src, dst, len };
    crate::guard::guarded(src, len, copy_job, &j as *const CopyJob as *mut c_void)
}

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h: u64 = 0x9E37_79B9_7F4A_7C15 ^ b.len() as u64;
    let mut chunks = b.chunks_exact(8);
    for c in &mut chunks {
        let w = u64::from_le_bytes(c.try_into().unwrap());
        h = (h ^ w).wrapping_mul(0x0000_0100_0000_01B3).rotate_left(23);
    }
    for &x in chunks.remainder() {
        h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

// ---- Xbox layout via the real xg_xs.dll ---------------------------------------------------

/// Element offsets of one subresource inside the Xbox resource memory.
struct SubMap {
    w_e: u32,
    h_e: u32,
    offs: Vec<u32>,
    lo: usize,
    hi: usize, // exclusive
    rows: std::sync::OnceLock<Vec<(u32, u32)>>,
}

impl SubMap {
    fn row_extents(&self, bpe: usize) -> &[(u32, u32)] {
        self.rows.get_or_init(|| crate::cpu_watch::row_extents(&self.offs, self.w_e as usize, self.h_e as usize, bpe))
    }
}

struct TexLayout {
    comp: usize, // XGTextureAddressComputer*
    total: u64,
    bpe: u32,
    planes: u32,
    mips: u32,
    subs: Mutex<HashMap<u32, Option<Arc<SubMap>>>>,
}

impl TexLayout {
    unsafe fn create(desc: &[u8; 0x38]) -> Result<TexLayout, String> {
        let (h, bpe_off) = crate::xone_tex::xg_module();
        if h.is_null() {
            return Err("neither xg_xs.dll nor xg_x.dll is loadable".into());
        }
        let f = GetProcAddress(h, b"XGCreateTextureComputer\0".as_ptr());
        if f.is_null() {
            return Err("XGCreateTextureComputer export missing".into());
        }
        let create: unsafe extern "system" fn(*const u8, *mut *mut c_void) -> i32 = core::mem::transmute(f);
        let mut comp: *mut c_void = core::ptr::null_mut();
        let hr = create(desc.as_ptr(), &mut comp);
        if hr < 0 || comp.is_null() {
            return Err(format!("XGCreateTextureComputer hr={hr:#010x}"));
        }
        // XG_RESOURCE_LAYOUT: u64 size, u64 alignment, u32 mips, u32 planes, planes[] of 0x6C8
        // bytes each (title's FD3D12TextureLayout::FillFromDesc copies exactly that); the
        // plane's BytesPerElement is at plane+0x28 (probe).
        let mut lay = vec![0u8; 0x4000];
        let hr = vcall!(comp, 2, fn(*mut u8) -> i32, lay.as_mut_ptr());
        if hr < 0 {
            return Err(format!("GetResourceLayout hr={hr:#010x}"));
        }
        let u64at = |o: usize| u64::from_le_bytes(lay[o..o + 8].try_into().unwrap());
        let u32at = |o: usize| u32::from_le_bytes(lay[o..o + 4].try_into().unwrap());
        Ok(TexLayout {
            comp: comp as usize,
            total: u64at(0),
            mips: u32at(0x10),
            planes: u32at(0x14),
            bpe: u32at(bpe_off),
            subs: Mutex::new(HashMap::new()),
        })
    }

    /// Element map of subresource `sub` (mip + slice * mips), built on first use.
    unsafe fn submap(&self, sub: u32, w_e: u32, h_e: u32) -> Option<Arc<SubMap>> {
        let mut g = self.subs.lock().unwrap();
        if let Some(v) = g.get(&sub) {
            return v.clone();
        }
        let (mip, slice) = (sub % self.mips, sub / self.mips);
        let comp = self.comp as *mut c_void;
        let mut offs = Vec::with_capacity((w_e * h_e) as usize);
        let (mut lo, mut hi) = (usize::MAX, 0usize);
        let mut ok = true;
        'outer: for y in 0..h_e {
            for x in 0..w_e {
                let o = vcall!(comp, 6, fn(u32, u32, u64, u32, u32, u32, *mut u32) -> u64,
                    0, mip, x as u64, y, slice, 0, core::ptr::null_mut());
                if o >= self.total || o + self.bpe as u64 > self.total || o > u32::MAX as u64 {
                    ok = false;
                    break 'outer;
                }
                lo = lo.min(o as usize);
                hi = hi.max(o as usize + self.bpe as usize);
                offs.push(o as u32);
            }
        }
        let v = if ok { Some(Arc::new(SubMap { w_e, h_e, offs, lo, hi, rows: std::sync::OnceLock::new() })) } else { None };
        g.insert(sub, v.clone());
        v
    }
}

fn layout_for(desc: &[u8; 0x38]) -> Result<Arc<TexLayout>, String> {
    static CACHE: Mutex<Option<HashMap<[u8; 0x38], Result<Arc<TexLayout>, String>>>> = Mutex::new(None);
    let mut key = *desc;
    key[8..16].fill(0); // alignment does not affect the layout request we make
    let mut g = CACHE.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(v) = m.get(&key) {
        return v.clone();
    }
    let v = unsafe { TexLayout::create(desc) }.map(Arc::new);
    m.insert(key, v.clone());
    v
}

// ---- tracking + upload -----------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Footprint {
    raw: [u8; 32], // D3D12_PLACED_SUBRESOURCE_FOOTPRINT
    rows: u32,
    row_size: u64,
}
impl Footprint {
    fn offset(&self) -> u64 { u64::from_le_bytes(self.raw[0..8].try_into().unwrap()) }
    fn row_pitch(&self) -> u32 { u32::from_le_bytes(self.raw[0x18..0x1c].try_into().unwrap()) }
}

struct Tracked {
    res: usize,
    p: Placed,
    layout: Arc<TexLayout>,
    fps: Vec<Footprint>,
    total_bytes: u64,
    hashes: Vec<Option<u64>>,
    partial: Vec<usize>,
    zero: Vec<bool>,
    watch: Vec<u64>,
    pend: Vec<u64>,
    hot: u64,
    ww: bool,
    ww_ranges: Vec<(usize, usize, usize)>,
    cpu_dirty: Vec<Vec<(usize, usize)>>,
    sent: Vec<bool>,
    freed: bool,
    rcache: Option<(Scan, u64)>,
}

struct Uploader {
    device: *mut c_void,
    alloc: *mut c_void,
    list: *mut c_void,
    fence: *mut c_void,
    fence_value: u64,
    tracked: Vec<Tracked>,
    retire: Vec<(u64, usize, u64)>,
    pool: Vec<(u64, usize)>,
    pool_bytes: u64,
    backlog: std::collections::VecDeque<(usize, Placed)>,
    release_cursor: usize,
    cursor: usize,
    partial_cursor: usize,
    pumps: u64,
    uploaded_subs: u64,
    uploaded_bytes: u64,
    unreadable: u64,
    rejected: u64,
    recommitted: u64,
    zero_cursor: usize,
    zero_filled: u64,
    watch_changed: u64,
    index: Vec<(usize, usize, usize)>,
    index_stale: bool,
    ev: EventStats,
    cost_sum_us: u64,
    cost_max_us: u64,
    cost_n: u64,
    reaper: crate::reaper::Reaper,
    ww: WwStats,
    queue: *mut c_void,
    main_value: u64,
    ring: Vec<(*mut c_void, *mut c_void, u64)>,
    sb: SubmitStats,
}
unsafe impl Send for Uploader {}

#[derive(Default)]
struct SubmitStats {
    hooks: u64,
    fast: u64,
    us_total: u64,
    us_max: u64,
    uploads: u64,
    subs: u64,
    ring_waits: u64,
    cross_queue: u64,
}

const SUBMIT_RING_MAX: usize = 16;

thread_local! {
    static IN_UPLOAD: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

struct UploadGuard;

impl UploadGuard {
    fn enter() -> Option<UploadGuard> {
        if IN_UPLOAD.with(|c| c.replace(true)) {
            return None;
        }
        Some(UploadGuard)
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        IN_UPLOAD.with(|c| c.set(false));
    }
}

pub fn submit_hook_enabled() -> bool {
    static SW: crate::live_sw::Sw = crate::live_sw::Sw::new("TOMBOX_WW_SUBMIT", "ww_submit", true);
    crate::cpu_watch::enabled() && SW.on()
}

#[derive(Default)]
struct WwStats {
    buf: Vec<usize>,
    classes: HashMap<&'static str, (u64, u64)>,
    logged: std::collections::HashSet<usize>,
    log_lines: [u32; 8],
    queries: u64,
    query_us: u64,
    dirty_pages: u64,
    dirty_subs: u64,
    unchanged: u64,
    backing_freed: u64,
    backing_back: u64,
    band_uploads: u64,
    band_bytes: u64,
    whole_uploads: u64,
    lost: u64,
    lost_res: u64,
    gpu_owned_writes: u64,
    fallbacks: u64,
}

impl WwStats {
    fn once(&mut self, res: usize, kind: u8, msg: impl FnOnce() -> String) {
        let k = (kind as usize).min(7);
        if self.logged.insert(res ^ ((kind as usize) << 56)) && self.log_lines[k] < 200 {
            self.log_lines[k] += 1;
            record("d3d12_xs", "upload", &msg());
        }
    }
}

#[derive(Default)]
struct EventStats {
    io_notes: u64,
    io_bytes: u64,
    io_subs: u64,
    commit_notes: u64,
    commit_bytes: u64,
    commit_subs: u64,
    unmatched: u64,
}

const WATCH_PUMPS: u64 = 30;
const PENDING_PUMPS: u64 = 120;
const ZERO_SCAN_BUDGET: u64 = 64 << 20;
const EVENT_SCAN_BUDGET: u64 = 16 << 20;
pub const NOTE_WRITTEN: u32 = 0;
pub const NOTE_COMMITTED: u32 = 1;
static UP: Mutex<Option<Uploader>> = Mutex::new(None);
static NOTES: Mutex<Vec<(usize, usize, u32)>> = Mutex::new(Vec::new());

fn poll_mode() -> bool {
    static SW: crate::live_sw::Sw = crate::live_sw::Sw::new("TOMBOX_UPLOAD_POLL", "upload_poll", false);
    SW.on()
}

fn res_desc(p: &Placed) -> String {
    format!("{}x{}x{} mips={} fmt={} layout={} flags={:#x}", p.width(), p.height(), p.depth_or_array(), p.mips(), p.format(), p.layout(), p.flags())
}

fn polls(t: &Tracked, poll: bool) -> bool {
    !t.ww && poll
}

#[no_mangle]
pub extern "system" fn TomboxNoteTitleMemory(addr: usize, len: usize, kind: u32) {
    if addr == 0 || len == 0 {
        return;
    }
    crate::alias::note_title_memory(addr, len, kind);
    let mut g = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(last) = g.last_mut() {
        if last.2 == kind && last.0 + last.1 == addr {
            last.1 += len;
            return;
        }
    }
    g.push((addr, len, kind));
}

fn take_notes() -> Vec<(usize, usize, u32)> {
    core::mem::take(&mut *NOTES.lock().unwrap_or_else(|e| e.into_inner()))
}

fn rebuild_index(up: &mut Uploader) {
    up.index.clear();
    for (i, t) in up.tracked.iter().enumerate() {
        let lo = t.p.va as usize;
        up.index.push((lo, lo + t.layout.total as usize, i));
    }
    up.index.sort_unstable();
    up.index_stale = false;
}

fn overlapping(index: &[(usize, usize, usize)], a: usize, b: usize) -> Vec<usize> {
    let end = index.partition_point(|e| e.0 < b);
    let mut out = Vec::new();
    let mut i = end;
    while i > 0 {
        i -= 1;
        let (_, hi, idx) = index[i];
        if hi > a {
            out.push(idx);
        }
        if end - i > 64 && hi <= a {
            break;
        }
    }
    out
}

fn apply_notes(up: &mut Uploader, notes: &[(usize, usize, u32)]) {
    if notes.is_empty() {
        return;
    }
    if up.index_stale {
        rebuild_index(up);
    }
    let now = up.pumps;
    for &(a, n, kind) in notes {
        let b = a + n;
        if kind == NOTE_WRITTEN {
            up.ev.io_notes += 1;
            up.ev.io_bytes += n as u64;
        } else {
            up.ev.commit_notes += 1;
            up.ev.commit_bytes += n as u64;
        }
        let hits = overlapping(&up.index, a, b);
        if hits.is_empty() {
            up.ev.unmatched += 1;
            continue;
        }
        for idx in hits {
            let t = &mut up.tracked[idx];
            if kind != NOTE_WRITTEN {
                drop_rcache(t);
            }
            if t.freed && kind != NOTE_WRITTEN {
                rebirth(t, &mut up.ww, a, n);
            }
            for sub in 0..t.hashes.len() {
                let Some((lo, len)) = sub_range(t, sub) else { continue };
                if lo >= b || lo + len <= a {
                    continue;
                }
                t.hashes[sub] = None;
                t.zero[sub] = false;
                if kind == NOTE_WRITTEN {
                    up.ev.io_subs += 1;
                } else {
                    t.pend[sub] = now + PENDING_PUMPS;
                    t.hot = t.hot.max(now + PENDING_PUMPS);
                    up.ev.commit_subs += 1;
                }
            }
        }
    }
}

fn any_committed(lo: usize, len: usize) -> bool {
    let mut a = lo & !(crate::cpu_watch::PAGE - 1);
    while a < lo + len {
        if crate::cpu_watch::committed(a, 1) {
            return true;
        }
        a += crate::cpu_watch::PAGE;
    }
    false
}

fn rebirth(t: &mut Tracked, w: &mut WwStats, a: usize, n: usize) {
    let (va, total) = (t.p.va as usize, t.layout.total as usize);
    t.freed = false;
    drop_rcache(t);
    t.hashes.iter_mut().for_each(|h| *h = None);
    t.zero.iter_mut().for_each(|z| *z = false);
    t.cpu_dirty.iter_mut().for_each(|d| d.clear());
    w.backing_back += 1;
    let how = match crate::cpu_watch::watched_ranges(va, total) {
        Some(r) if !r.is_empty() => {
            t.ww = true;
            for sub in 0..t.hashes.len() {
                let Some((lo, len)) = sub_range(t, sub) else { continue };
                for &(x, y, _) in &r {
                    let (p, q) = (x.max(lo), y.min(lo + len));
                    if p < q {
                        t.cpu_dirty[sub].push((p, q));
                    }
                }
            }
            let s = r.iter().map(|&(x, y, g)| format!("{x:#x}..{y:#x} (region {g:#x})")).collect::<Vec<_>>().join(", ");
            t.ww_ranges = r;
            format!("write-watched again over {s}; every overlapping subresource is uploaded now")
        }
        _ => {
            t.ww = false;
            t.ww_ranges.clear();
            "the new memory has no emulator write watch, so it takes the hash path".to_string()
        }
    };
    let (res, d) = (t.res, res_desc(&t.p));
    w.once(res, 8, || format!("backing reborn: placed resource res={res:#x} va={va:#x}+{total:#x} {d} overlaps a new title allocation at {a:#x}+{n:#x}; {how} | at va: {}", crate::cpu_watch::region(va)));
}

fn ww_sync(up: &mut Uploader) {
    if !crate::cpu_watch::enabled() {
        return;
    }
    if up.index_stale {
        rebuild_index(up);
    }
    let mut by_region: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for t in &up.tracked {
        if t.ww {
            for &(a, b, r) in &t.ww_ranges {
                by_region.entry(r).or_default().push((a, b));
            }
        }
    }
    if by_region.is_empty() {
        return;
    }
    let t0 = std::time::Instant::now();
    let mut failed: Vec<(usize, usize, u32)> = Vec::new();
    let mut dirty: Vec<(usize, usize)> = Vec::new();
    for (_, ext) in by_region {
        for (a, b) in crate::cpu_watch::merge_extents(ext) {
            up.ww.queries += 1;
            match crate::cpu_watch::take_dirty(a, b, &mut up.ww.buf) {
                Ok(r) => dirty.extend(r),
                Err(e) => failed.push((a, b, e)),
            }
        }
    }
    up.ww.query_us += t0.elapsed().as_micros() as u64;
    for (a, b, e) in failed {
        for idx in overlapping(&up.index, a, b) {
            let t = &mut up.tracked[idx];
            if !t.ww {
                continue;
            }
            let now_ranges = crate::cpu_watch::watched_ranges(t.p.va as usize, t.layout.total as usize);
            if let Some(r) = now_ranges.as_ref().filter(|r| !r.is_empty()) {
                if *r != t.ww_ranges {
                    t.ww_ranges = r.clone();
                    drop_rcache(t);
                    continue;
                }
            }
            t.ww = false;
            drop_rcache(t);
            t.cpu_dirty.iter_mut().for_each(|d| d.clear());
            if now_ranges.as_ref().is_some_and(|r| r.is_empty()) || !any_committed(t.p.va as usize, t.layout.total as usize) {
                t.freed = true;
                up.ww.backing_freed += 1;
                let (res, va, d, total) = (t.res, t.p.va, res_desc(&t.p), t.layout.total);
                up.ww.once(res, 3, || format!("backing freed: placed resource res={res:#x} va={va:#x}+{total:#x} {d} is still alive but the title released its memory; it is no longer polled or uploaded (GPU keeps the last uploaded texels) until that memory is committed again | at va: {}", crate::cpu_watch::region(va as usize)));
                continue;
            }
            t.hashes.iter_mut().for_each(|h| *h = None);
            up.ww.fallbacks += 1;
            let (res, va, d, total) = (t.res, t.p.va, res_desc(&t.p), t.layout.total);
            up.ww.once(res, 3, || {
                let (o, ob, on) = crate::cpu_watch::owner(va as usize);
                let end = va + total;
                format!("GetWriteWatch over {a:#x}..{b:#x} failed (GetLastError {e}); placed resource res={res:#x} va={va:#x}+{total:#x} (end {end:#x}) {d} falls back to the hash path | owner={o} region={ob:#x}+{on:#x} (end {:#x}, resource {} it by {:#x}) | at va: {} | at query end-1: {} | at resource end-1: {}",
                    ob + on, if end as usize > ob + on { "OVERRUNS" } else { "fits in" }, (end as usize).abs_diff(ob + on), crate::cpu_watch::region(va as usize), crate::cpu_watch::region(b - 1), crate::cpu_watch::region(end as usize - 1))
            });
        }
    }
    for (a, b) in dirty {
        up.ww.dirty_pages += ((b - a) / crate::cpu_watch::PAGE) as u64;
        for idx in overlapping(&up.index, a, b) {
            let t = &mut up.tracked[idx];
            if !t.ww {
                continue;
            }
            for sub in 0..t.hashes.len() {
                let Some((lo, len)) = sub_range(t, sub) else { continue };
                if lo >= b || lo + len <= a {
                    continue;
                }
                let d = &mut t.cpu_dirty[sub];
                d.push((a.max(lo), b.min(lo + len)));
                if d.len() > 64 {
                    let (x, y) = (d.iter().map(|r| r.0).min().unwrap(), d.iter().map(|r| r.1).max().unwrap());
                    d.clear();
                    d.push((x, y));
                }
                t.zero[sub] = false;
                up.ww.dirty_subs += 1;
            }
        }
    }
}

static UP_TOTALS: [core::sync::atomic::AtomicU64; 4] = [const { core::sync::atomic::AtomicU64::new(0) }; 4];
static UP_QUEUES: Mutex<Vec<(usize, &'static str)>> = Mutex::new(Vec::new());
static TRACKED_BYTES: [core::sync::atomic::AtomicU64; 2] = [const { core::sync::atomic::AtomicU64::new(0) }; 2];

pub fn upload_totals() -> [u64; 4] {
    core::array::from_fn(|i| UP_TOTALS[i].load(core::sync::atomic::Ordering::Relaxed))
}

pub fn tracked_bytes() -> (u64, u64) {
    (TRACKED_BYTES[0].load(core::sync::atomic::Ordering::Relaxed), TRACKED_BYTES[1].load(core::sync::atomic::Ordering::Relaxed))
}

fn publish_tracked(up: &Uploader) {
    TRACKED_BYTES[0].store(up.tracked.len() as u64, core::sync::atomic::Ordering::Relaxed);
    TRACKED_BYTES[1].store(up.tracked.iter().map(|t| t.layout.total).sum::<u64>(), core::sync::atomic::Ordering::Relaxed);
}

unsafe fn queue_type(queue: *mut c_void) -> u32 {
    let f: unsafe extern "system" fn(*mut c_void, *mut [u32; 4]) -> *mut [u32; 4] = core::mem::transmute(*vt(queue).add(18));
    let mut d = [u32::MAX; 4];
    f(queue, &mut d);
    d[0]
}

unsafe fn note_upload_queue(queue: *mut c_void, path: &'static str) {
    let mut g = UP_QUEUES.lock().unwrap_or_else(|e| e.into_inner());
    if g.iter().any(|&(q, p)| q == queue as usize && p == path) || g.len() >= 16 {
        return;
    }
    g.push((queue as usize, path));
    let ty = queue_type(queue);
    let name = match ty { 0 => "DIRECT", 1 => "BUNDLE", 2 => "COMPUTE", 3 => "COPY", _ => "?" };
    record("d3d12_xs", "upload", &format!("upload command lists execute on queue {queue:p} type {ty} ({name}) via {path}"));
}

fn note_upload_list(bytes: u64, copies: u64) {
    UP_TOTALS[0].fetch_add(bytes, core::sync::atomic::Ordering::Relaxed);
    UP_TOTALS[1].fetch_add(copies, core::sync::atomic::Ordering::Relaxed);
    UP_TOTALS[2].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    crate::frame_ev::note_upload(bytes, copies);
}

unsafe fn record_uploads(alloc: *mut c_void, list: *mut c_void, staged: &[Staged]) {
    const COPY_DEST: u32 = 0x400;
    let mut copies = 0u64;
    vcall!(alloc, 8, fn() -> i32);
    vcall!(list, 10, fn(*mut c_void, *mut c_void) -> i32, alloc, core::ptr::null_mut());
    crate::gpuprof::own_begin(list, crate::gpuprof::TAG_UPLOAD);
    for s in staged {
        if s.state != COPY_DEST {
            let b = [barrier(s.res, s.state, COPY_DEST)];
            vcall!(list, 26, fn(u32, *const [u64; 4]) -> (), 1, b.as_ptr());
        }
        for (sub, fp, boxes) in &s.subs {
            // D3D12_TEXTURE_COPY_LOCATION { pResource; Type; union { PlacedFootprint | SubresourceIndex } }
            let mut dst = [0u8; 48];
            dst[0..8].copy_from_slice(&(s.res as u64).to_le_bytes());
            dst[16..20].copy_from_slice(&sub.to_le_bytes()); // Type 0 = SUBRESOURCE_INDEX
            let mut src = [0u8; 48];
            src[0..8].copy_from_slice(&(s.buf as u64).to_le_bytes());
            src[8..12].copy_from_slice(&1u32.to_le_bytes()); // Type 1 = PLACED_FOOTPRINT
            src[16..48].copy_from_slice(&fp.raw);
            match boxes {
                Some(bx) => {
                    for b in bx {
                        vcall!(list, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (),
                            dst.as_ptr(), 0, b[1], 0, src.as_ptr(), b.as_ptr() as *const c_void);
                        copies += 1;
                    }
                }
                None => {
                    vcall!(list, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (),
                        dst.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
                    copies += 1;
                }
            }
        }
        if s.state != COPY_DEST {
            let b = [barrier(s.res, COPY_DEST, s.state)];
            vcall!(list, 26, fn(u32, *const [u64; 4]) -> (), 1, b.as_ptr());
        }
    }
    crate::gpuprof::own_close(list);
    note_upload_list(staged.iter().map(|s| s.size).sum(), copies);
}

unsafe fn adopt_backlog(up: &mut Uploader, budget_us: u128) {
    up.backlog.extend(crate::placed::drain_new());
    let budget = std::time::Instant::now();
    while budget.elapsed().as_micros() < budget_us {
        let Some((res, p)) = up.backlog.pop_front() else { break };
        match adopt(up, res as *mut c_void, p.clone()) {
            Ok(t) => {
                up.tracked.push(t);
                up.index_stale = true;
            }
            Err(e) => {
                up.rejected += 1;
                if up.rejected <= 12 {
                    record("d3d12_xs", "upload", &format!(
                        "NOT mirrored: va={:#x} {}x{}x{} mips={} fmt={} layout={} flags={:#x}: {e}",
                        p.va, p.width(), p.height(), p.depth_or_array(), p.mips(), p.format(), p.layout(), p.flags()));
                }
                crate::placed::forget(res as *mut c_void);
                vcall!(res as *mut c_void, 2, fn() -> u32);
            }
        }
    }
}

unsafe fn ring_slot(up: &mut Uploader) -> Option<usize> {
    let done = vcall!(up.fence, 8, fn() -> u64);
    if let Some(i) = up.ring.iter().position(|r| r.2 <= done) {
        return Some(i);
    }
    if up.ring.len() < SUBMIT_RING_MAX {
        let mut alloc: *mut c_void = core::ptr::null_mut();
        if vcall!(up.device, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut alloc) < 0 || alloc.is_null() {
            return None;
        }
        let mut list: *mut c_void = core::ptr::null_mut();
        if vcall!(up.device, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, alloc, core::ptr::null_mut(), &IID_GFX_LIST, &mut list) < 0 || list.is_null() {
            vcall!(alloc, 2, fn() -> u32);
            return None;
        }
        vcall!(list, 9, fn() -> i32);
        up.ring.push((alloc, list, 0));
        return Some(up.ring.len() - 1);
    }
    let (i, v) = up.ring.iter().enumerate().map(|(i, r)| (i, r.2)).min_by_key(|r| r.1)?;
    up.sb.ring_waits += 1;
    let t = std::time::Instant::now();
    while vcall!(up.fence, 8, fn() -> u64) < v {
        if t.elapsed() > std::time::Duration::from_secs(2) {
            return None;
        }
        std::thread::yield_now();
    }
    Some(i)
}

pub unsafe fn submit_hook(queue: *mut c_void) {
    if !submit_hook_enabled() {
        return;
    }
    let Some(_g) = UploadGuard::enter() else { return };
    let t0 = std::time::Instant::now();
    let mut guard = match UP.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    let Some(up) = guard.as_mut() else { return };
    if crate::dred::removed() {
        return;
    }
    up.sb.hooks += 1;
    adopt_backlog(up, ADOPT_BUDGET_US / 4);
    let notes = take_notes();
    apply_notes(up, &notes);
    ww_sync(up);
    let dirty: Vec<usize> = (0..up.tracked.len()).filter(|&i| up.tracked[i].ww && up.tracked[i].cpu_dirty.iter().any(|d| !d.is_empty())).collect();
    if dirty.is_empty() {
        up.sb.fast += 1;
        let us = t0.elapsed().as_micros() as u64;
        up.sb.us_total += us;
        up.sb.us_max = up.sb.us_max.max(us);
        return;
    }
    let mut staged: Vec<Staged> = Vec::new();
    let mut hashed = 0u64;
    for idx in dirty {
        if let Some(s) = check(up, idx, &mut hashed, true) {
            staged.push(s);
        }
    }
    if !staged.is_empty() {
        match ring_slot(up) {
            Some(i) => {
                let (alloc, list, _) = up.ring[i];
                record_uploads(alloc, list, &staged);
                let hr = vcall!(list, 9, fn() -> i32);
                if hr < 0 {
                    record("d3d12_xs", "upload", &format!("submit-time upload list Close FAILED hr={hr:#010x}"));
                } else {
                    let lists = [list];
                    note_upload_queue(up.queue, "the ExecuteCommandLists submit hook");
                    vcall!(up.queue, 10, fn(u32, *const *mut c_void) -> (), 1, lists.as_ptr());
                }
                up.fence_value += 1;
                vcall!(up.queue, 14, fn(*mut c_void, u64) -> i32, up.fence, up.fence_value);
                up.ring[i].2 = up.fence_value;
                if queue != up.queue {
                    up.sb.cross_queue += 1;
                    UP_TOTALS[3].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    let (uq, f, v, n) = (up.queue, up.fence, up.fence_value, up.sb.cross_queue);
                    if n <= 8 || n.is_power_of_two() {
                        record("d3d12_xs", "upload", &format!("cross-queue Wait #{n}: title queue {queue:p} type {} waits on upload fence {f:p} value {v} signalled by upload queue {uq:p} type {}", queue_type(queue), queue_type(uq)));
                    }
                    vcall!(queue, 15, fn(*mut c_void, u64) -> i32, up.fence, up.fence_value);
                }
                up.sb.uploads += 1;
                up.sb.subs += staged.iter().map(|s| s.subs.len() as u64).sum::<u64>();
                for s in &staged {
                    up.retire.push((up.fence_value, s.buf as usize, s.size));
                }
            }
            None => {
                up.ww.once(0, 5, || "submit-time upload ring exhausted: the GPU did not finish an earlier upload within 2 s, so these dirty subresources are re-queued for the next submit or PresentX".to_string());
                for s in staged {
                    if let Some(t) = up.tracked.iter_mut().find(|t| t.res == s.res) {
                        let whole = (t.p.va as usize, t.p.va as usize + t.layout.total as usize);
                        for (sub, _, _) in &s.subs {
                            t.hashes[*sub as usize] = None;
                            t.cpu_dirty[*sub as usize].push(whole);
                        }
                    }
                    vcall!(s.buf, 2, fn() -> u32);
                }
            }
        }
    }
    let us = t0.elapsed().as_micros() as u64;
    up.sb.us_total += us;
    up.sb.us_max = up.sb.us_max.max(us);
}

unsafe fn init(queue: *mut c_void) -> Result<Uploader, String> {
    let mut device: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(queue, 7, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE, &mut device);
    if hr < 0 || device.is_null() {
        return Err(format!("queue->GetDevice hr={hr:#010x}"));
    }
    crate::vram_log::start(device);
    let mut alloc: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(device, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut alloc);
    if hr < 0 {
        return Err(format!("CreateCommandAllocator hr={hr:#010x}"));
    }
    let mut list: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(device, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32,
        0, 0, alloc, core::ptr::null_mut(), &IID_GFX_LIST, &mut list);
    if hr < 0 {
        return Err(format!("CreateCommandList hr={hr:#010x}"));
    }
    vcall!(list, 9, fn() -> i32);
    let mut fence: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(device, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut fence);
    if hr < 0 {
        return Err(format!("CreateFence hr={hr:#010x}"));
    }
    crate::gpuprof_q::name_fence(fence, "upload");
    let fence_addr = fence as usize;
    let reaper = crate::reaper::Reaper::new(
        move || unsafe { vcall!(fence_addr as *mut c_void, 8, fn() -> u64) },
        |res| unsafe {
            vcall!(res as *mut c_void, 2, fn() -> u32);
        },
    );
    Ok(Uploader {
        reaper,
        device, alloc, list, fence, fence_value: 0, tracked: Vec::new(), retire: Vec::new(), pool: Vec::new(), pool_bytes: 0, backlog: std::collections::VecDeque::new(), release_cursor: 0, cursor: 0, partial_cursor: 0,
        pumps: 0, uploaded_subs: 0, uploaded_bytes: 0, unreadable: 0, rejected: 0, recommitted: 0,
        zero_cursor: 0, zero_filled: 0, watch_changed: 0,
        index: Vec::new(), index_stale: true, ev: EventStats::default(), cost_sum_us: 0, cost_max_us: 0, cost_n: 0,
        ww: WwStats::default(),
        queue,
        main_value: 0,
        ring: Vec::new(),
        sb: SubmitStats::default(),
    })
}

/// Why a placed resource is not mirrored (None = it is).
fn unsupported_reason(p: &Placed) -> Option<String> {
    if p.dim() != 3 {
        return Some(format!("dimension {} (only Texture2D/array/cube are mirrored)", p.dim()));
    }
    if p.flags() & 0x3 != 0 {
        return Some("render-target/depth-stencil texture (GPU-written, nothing to mirror)".into());
    }
    None
}

unsafe fn adopt(up: &mut Uploader, res: *mut c_void, p: Placed) -> Result<Tracked, String> {
    if let Some(r) = unsupported_reason(&p) {
        return Err(r);
    }
    let layout = layout_for(&p.desc)?;
    if layout.planes != 1 {
        return Err(format!("{} planes", layout.planes));
    }
    if layout.bpe == 0 || layout.total == 0 || layout.total > u32::MAX as u64 {
        return Err(format!("layout size {:#x} bpe {}", layout.total, layout.bpe));
    }
    let nsubs = p.mips() as u32 * p.depth_or_array() as u32;
    if layout.mips != p.mips() as u32 || nsubs == 0 || nsubs > 4096 {
        return Err(format!("mip mismatch xg={} desc={}", layout.mips, p.mips()));
    }
    let mut pc_desc = [0u8; 0x38];
    vcall!(res, 10, fn(*mut u8) -> *mut u8, pc_desc.as_mut_ptr());
    let mut raw = vec![[0u8; 32]; nsubs as usize];
    let mut rows = vec![0u32; nsubs as usize];
    let mut row_sizes = vec![0u64; nsubs as usize];
    let mut total = 0u64;
    vcall!(up.device, 38, fn(*const u8, u32, u32, u64, *mut [u8; 32], *mut u32, *mut u64, *mut u64) -> (),
        pc_desc.as_ptr(), 0, nsubs, 0, raw.as_mut_ptr(), rows.as_mut_ptr(), row_sizes.as_mut_ptr(), &mut total);
    if total == 0 || total == u64::MAX {
        return Err("GetCopyableFootprints failed".into());
    }
    let fps: Vec<Footprint> = (0..nsubs as usize).map(|i| Footprint { raw: raw[i], rows: rows[i], row_size: row_sizes[i] }).collect();
    if fps.iter().any(|f| f.row_size % layout.bpe as u64 != 0) {
        return Err(format!("row size not a multiple of bytes-per-element {}", layout.bpe));
    }
    if p.layout() == 264 {
        let f0 = fps[0];
        let (va, xt, ft, d) = (p.va as usize, layout.total as usize, total as usize, res_desc(&p));
        let align = u64::from_le_bytes(p.desc[8..16].try_into().unwrap());
        let h0 = u32::from_le_bytes(f0.raw[16..20].try_into().unwrap());
        let w0 = u32::from_le_bytes(f0.raw[12..16].try_into().unwrap());
        up.ww.once(res as usize, 7, || {
            let (o, ob, on) = crate::cpu_watch::owner(va);
            let need = va + xt;
            let end = if on > 0 { ob + on } else { 0 };
            let fit = if end == 0 { "no xmem region".to_string() } else if need > end { format!("OVERRUNS the xmem region end {end:#x} by {:#x}", need - end) } else { format!("fits in the xmem region (end {end:#x}, {:#x} spare)", end - need) };
            format!("linear (layout 264) placed resource res={:#x} va={va:#x} {d} desc-align={align:#x} | XG layout.total={xt:#x} | PC footprint sub0: {w0}x{h0} RowPitch={} NumRows={} RowSizeInBytes={} TotalBytes(all subs)={ft:#x} | va+XG total={need:#x} {fit} (owner={o} region={ob:#x}+{on:#x}) | at va: {} | at va+XG total-1: {}",
                res as usize, f0.row_pitch(), f0.rows, f0.row_size, crate::cpu_watch::region(va), crate::cpu_watch::region(need - 1))
        });
    }
    let class = crate::cpu_watch::classify(p.va as usize, layout.total as usize);
    let e = up.ww.classes.entry(class.why()).or_insert((0, 0));
    e.0 += 1;
    e.1 += layout.total;
    let ww = class == crate::cpu_watch::Class::Watched;
    if !ww {
        let (va, total, why) = (p.va, layout.total, class.why());
        let d = res_desc(&p);
        up.ww.once(res as usize, 0, || format!("placed resource res={:#x} va={va:#x}+{total:#x} {d} keeps the hash fallback (exactly the TOMBOX_WRITE_WATCH=0 treatment): {why} | {}", res as usize, crate::cpu_watch::region(va as usize)));
    }
    let ww_ranges = if ww { vec![(p.va as usize, p.va as usize + layout.total as usize, crate::cpu_watch::owner(p.va as usize).1)] } else { Vec::new() };
    if ww {
        let (va, total, d) = (p.va, layout.total, res_desc(&p));
        up.ww.once(res as usize, 6, || format!("placed resource res={:#x} va={va:#x}+{total:#x} {d} is write-watched", res as usize));
    }
    Ok(Tracked { res: res as usize, p, layout, fps, total_bytes: total, hashes: vec![None; nsubs as usize], partial: vec![0; nsubs as usize], zero: vec![false; nsubs as usize], watch: vec![0; nsubs as usize], pend: vec![up.pumps + PENDING_PUMPS; nsubs as usize], hot: up.pumps + PENDING_PUMPS, ww, ww_ranges, cpu_dirty: vec![Vec::new(); nsubs as usize], sent: vec![false; nsubs as usize], freed: false, rcache: None })
}

struct Staged {
    res: usize,
    state: u32,
    buf: *mut c_void,
    size: u64,
    subs: Vec<(u32, Footprint, Option<Vec<[u32; 6]>>)>,
}

/// Check one tracked texture; stage an upload for every changed, readable subresource.
/// `fresh_only`: look only at subresources that have never been uploaded.
unsafe fn check(up: &mut Uploader, idx: usize, hashed: &mut u64, fresh_only: bool) -> Option<Staged> {
    if crate::dred::removed() {
        return None;
    }
    let device = up.device;
    let now = up.pumps;
    let t = &mut up.tracked[idx];
    let bpe = t.layout.bpe as usize;
    let mut changed: Vec<(u32, Arc<SubMap>, u64, Option<Arc<Vec<u8>>>, Option<Vec<[u32; 6]>>)> = Vec::new();
    let mut unreadable = 0;
    let want: Vec<usize> = (0..t.fps.len()).filter(|&s| fresh_only != t.hashes[s].is_some() || (fresh_only && !t.cpu_dirty[s].is_empty())).collect();
    if want.is_empty() {
        return None;
    }
    let gpu_owned = crate::placed::gpu_written(t.res);
    let ck = std::time::Instant::now();
    let whole = scan_subs(t, &want);
    stage_time(0, ck);
    let Some(whole) = whole else { return None };
    for sub in want.into_iter().map(|s| s as u32) {
        let fp = t.fps[sub as usize];
        let (w_e, h_e) = ((fp.row_size / bpe as u64) as u32, fp.rows);
        let ck = std::time::Instant::now();
        let map = t.layout.submap(sub, w_e, h_e);
        stage_time(1, ck);
        let Some(map) = map else {
            continue; // layout query failed for this subresource (counted at adoption time)
        };
        let base = t.p.va as usize;
        let (lo, len) = (base + map.lo, map.hi - map.lo);
        let sc = whole.clip(lo, len);
        let mut snap: Option<Arc<Vec<u8>>> = None;
        if let Some((bp, st, pr, ty, rb, rs)) = sc.bad {
            let good: usize = sc.ok.iter().map(|(a, b)| b - a).sum();
            static LOGGED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            let mark = good + 1;
            if t.partial[sub as usize] != mark && LOGGED.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 400 {
                record("d3d12_xs", "upload", &format!(
                    "unreadable placed texture res={:#x} va={:#x} {}x{}x{} mips={} fmt={} layout={} sub={sub} range={lo:#x}+{len:#x} readable={good:#x} first-bad={bp:#x} state={st:#x} protect={pr:#x} type={ty:#x} region={rb:#x}+{rs:#x} -> {}",
                    t.res, t.p.va, t.p.width(), t.p.height(), t.p.depth_or_array(), t.p.mips(), t.p.format(), t.p.layout(),
                    if sparse_zero_enabled() { "unmapped pages read as zero, re-checked every present" } else { "skipped" }));
            }
            if !sparse_zero_enabled() {
                t.partial[sub as usize] = mark;
                unreadable += 1;
                if !t.cpu_dirty[sub as usize].is_empty() {
                    t.cpu_dirty[sub as usize].clear();
                    up.ww.lost += 1;
                    let (res, va, d) = (t.res, t.p.va, res_desc(&t.p));
                    let before = up.ww.logged.len();
                    up.ww.once(res, 2, || format!("CPU-written placed resource res={res:#x} va={va:#x} {d} sub={sub} NOT uploaded: part of its memory is unreadable and TOMBOX_SPARSE_ZERO is off"));
                    if up.ww.logged.len() > before {
                        up.ww.lost_res += 1;
                    }
                }
                continue;
            }
            let mut v = vec![0u8; len];
            for (a, b) in &sc.ok {
                let dst = v.as_mut_ptr().add(a - lo) as usize;
                let (src, n) = (*a, b - a);
                let _ = copy_title(src, dst, n);
            }
            snap = Some(Arc::new(v));
            t.partial[sub as usize] = mark;
        } else {
            t.partial[sub as usize] = 0;
        }
        let mem: &[u8] = match &snap {
            Some(v) => v.as_slice(),
            None => core::slice::from_raw_parts(lo as *const u8, len),
        };
        let ck = std::time::Instant::now();
        let dirty = !t.cpu_dirty[sub as usize].is_empty();
        let need_zero_test = t.hashes[sub as usize].is_none() && !gpu_owned && !dirty;
        let hashed_now = hash_title(mem.as_ptr() as usize, mem.len(), need_zero_test);
        let Some((h, all_zero)) = hashed_now else {
            unreadable += 1;
            continue;
        };
        stage_time(2, ck);
        *hashed += mem.len() as u64;
        if t.hashes[sub as usize].is_none() && (gpu_owned || all_zero) && !dirty {
            // Never written by the title (fresh committed pages are zero). Do not upload:
            // the title may instead fill this subresource with a GPU copy
            // (CopyTextureRegion), which an upload of zeros could overwrite.
            t.hashes[sub as usize] = Some(h);
            t.zero[sub as usize] = !gpu_owned;
            continue;
        }
        if dirty && t.hashes[sub as usize] == Some(h) {
            t.cpu_dirty[sub as usize].clear();
            up.ww.unchanged += 1;
            continue;
        }
        t.zero[sub as usize] = false;
        if dirty && gpu_owned {
            up.ww.gpu_owned_writes += 1;
            let (res, va, d) = (t.res, t.p.va, res_desc(&t.p));
            up.ww.once(res, 1, || format!("placed resource res={res:#x} va={va:#x} {d} is a GPU copy destination and the CPU also wrote it; only the CPU-written row bands are uploaded, GPU-written texels elsewhere are kept"));
        }
        let mut boxes = None;
        if t.ww && dirty && t.sent[sub as usize] {
            let fh = u32::from_le_bytes(fp.raw[16..20].try_into().unwrap());
            let fw = u32::from_le_bytes(fp.raw[12..16].try_into().unwrap());
            let fd = u32::from_le_bytes(fp.raw[20..24].try_into().unwrap()).max(1);
            if fp.rows > 0 && fh % fp.rows == 0 {
                let bh = fh / fp.rows;
                let bands = crate::cpu_watch::dirty_bands(map.row_extents(bpe), t.p.va as usize, &t.cpu_dirty[sub as usize], 8);
                if bands.is_empty() {
                    t.cpu_dirty[sub as usize].clear();
                    t.hashes[sub as usize] = Some(h);
                    continue;
                }
                boxes = Some(bands.iter().map(|&(y0, y1)| [0, y0 as u32 * bh, 0, fw, (y1 as u32 * bh).min(fh), fd]).collect::<Vec<[u32; 6]>>());
            }
        }
        if dirty || t.hashes[sub as usize] != Some(h) {
            changed.push((sub, map, h, snap, boxes));
        }
    }
    up.unreadable += unreadable;
    if changed.is_empty() {
        return None;
    }
    // One upload buffer for this texture's changed subresources.
    let heap: [u32; 5] = [2 /* UPLOAD */, 0, 0, 1, 1];
    let mut bd = [0u8; 0x38];
    bd[0..4].copy_from_slice(&1u32.to_le_bytes());
    let size = pool_bucket(t.total_bytes);
    bd[0x10..0x18].copy_from_slice(&size.to_le_bytes());
    bd[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    bd[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    bd[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    bd[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    bd[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
    let mut buf: *mut c_void = core::ptr::null_mut();
    let ck = std::time::Instant::now();
    STAGE_US[5].fetch_add(t.total_bytes, core::sync::atomic::Ordering::Relaxed);
    let hr = match pool_take(&mut up.pool, &mut up.pool_bytes, size) {
        Some(b) => {
            buf = b as *mut c_void;
            0
        }
        None => vcall!(device, 27, fn(*const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32,
            heap.as_ptr(), 0, bd.as_ptr(), 0x2C3, core::ptr::null(), &IID_RESOURCE, &mut buf),
    };
    if hr < 0 || buf.is_null() {
        record("d3d12_xs", "upload", &format!("upload buffer ({} bytes) FAILED hr={hr:#010x}", t.total_bytes));
        return None;
    }
    let mut data: *mut u8 = core::ptr::null_mut();
    let hr = vcall!(buf, 8, fn(u32, *const c_void, *mut *mut u8) -> i32, 0, core::ptr::null(), &mut data);
    if hr < 0 || data.is_null() {
        vcall!(buf, 2, fn() -> u32);
        record("d3d12_xs", "upload", &format!("upload buffer Map FAILED hr={hr:#010x}"));
        return None;
    }
    stage_time(3, ck);
    let ck = std::time::Instant::now();
    let base = t.p.va as *const u8;
    let mut subs = Vec::with_capacity(changed.len());
    for (sub, map, h, snap, boxes) in changed {
        let fp = t.fps[sub as usize];
        let dst0 = data.add(fp.offset() as usize);
        let pitch = fp.row_pitch() as usize;
        let src0 = match &snap { Some(v) => v.as_ptr().wrapping_sub(map.lo), None => base };
        let (title_lo, title_len) = ((src0 as usize).wrapping_add(map.lo), map.hi - map.lo);
        let bh = if fp.rows == 0 { 1 } else { (u32::from_le_bytes(fp.raw[16..20].try_into().unwrap()) / fp.rows).max(1) };
        let ok = match &boxes {
            Some(bx) => bx.iter().all(|b| {
                let (y0, y1) = ((b[1] / bh) as usize, (b[4].div_ceil(bh) as usize).min(map.h_e as usize));
                detile_guarded(title_lo, title_len, &DetileJob { dst0: dst0 as usize, pitch, src0: src0 as usize, offs: map.offs.as_ptr(), offs_len: map.offs.len(), w_e: map.w_e as usize, y0, y1, bpe })
            }),
            None => detile_rows(dst0, pitch, src0, title_lo, title_len, &map.offs, map.w_e as usize, map.h_e as usize, bpe),
        };
        let was_dirty = !t.cpu_dirty[sub as usize].is_empty();
        if !ok {
            up.unreadable += 1;
            if was_dirty {
                t.cpu_dirty[sub as usize].clear();
                up.ww.lost += 1;
                let (res, va, d) = (t.res, t.p.va, res_desc(&t.p));
                let before = up.ww.logged.len();
                up.ww.once(res, 2, || format!("CPU-written placed resource res={res:#x} va={va:#x} {d} sub={sub} NOT uploaded: its memory faulted while being read"));
                if up.ww.logged.len() > before {
                    up.ww.lost_res += 1;
                }
            }
            continue;
        }
        t.hashes[sub as usize] = Some(h);
        if t.ww {
            t.watch[sub as usize] = 0;
        } else {
            t.watch[sub as usize] = now + WATCH_PUMPS;
            t.hot = t.hot.max(now + WATCH_PUMPS);
        }
        if !t.sent.iter().any(|&x| x) {
            crate::firstuse::note_bytes(t.res, crate::firstuse::UPLOAD, t.layout.total);
            if t.p.layout() != 264 {
                crate::firstuse::note_bytes(t.res, crate::firstuse::DETILE, t.layout.total);
            }
        }
        t.sent[sub as usize] = true;
        t.cpu_dirty[sub as usize].clear();
        up.uploaded_subs += 1;
        let bytes = match &boxes {
            Some(bx) => bx.iter().map(|b| ((b[4] - b[1]).div_ceil(bh)) as u64 * map.w_e as u64 * bpe as u64).sum::<u64>(),
            None => (map.w_e as u64) * (map.h_e as u64) * bpe as u64,
        };
        up.uploaded_bytes += bytes;
        if was_dirty {
            if boxes.is_some() {
                up.ww.band_uploads += 1;
                up.ww.band_bytes += bytes;
            } else {
                up.ww.whole_uploads += 1;
            }
        }
        subs.push((sub, fp, boxes));
    }
    vcall!(buf, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
    stage_time(4, ck);
    Some(Staged { res: t.res, state: t.p.init_state, buf, size, subs })
}

static STAGE_US: [core::sync::atomic::AtomicU64; 6] = [const { core::sync::atomic::AtomicU64::new(0) }; 6];

fn stage_time(i: usize, since: std::time::Instant) {
    STAGE_US[i].fetch_add(since.elapsed().as_micros() as u64, core::sync::atomic::Ordering::Relaxed);
}

fn take_stage_times() -> [u64; 6] {
    core::array::from_fn(|i| STAGE_US[i].swap(0, core::sync::atomic::Ordering::Relaxed))
}

const PARALLEL_DETILE_BYTES: usize = 16 << 20;
const ADOPT_BUDGET_US: u128 = 3_000;
const RETIRE_BUDGET_US: u128 = 2_000;
const RELEASE_BUDGET_US: u128 = 3_000;
const POOL_CAP_BYTES: u64 = 256 << 20;
const POOL_MIN_BUCKET: u64 = 64 << 10;

fn pool_bucket(bytes: u64) -> u64 {
    bytes.max(POOL_MIN_BUCKET).next_power_of_two()
}

fn pool_take(pool: &mut Vec<(u64, usize)>, pool_bytes: &mut u64, size: u64) -> Option<usize> {
    let i = pool.iter().rposition(|&(s, _)| s == size)?;
    let (s, b) = pool.swap_remove(i);
    *pool_bytes -= s;
    Some(b)
}

fn pool_give(pool: &mut Vec<(u64, usize)>, pool_bytes: &mut u64, size: u64, buf: usize, cap: u64) -> bool {
    if *pool_bytes + size > cap {
        return false;
    }
    pool.push((size, buf));
    *pool_bytes += size;
    true
}

unsafe fn detile_span(dst0: usize, pitch: usize, src0: usize, offs: &[u32], w_e: usize, rows: core::ops::Range<usize>, bpe: usize) {
    for y in rows {
        let row = (dst0 + y * pitch) as *mut u8;
        for (x, &o) in offs[y * w_e..(y + 1) * w_e].iter().enumerate() {
            core::ptr::copy_nonoverlapping((src0 as *const u8).wrapping_add(o as usize), row.add(x * bpe), bpe);
        }
    }
}

#[track_caller]
fn detile_guarded(title_lo: usize, title_len: usize, job: &DetileJob) -> bool {
    crate::guard::guarded(title_lo, title_len, detile_job, job as *const DetileJob as *mut c_void)
}

unsafe fn detile_rows(dst0: *mut u8, pitch: usize, src0: *const u8, title_lo: usize, title_len: usize, offs: &[u32], w_e: usize, h_e: usize, bpe: usize) -> bool {
    let job = |y0: usize, y1: usize| DetileJob { dst0: dst0 as usize, pitch, src0: src0 as usize, offs: offs.as_ptr(), offs_len: offs.len(), w_e, y0, y1, bpe };
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    if w_e * h_e * bpe < PARALLEL_DETILE_BYTES || workers == 1 || h_e < 2 {
        return detile_guarded(title_lo, title_len, &job(0, h_e));
    }
    let chunk = h_e.div_ceil(workers);
    let faulted = core::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        for k in 1..workers {
            let (y0, y1) = ((k * chunk).min(h_e), ((k + 1) * chunk).min(h_e));
            if y0 < y1 {
                let (faulted, job) = (&faulted, job(y0, y1));
                let job = SendJob(job);
                sc.spawn(move || {
                    let job = job;
                    if !detile_guarded(title_lo, title_len, &job.0) {
                        faulted.store(true, core::sync::atomic::Ordering::Relaxed);
                    }
                });
            }
        }
        if !detile_guarded(title_lo, title_len, &job(0, chunk.min(h_e))) {
            faulted.store(true, core::sync::atomic::Ordering::Relaxed);
        }
    });
    !faulted.load(core::sync::atomic::Ordering::Relaxed)
}

struct SendJob(DetileJob);

unsafe impl Send for SendJob {}

fn barrier(res: usize, before: u32, after: u32) -> [u64; 4] {
    [0, res as u64, 0xffff_ffffu64 | ((before as u64) << 32), after as u64]
}

fn first_nonzero(ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(a, b)| nonzero_title(a, b - a).unwrap_or(true))
}

fn watch_pending(up: &mut Uploader) -> (u64, u64) {
    let nt = up.tracked.len();
    let now = up.pumps;
    let poll_all = poll_mode();
    let budget = if poll_all { ZERO_SCAN_BUDGET } else { EVENT_SCAN_BUDGET };
    let (mut scanned, mut filled, mut changed, mut k) = (0u64, 0u64, 0u64, 0usize);
    let mut found: Vec<(usize, String, &'static str)> = Vec::new();
    while k < nt && scanned < budget {
        let idx = (up.zero_cursor + k) % nt;
        k += 1;
        let t = &mut up.tracked[idx];
        if t.ww || t.freed {
            continue;
        }
        let poll = polls(t, poll_all);
        if !poll && t.hot <= now {
            continue;
        }
        let subs: Vec<usize> = (0..t.hashes.len()).filter(|&s| t.hashes[s].is_some() && ((t.zero[s] && (poll || t.pend[s] > now)) || t.watch[s] > now)).collect();
        if subs.is_empty() {
            continue;
        }
        let Some(whole) = scan_subs(t, &subs) else { continue };
        for sub in subs {
            let Some((a, n)) = sub_range(t, sub) else { continue };
            let sc = whole.clip(a, n);
            if t.zero[sub] {
                scanned += sc.ok.iter().map(|(a, b)| (b - a) as u64).sum::<u64>();
                if first_nonzero(&sc.ok) {
                    t.zero[sub] = false;
                    t.hashes[sub] = None;
                    filled += 1;
                    if crate::cpu_watch::enabled() {
                        found.push((t.res, format!("va={:#x} {} sub={sub}", t.p.va, res_desc(&t.p)), "zero->data"));
                    }
                }
            } else if sc.bad.is_none() {
                scanned += n as u64;
                let Some((h, _)) = hash_title(a, n, false) else { continue };
                if t.hashes[sub] != Some(h) {
                    t.hashes[sub] = None;
                    changed += 1;
                    if crate::cpu_watch::enabled() {
                        found.push((t.res, format!("va={:#x} {} sub={sub}", t.p.va, res_desc(&t.p)), "re-written"));
                    }
                }
            }
        }
    }
    up.zero_cursor = if nt == 0 { 0 } else { (up.zero_cursor + k) % nt };
    for (res, d, what) in found {
        up.ww.once(res, 4, || format!("CPU write found only by polling ({what}) in not-write-watched placed resource res={res:#x} {d}"));
    }
    (filled, changed)
}

struct PumpPhases {
    t0: std::time::Instant,
    last: std::time::Instant,
    parts: Vec<(&'static str, u64)>,
}

impl PumpPhases {
    fn start() -> Self {
        let now = std::time::Instant::now();
        PumpPhases { t0: now, last: now, parts: Vec::new() }
    }

    fn mark(&mut self, what: &'static str) {
        let now = std::time::Instant::now();
        self.parts.push((what, (now - self.last).as_micros() as u64));
        self.last = now;
    }

    fn report(&self, pump: u64, note: &str, fresh_visits: usize, staged: usize, bytes: u64) {
        static N: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        let total = self.t0.elapsed().as_micros() as u64;
        let st = take_stage_times();
        let rs = take_rs();
        if total < 20_000 || N.fetch_add(1, core::sync::atomic::Ordering::Relaxed) >= 60 {
            return;
        }
        let parts: Vec<String> = self.parts.iter().map(|(w, us)| format!("{w}={:.1}ms", *us as f64 / 1000.0)).collect();
        record("d3d12_xs", "upload", &format!("slow pump {pump}: {:.1} ms on the present thread: {}; fresh visits={fresh_visits} staged={staged} hashed={} MB; inside check: residency-scan={:.1}ms (regions queried={} cache hits={} misses={} invalidated={}, cache {}) submap={:.1}ms hash={:.1}ms upload-buffer-create+map={:.1}ms ({} MB) detile+unmap={:.1}ms{}",
            total as f64 / 1000.0, parts.join(" "), bytes >> 20, st[0] as f64 / 1000.0, rs[RS_QUERIES], rs[RS_HITS], rs[RS_MISSES], rs[RS_DROPS], residency_cache_state(), st[1] as f64 / 1000.0, st[2] as f64 / 1000.0, st[3] as f64 / 1000.0, st[5] >> 20, st[4] as f64 / 1000.0,
            if note.is_empty() { String::new() } else { format!(" ({note})") }));
    }
}

static DIRECT_QUEUES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

pub fn register_direct_queue(queue: *mut c_void) {
    if !queue.is_null() {
        let mut g = DIRECT_QUEUES.lock().unwrap_or_else(|e| e.into_inner());
        let addr = queue as usize;
        if !g.contains(&addr) {
            g.push(addr);
        }
    }
}

pub fn is_direct_queue(queue: *mut c_void) -> bool {
    if queue.is_null() {
        return false;
    }
    DIRECT_QUEUES.lock().unwrap_or_else(|e| e.into_inner()).contains(&(queue as usize))
}

pub fn direct_queue() -> Option<*mut c_void> {
    DIRECT_QUEUES.lock().unwrap_or_else(|e| e.into_inner()).first().copied().map(|q| q as *mut c_void)
}

/// Called once per `PresentX`, on the presenting thread, before the frame is copied out.
pub unsafe fn pump(queue: *mut c_void) {
    let Some(_g) = UploadGuard::enter() else { return };
    let t0 = std::time::Instant::now();
    pump_inner(queue);
    let us = t0.elapsed().as_micros() as u64;
    if let Some(up) = UP.lock().unwrap().as_mut() {
        up.cost_sum_us += us;
        up.cost_n += 1;
        up.cost_max_us = up.cost_max_us.max(us);
    }
}

unsafe fn pump_inner(queue: *mut c_void) {
    static FAILED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if FAILED.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let mut guard = UP.lock().unwrap();
    if guard.is_none() {
        match init(queue) {
            Ok(u) => *guard = Some(u),
            Err(e) => {
                FAILED.store(true, core::sync::atomic::Ordering::Relaxed);
                record("d3d12_xs", "upload", &format!("init FAILED: {e} - placed textures will stay empty"));
                return;
            }
        }
    }
    let up = guard.as_mut().unwrap();
    up.pumps += 1;
    let mut ph = PumpPhases::start();

    // Retire upload buffers whose copy has completed.
    let done = vcall!(up.fence, 8, fn() -> u64);
    let budget = std::time::Instant::now();
    let (pool, pool_bytes, reaper) = (&mut up.pool, &mut up.pool_bytes, &up.reaper);
    up.retire.retain(|&(v, b, size)| {
        if v <= done && budget.elapsed().as_micros() < RETIRE_BUDGET_US {
            if !pool_give(pool, pool_bytes, size, b, POOL_CAP_BYTES) && !reaper.push(b, 0) {
                vcall!(b as *mut c_void, 2, fn() -> u32);
            }
            false
        } else {
            true
        }
    });
    ph.mark("retire");

    adopt_backlog(up, ADOPT_BUDGET_US);
    ph.mark("adopt");

    // Drop textures the title has released (ours is the last reference).
    let budget = std::time::Instant::now();
    let n = up.tracked.len();
    let mut seen = 0;
    let mut freed = 0u32;
    while seen < n && !up.tracked.is_empty() {
        if freed > 0 && budget.elapsed().as_micros() >= RELEASE_BUDGET_US {
            break;
        }
        let i = up.release_cursor % up.tracked.len();
        let r = up.tracked[i].res as *mut c_void;
        seen += 1;
        vcall!(r, 1, fn() -> u32);
        if vcall!(r, 2, fn() -> u32) == 1 {
            crate::placed::forget(r);
            if !up.reaper.push(r as usize, up.fence_value) {
                vcall!(r, 2, fn() -> u32);
            }
            up.tracked.swap_remove(i);
            up.index_stale = true;
            freed += 1;
        } else {
            up.release_cursor = i + 1;
        }
    }

    ph.mark("release");
    let notes = take_notes();
    apply_notes(up, &notes);
    ph.mark("notes");
    ww_sync(up);
    if up.pumps % 120 == 0 {
        for t in up.tracked.iter_mut().filter(|t| t.freed) {
            let (va, total) = (t.p.va as usize, t.layout.total as usize);
            if any_committed(va, total) {
                rebirth(t, &mut up.ww, va, total);
            }
        }
    }
    ph.mark("write-watch");

    // The single upload list/allocator can be reused only once the previous copy is done.
    if done < up.main_value {
        ph.report(up.pumps, "previous upload copy still in flight", 0, 0, 0);
        return;
    }

    const FRESH_BUDGET: u64 = 96 << 20; // bytes hashed per frame for never-uploaded textures
    const RECHECK_BUDGET: u64 = 8 << 20; // bytes hashed per frame re-checking uploaded ones
    const PARTIAL_TEXTURES_PER_FRAME: usize = 64;
    const RECHECK_TEXTURES_PER_FRAME: usize = 256;
    const PUMP_TIME_BUDGET_US: u64 = 4000;
    let poll = poll_mode();
    let fallback_poll = poll;
    let nt = up.tracked.len();
    let mut partial_visits = 0;
    let mut k = 0;
    while fallback_poll && k < nt && partial_visits < PARTIAL_TEXTURES_PER_FRAME {
        let idx = (up.partial_cursor + k) % nt;
        k += 1;
        let t = &mut up.tracked[idx];
        if t.freed || !polls(t, poll) {
            continue;
        }
        let subs: Vec<usize> = (0..t.partial.len()).filter(|&s| t.partial[s] != 0 && t.hashes[s].is_some()).collect();
        if subs.is_empty() {
            continue;
        }
        partial_visits += 1;
        let Some(whole) = scan_subs(t, &subs) else { continue };
        for sub in subs {
            let Some((a, n)) = sub_range(t, sub) else { continue };
            let sc = whole.clip(a, n);
            let good: usize = sc.ok.iter().map(|(a, b)| b - a).sum();
            if sc.bad.is_none() || good + 1 != t.partial[sub] {
                t.hashes[sub] = None;
                up.recommitted += 1;
            }
        }
    }
    if fallback_poll {
        up.partial_cursor = if nt == 0 { 0 } else { (up.partial_cursor + k) % nt };
    }
    ph.mark("partial-rescan");
    let (filled, changed) = watch_pending(up);
    up.zero_filled += filled;
    up.watch_changed += changed;
    ph.mark("zero-watch");
    let mut staged: Vec<Staged> = Vec::new();
    let mut hashed = 0u64;
    let n = up.tracked.len();
    let deadline = std::time::Instant::now() + std::time::Duration::from_micros(PUMP_TIME_BUDGET_US);
    let mut fresh_visits = 0usize;
    static FRESH_CURSOR: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let start = if n == 0 { 0 } else { FRESH_CURSOR.load(core::sync::atomic::Ordering::Relaxed) % n };
    let mut last = start;
    for idx in (start..n).chain(0..start) {
        if hashed >= FRESH_BUDGET || (fresh_visits > 0 && std::time::Instant::now() >= deadline) {
            break;
        }
        last = idx + 1;
        if up.tracked[idx].freed {
            continue;
        }
        if up.tracked[idx].hashes.iter().any(|h| h.is_none()) || (up.tracked[idx].ww && up.tracked[idx].cpu_dirty.iter().any(|d| !d.is_empty())) {
            fresh_visits += 1;
            if let Some(s) = check(up, idx, &mut hashed, true) {
                staged.push(s);
            }
        }
    }
    FRESH_CURSOR.store(if last >= n { 0 } else { last }, core::sync::atomic::Ordering::Relaxed);
    ph.mark("fresh-hash+stage");
    let fresh_staged = staged.len();
    let mut rehashed = 0u64;
    let mut visited = 0;
    while fallback_poll && n > 0 && visited < n.min(RECHECK_TEXTURES_PER_FRAME) && rehashed < RECHECK_BUDGET && (visited == 0 || std::time::Instant::now() < deadline) {
        let idx = up.cursor % n;
        up.cursor = (up.cursor + 1) % n;
        visited += 1;
        if up.tracked[idx].ww || up.tracked[idx].freed {
            continue;
        }
        if let Some(s) = check(up, idx, &mut rehashed, false) {
            staged.push(s);
        }
    }
    ph.mark("recheck");

    if !staged.is_empty() {
        record_uploads(up.alloc, up.list, &staged);
        let hr = vcall!(up.list, 9, fn() -> i32);
        if hr < 0 {
            record("d3d12_xs", "upload", &format!("upload list Close FAILED hr={hr:#010x}"));
        } else {
            let lists = [up.list];
            note_upload_queue(queue, "the PresentX pump");
            vcall!(queue, 10, fn(u32, *const *mut c_void) -> (), 1, lists.as_ptr());
        }
        up.fence_value += 1;
        vcall!(queue, 14, fn(*mut c_void, u64) -> i32, up.fence, up.fence_value);
        up.main_value = up.fence_value;
        for s in &staged {
            up.retire.push((up.fence_value, s.buf as usize, s.size));
        }
    }
    publish_tracked(up);
    ph.mark("record+submit");
    ph.report(up.pumps, "", fresh_visits, fresh_staged, hashed + rehashed);

    if (!staged.is_empty() && up.pumps < 40) || up.pumps % 600 == 0 {
        let (gh, gn) = crate::placed::gpu_write_stats();
        let (mut subs_total, mut zero_pending, mut partial_subs, mut uploaded_now, mut zero_bytes) = (0usize, 0usize, 0usize, 0usize, 0u64);
        for t in &up.tracked {
            for s in 0..t.hashes.len() {
                subs_total += 1;
                if t.zero[s] {
                    zero_pending += 1;
                    zero_bytes += sub_range(t, s).map(|(_, n)| n as u64).unwrap_or(0);
                } else if t.partial[s] != 0 {
                    partial_subs += 1;
                } else if t.hashes[s].is_some() {
                    uploaded_now += 1;
                }
            }
        }
        record("d3d12_xs", "upload", &format!(
            "residency pump {}: subresources={subs_total} with-data={uploaded_now} still-zero={zero_pending} ({} MB) partly-unmapped={partial_subs} | zero->data detected={} re-written after upload={} | residency cache {}: cached resources={} regions queried={} cache hits={} misses={} invalidated={} (totals)",
            up.pumps, zero_bytes >> 20, up.zero_filled, up.watch_changed, residency_cache_state(), up.tracked.iter().filter(|t| t.rcache.is_some()).count(), rs_total()[RS_QUERIES], rs_total()[RS_HITS], rs_total()[RS_MISSES], rs_total()[RS_DROPS]));
        let avg = if up.cost_n == 0 { 0 } else { up.cost_sum_us / up.cost_n };
        let e = &up.ev;
        record("d3d12_xs", "upload", &format!(
            "events pump {} mode={}: io-writes={} ({} MB) -> subresources dirtied={} | commits/maps={} ({} MB) -> subresources dirtied={} | notes outside placed textures={} | pump cost avg={avg}us max={}us over {} presents",
            up.pumps, if poll { "poll" } else { "event" }, e.io_notes, e.io_bytes >> 20, e.io_subs, e.commit_notes, e.commit_bytes >> 20, e.commit_subs, e.unmatched, up.cost_max_us, up.cost_n));
        up.cost_sum_us = 0;
        up.cost_max_us = 0;
        up.cost_n = 0;
        let (mut wn, mut wb, mut un, mut ub) = (0u64, 0u64, 0u64, 0u64);
        for t in &up.tracked {
            if t.ww {
                wn += 1;
                wb += t.layout.total;
            } else {
                un += 1;
                ub += t.layout.total;
            }
        }
        let w = &up.ww;
        let mut why: Vec<String> = w.classes.iter().map(|(k, v)| format!("{k}: {} ({} MB)", v.0, v.1 >> 20)).collect();
        why.sort();
        record("d3d12_xs", "upload", &format!(
            "write-watch pump {} (TOMBOX_WRITE_WATCH={}): tracked watched={wn} ({} MB) hash-fallback={un} ({} MB) | queries={} cost={}us total | dirty pages={} -> subresources={} (bytes unchanged, upload skipped={}) | uploads: row-band={} ({} MB) whole={} | CPU-written but NOT uploaded={} (resources {}) | CPU writes into GPU-copy destinations={} | runtime fallbacks={} backing freed={} (recommitted {}) | submit hook (TOMBOX_WW_SUBMIT={}): calls={} fast-path={} uploads={} subresources={} cross-queue waits={} ring waits={} cpu total={}us avg={}us max={}us | classes at adoption: [{}]",
            up.pumps, if crate::cpu_watch::enabled() { "on" } else { "0" }, wb >> 20, ub >> 20, w.queries, w.query_us, w.dirty_pages, w.dirty_subs, w.unchanged, w.band_uploads, w.band_bytes >> 20, w.whole_uploads, w.lost, w.lost_res, w.gpu_owned_writes, w.fallbacks, w.backing_freed, w.backing_back,
            if submit_hook_enabled() { "on" } else { "0" }, up.sb.hooks, up.sb.fast, up.sb.uploads, up.sb.subs, up.sb.cross_queue, up.sb.ring_waits, up.sb.us_total, up.sb.us_total / up.sb.hooks.max(1), up.sb.us_max,
            why.join("; ")));
        record("d3d12_xs", "upload", &format!(
            "pump {}: staged {} textures; tracked={} total uploaded {} subresources / {} bytes; unreadable-skips={} not-mirrored={} gpu-copy-dest hits={gh} textures={gn} inserted-slot-calls={} texcache-flushes={} recommit-rechecks={}; reaper pending={} freed={} worst-release={:.1}ms",
            up.pumps, staged.len(), up.tracked.len(), up.uploaded_subs, up.uploaded_bytes, up.unreadable, up.rejected, crate::cmdlist_wrap::inserted_hits(), crate::cmdlist_wrap::flush_hits(), up.recommitted,
            up.reaper.pending(), up.reaper.freed(), up.reaper.worst_us() as f64 / 1000.0));
    }
}

#[cfg(test)]
pub(crate) fn test_offsets(desc: &[u8; 0x38]) -> Result<(u64, usize, Vec<u32>), String> {
    let l = layout_for(desc)?;
    let w = u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap()) as u32;
    let h = u32::from_le_bytes(desc[0x18..0x1c].try_into().unwrap());
    let m = unsafe { l.submap(0, w, h) }.ok_or_else(|| "no element map for subresource 0".to_string())?;
    Ok((l.total, l.bpe as usize, m.offs.clone()))
}

#[cfg(test)]
pub(crate) struct TestWw {
    pub watched: Option<bool>,
    pub sent: bool,
    pub band_uploads: u64,
    pub lost: u64,
    pub dirty_subs: u64,
    pub uploaded_subs: u64,
    pub unchanged: u64,
    pub freed: bool,
    pub fallbacks: u64,
    pub rcached: bool,
    pub rs: [u64; 4],
}

#[cfg(test)]
pub(crate) fn test_ww(res: *mut c_void) -> TestWw {
    let g = UP.lock().unwrap();
    let Some(up) = g.as_ref() else { return TestWw { watched: None, sent: false, band_uploads: 0, lost: 0, dirty_subs: 0, uploaded_subs: 0, unchanged: 0, freed: false, fallbacks: 0, rcached: false, rs: rs_total() } };
    let t = up.tracked.iter().find(|t| t.res == res as usize);
    TestWw { watched: t.map(|t| t.ww), sent: t.map_or(false, |t| t.sent.iter().all(|&x| x)), band_uploads: up.ww.band_uploads, lost: up.ww.lost, dirty_subs: up.ww.dirty_subs, uploaded_subs: up.uploaded_subs, unchanged: up.ww.unchanged, freed: t.map_or(false, |t| t.freed), fallbacks: up.ww.fallbacks, rcached: t.is_some_and(|t| t.rcache.is_some()), rs: rs_total() }
}

#[cfg(test)]
mod zero_watch_tests {
    use super::*;

    #[test]
    fn nonzero_found_only_inside_readable_ranges() {
        let mut buf = vec![0u8; 4096];
        let a = buf.as_ptr() as usize;
        assert!(!first_nonzero(&[(a, a + 4096)]));
        buf[3000] = 7;
        assert!(first_nonzero(&[(a, a + 4096)]));
        assert!(!first_nonzero(&[(a, a + 2048)]));
        assert!(first_nonzero(&[(a, a + 16), (a + 2990, a + 3001)]));
    }

    #[test]
    fn decommitted_memory_skips_the_read_instead_of_faulting() {
        extern "system" {
            fn VirtualAlloc(addr: *mut c_void, len: usize, ty: u32, protect: u32) -> *mut c_void;
            fn VirtualFree(addr: *mut c_void, len: usize, ty: u32) -> i32;
        }
        const PAGE: usize = 4096;
        unsafe {
            let base = VirtualAlloc(core::ptr::null_mut(), 3 * PAGE, 0x3000, 0x04) as usize;
            assert_ne!(base, 0);
            core::ptr::write_bytes(base as *mut u8, 0xab, 3 * PAGE);
            assert!(hash_title(base, 3 * PAGE, true).is_some());
            assert!(first_nonzero(&[(base, base + 3 * PAGE)]));
            assert_eq!(VirtualFree((base + PAGE) as *mut c_void, PAGE, 0x4000), 1);
            assert!(hash_title(base, 3 * PAGE, true).is_none());
            assert!(hash_title(base + PAGE, PAGE, false).is_none());
            assert!(hash_title(base, PAGE, false).is_some());
            assert!(hash_title(base + 2 * PAGE, PAGE, false).is_some());
            assert!(first_nonzero(&[(base + PAGE, base + 2 * PAGE)]));
            let mut copy = vec![0u8; 3 * PAGE];
            assert!(copy_title(base, copy.as_mut_ptr() as usize, PAGE));
            assert!(!copy_title(base + PAGE, copy.as_mut_ptr() as usize, PAGE));
            let (w, h, bpe) = (32usize, 128usize, 4usize);
            let offs: Vec<u32> = (0..w * h).map(|i| ((i % (3 * PAGE / bpe)) * bpe) as u32).collect();
            let mut dst = vec![0u8; w * h * bpe];
            assert!(!detile_rows(dst.as_mut_ptr(), w * bpe, base as *const u8, base, 3 * PAGE, &offs, w, h, bpe));
            VirtualFree(base as *mut c_void, 0, 0x8000);
        }
    }

    #[test]
    fn guarded_reads_cost_one_call_per_range_not_per_byte() {
        for &len in &[16usize << 10, 256 << 10, 4 << 20] {
            let buf: Vec<u8> = (0..len).map(|i| (i * 31 + i / 7) as u8).collect();
            let addr = buf.as_ptr() as usize;
            let iters = ((64usize << 20) / len).max(4);
            let time = |guarded: bool| {
                let t = std::time::Instant::now();
                let mut acc = 0u64;
                for _ in 0..iters {
                    acc ^= if guarded {
                        hash_title(addr, len, false).unwrap().0
                    } else {
                        hash_bytes(unsafe { core::slice::from_raw_parts(addr as *const u8, len) })
                    };
                }
                std::hint::black_box(acc);
                t.elapsed().as_nanos() as f64 / iters as f64
            };
            time(false);
            time(true);
            let (direct, guarded) = (time(false), time(true));
            eprintln!("hash of {:>5} KiB: unguarded {direct:>10.0} ns/call, guarded {guarded:>10.0} ns/call ({:+.1}%)", len >> 10, (guarded / direct - 1.0) * 100.0);
            assert_eq!(hash_title(addr, len, true), Some((hash_bytes(&buf), buf.iter().all(|&b| b == 0))));
        }
    }

    #[test]
    fn notes_find_only_overlapping_textures() {
        let index = vec![(0x1000, 0x2000, 7), (0x2000, 0x3000, 8), (0x8000, 0x9000, 9)];
        assert_eq!(overlapping(&index, 0x1800, 0x1900), vec![7]);
        let mut both = overlapping(&index, 0x1f00, 0x2100);
        both.sort();
        assert_eq!(both, vec![7, 8]);
        assert!(overlapping(&index, 0x3000, 0x8000).is_empty());
        assert_eq!(overlapping(&index, 0x8fff, 0xa000), vec![9]);
    }

    #[test]
    fn parallel_detile_matches_serial() {
        let (w, h, bpe, pitch) = (2048usize, 2176usize, 4usize, 8192usize + 256);
        let src: Vec<u8> = (0..w * h * bpe).map(|i| (i * 7 + i / 3) as u8).collect();
        let offs: Vec<u32> = (0..w * h).map(|i| (((i * 2654435761usize) % (w * h)) * bpe) as u32).collect();
        let mut a = vec![0u8; pitch * h];
        let mut b = vec![0u8; pitch * h];
        unsafe {
            assert!(detile_rows(a.as_mut_ptr(), pitch, src.as_ptr(), src.as_ptr() as usize, src.len(), &offs, w, h, bpe));
            detile_span(b.as_mut_ptr() as usize, pitch, src.as_ptr() as usize, &offs, w, 0..h, bpe);
        }
        assert!(w * h * bpe >= PARALLEL_DETILE_BYTES);
        assert_eq!(a, b);
        assert_eq!(&a[pitch * 3 + 8..pitch * 3 + 12], &src[offs[3 * w + 2] as usize..offs[3 * w + 2] as usize + 4]);
    }

    #[test]
    fn adjacent_notes_of_one_kind_coalesce() {
        take_notes();
        TomboxNoteTitleMemory(0x10000, 0x1000, NOTE_WRITTEN);
        TomboxNoteTitleMemory(0x11000, 0x1000, NOTE_WRITTEN);
        TomboxNoteTitleMemory(0x12000, 0x1000, NOTE_COMMITTED);
        TomboxNoteTitleMemory(0, 0x1000, NOTE_WRITTEN);
        assert_eq!(take_notes(), vec![(0x10000, 0x2000, NOTE_WRITTEN), (0x12000, 0x1000, NOTE_COMMITTED)]);
    }
}
#[cfg(test)]
mod pool_tests {
    use super::*;

    #[test]
    fn buckets_round_up_to_powers_of_two_with_a_floor() {
        assert_eq!(pool_bucket(1), 64 << 10);
        assert_eq!(pool_bucket(64 << 10), 64 << 10);
        assert_eq!(pool_bucket((64 << 10) + 1), 128 << 10);
        assert_eq!(pool_bucket(5 << 20), 8 << 20);
    }

    #[test]
    fn pool_reuses_exact_buckets_and_respects_the_cap() {
        let (mut pool, mut bytes) = (Vec::new(), 0u64);
        assert!(pool_give(&mut pool, &mut bytes, 1 << 20, 0xa, 3 << 20));
        assert!(pool_give(&mut pool, &mut bytes, 2 << 20, 0xb, 3 << 20));
        assert!(!pool_give(&mut pool, &mut bytes, 1 << 20, 0xc, 3 << 20));
        assert_eq!(bytes, 3 << 20);
        assert_eq!(pool_take(&mut pool, &mut bytes, 4 << 20), None);
        assert_eq!(pool_take(&mut pool, &mut bytes, 2 << 20), Some(0xb));
        assert_eq!(bytes, 1 << 20);
        assert_eq!(pool_take(&mut pool, &mut bytes, 1 << 20), Some(0xa));
        assert_eq!(pool_take(&mut pool, &mut bytes, 1 << 20), None);
        assert_eq!(bytes, 0);
    }
}
#[cfg(test)]
mod detile_speed_tests {
    use super::*;

    #[test]
    fn one_megabyte_detiles_on_one_thread_in_a_few_ms() {
        let (w, h, bpe) = (512usize, 512usize, 4usize);
        let pitch = w * bpe;
        let src: Vec<u8> = (0..w * h * bpe).map(|i| (i * 13) as u8).collect();
        let offs: Vec<u32> = (0..w * h).map(|i| (((i * 2654435761usize) % (w * h)) * bpe) as u32).collect();
        let mut dst = vec![0u8; pitch * h];
        let mut per = f64::MAX;
        for _ in 0..10 {
            let t = std::time::Instant::now();
            unsafe { detile_span(dst.as_mut_ptr() as usize, pitch, src.as_ptr() as usize, &offs, w, 0..h, bpe) };
            per = per.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        eprintln!("single-thread detile of 1 MB: best of 10 {per:.2} ms");
        for (i, &o) in offs.iter().enumerate() {
            let (o, d) = (o as usize, (i / w) * pitch + (i % w) * bpe);
            assert_eq!(&dst[d..d + bpe], &src[o..o + bpe], "element {i}");
        }
        if !cfg!(debug_assertions) {
            assert!(per < 20.0, "{per} ms");
        }
    }
}

#[cfg(test)]
mod direct_queue_and_pending_tests {
    use super::*;

    #[test]
    fn direct_queue_registration_and_detection() {
        let fake_q = 0xdead_beefusize as *mut c_void;
        assert!(!is_direct_queue(fake_q));
        register_direct_queue(fake_q);
        assert!(is_direct_queue(fake_q));
        assert!(!is_direct_queue((fake_q as usize + 8) as *mut c_void));
        assert!(direct_queue().is_some());
    }
}
