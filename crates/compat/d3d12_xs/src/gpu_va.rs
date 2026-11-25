use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

pub const SYNTHETIC_TEXTURE_BASE: u64 = 0xf5a7_0000_0000_0000;
const RESOURCE_GET_GPU_VA: usize = 11;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub start: u64,
    pub len: u64,
    pub res: usize,
    pub buffer: bool,
}

struct Tables {
    ranges: Vec<BTreeMap<u64, Entry>>,
    cpu: Vec<BTreeMap<u64, Entry>>,
    texture_va: HashMap<usize, u64>,
    state: HashMap<usize, u32>,
    width: HashMap<usize, u64>,
    owned: HashMap<usize, HashSet<(bool, u64)>>,
    watched: HashSet<usize>,
}

static T: Mutex<Option<Tables>> = Mutex::new(None);
static NEXT_SYNTH: AtomicU64 = AtomicU64::new(SYNTHETIC_TEXTURE_BASE);

fn with<R>(f: impl FnOnce(&mut Tables) -> R) -> R {
    let mut g = T.lock().unwrap_or_else(|e| e.into_inner());
    let t = g.get_or_insert_with(|| Tables { ranges: Vec::new(), cpu: Vec::new(), texture_va: HashMap::new(), state: HashMap::new(), width: HashMap::new(), owned: HashMap::new(), watched: HashSet::new() });
    f(t)
}

pub fn forget(res: usize) {
    with(|t| {
        for (cpu, start) in t.owned.remove(&res).unwrap_or_default() {
            let levels = if cpu { &mut t.cpu } else { &mut t.ranges };
            for m in levels.iter_mut() {
                if m.get(&start).is_some_and(|e| e.res == res) {
                    m.remove(&start);
                }
            }
        }
        t.texture_va.remove(&res);
        t.state.remove(&res);
        t.width.remove(&res);
        t.watched.remove(&res);
    });
}

const GUID_TB_GPU_VA_WATCH: crate::device_wrap::Guid = crate::device_wrap::Guid { d1: 0x5c3e_91a7, d2: 0x6b0d, d3: 0x4f2a, d4: [0x8e, 0x47, 0x1d, 0x93, 0xc2, 0x5a, 0x70, 0xe4] };
const IID_IUNKNOWN: crate::device_wrap::Guid = crate::device_wrap::Guid { d1: 0, d2: 0, d3: 0, d4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46] };
const OBJECT_SET_PRIVATE_DATA_INTERFACE: usize = 5;

#[repr(C)]
struct Watch {
    vt: &'static WatchVt,
    refs: AtomicU32,
    armed: AtomicBool,
    res: usize,
}

#[repr(C)]
struct WatchVt {
    qi: unsafe extern "system" fn(*mut Watch, *const crate::device_wrap::Guid, *mut *mut c_void) -> i32,
    add: unsafe extern "system" fn(*mut Watch) -> u32,
    rel: unsafe extern "system" fn(*mut Watch) -> u32,
}

static WATCH_VT: WatchVt = WatchVt { qi: watch_qi, add: watch_add, rel: watch_rel };

unsafe extern "system" fn watch_qi(this: *mut Watch, riid: *const crate::device_wrap::Guid, ppv: *mut *mut c_void) -> i32 {
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
        let w = unsafe { Box::from_raw(this) };
        if w.armed.load(Ordering::Acquire) {
            forget(w.res);
        }
    }
    n
}

unsafe fn watch(res: *mut c_void) {
    if !with(|t| t.watched.insert(res as usize)) {
        return;
    }
    let w = Box::into_raw(Box::new(Watch { vt: &WATCH_VT, refs: AtomicU32::new(1), armed: AtomicBool::new(false), res: res as usize }));
    let set: unsafe extern "system" fn(*mut c_void, *const crate::device_wrap::Guid, *mut c_void) -> i32 = unsafe { core::mem::transmute(*(*(res as *const *const usize)).add(OBJECT_SET_PRIVATE_DATA_INTERFACE)) };
    unsafe { (*w).armed.store(true, Ordering::Release) };
    if unsafe { set(res, &GUID_TB_GPU_VA_WATCH, w as *mut c_void) } < 0 {
        unsafe { (*w).armed.store(false, Ordering::Release) };
        with(|t| t.watched.remove(&(res as usize)));
    }
    unsafe { watch_rel(w) };
}

const MAX_LEVELS: usize = 8;

fn overlapping(m: &BTreeMap<u64, Entry>, e: &Entry) -> Vec<Entry> {
    let end = e.start.saturating_add(e.len);
    m.range(..end).rev()
        .take_while(|(_, x)| x.start.saturating_add(x.len) > e.start)
        .map(|(_, x)| *x).collect()
}

fn insert_into(levels: &mut Vec<BTreeMap<u64, Entry>>, e: Entry) {
    let mut l = 0;
    loop {
        if levels.len() <= l {
            levels.push(BTreeMap::new());
        }
        let mut larger = false;
        for old in overlapping(&levels[l], &e) {
            if old.res != e.res && old.len > e.len && l + 1 < MAX_LEVELS {
                larger = true;
            } else if let Some(old) = levels[l].remove(&old.start) {
                crate::hang_cap::note_evict(&old, &e);
            }
        }
        if !larger {
            levels[l].insert(e.start, e);
            return;
        }
        l += 1;
    }
}

fn find_in(levels: &[BTreeMap<u64, Entry>], a: u64) -> Option<(Entry, u64)> {
    levels.iter()
        .filter_map(|m| m.range(..=a).next_back().map(|(_, e)| *e).filter(|e| a < e.start.saturating_add(e.len)))
        .min_by_key(|e| e.len)
        .map(|e| crate::placed_offset::true_entry(e, a))
}

pub fn insert(e: Entry) {
    if e.len != 0 {
        with(|t| {
            insert_into(&mut t.ranges, e);
            t.owned.entry(e.res).or_default().insert((false, e.start));
        });
    }
}

pub fn insert_cpu(e: Entry) {
    if e.len != 0 {
        with(|t| {
            insert_into(&mut t.cpu, e);
            t.owned.entry(e.res).or_default().insert((true, e.start));
        });
    }
}

pub fn lookup(va: u64) -> Option<(Entry, u64)> {
    with(|t| find_in(&t.ranges, va).or_else(|| find_in(&t.cpu, va)))
}

pub fn near_in(m: &BTreeMap<u64, Entry>, va: u64, n: usize) -> Vec<Entry> {
    let mut below: Vec<Entry> = m.range(..=va).rev().take(n).map(|(_, e)| *e).collect();
    below.reverse();
    below.extend(m.range(va.saturating_add(1)..).take(n).map(|(_, e)| *e));
    below
}

pub fn near(va: u64, n: usize) -> Vec<Entry> {
    with(|t| {
        let mut all: BTreeMap<u64, Entry> = BTreeMap::new();
        for m in t.ranges.iter().rev() {
            all.extend(m.iter().map(|(k, e)| (*k, *e)));
        }
        near_in(&all, va, n)
    })
}

pub fn lookup_buffer(va: u64) -> Option<(Entry, u64)> {
    with(|t| {
        t.ranges.iter()
            .filter_map(|m| m.range(..=va).next_back().map(|(_, e)| *e).filter(|e| e.buffer && va < e.start.saturating_add(e.len)))
            .min_by_key(|e| e.len)
            .map(|e| crate::placed_offset::true_entry(e, va))
    })
}

pub fn lookup_gpu(va: u64) -> Option<(Entry, u64)> {
    with(|t| find_in(&t.ranges, va))
}

pub fn set_state(res: usize, state: u32) {
    with(|t| { t.state.insert(res, state); });
}

pub fn state(res: usize) -> u32 {
    with(|t| t.state.get(&res).copied().unwrap_or(0))
}

pub fn note_barriers(n: u32, barriers: *const u8) {
    if barriers.is_null() {
        return;
    }
    for i in 0..n as usize {
        let b = unsafe { core::slice::from_raw_parts(barriers.add(i * 32), 32) };
        if u32::from_le_bytes(b[0..4].try_into().unwrap()) != 0 {
            continue;
        }
        let flags = u32::from_le_bytes(b[4..8].try_into().unwrap());
        if flags & 1 != 0 {
            continue;
        }
        let res = u64::from_le_bytes(b[8..16].try_into().unwrap()) as usize;
        set_state(res, u32::from_le_bytes(b[24..28].try_into().unwrap()));
    }
}

pub fn texture_va(res: usize) -> Option<u64> {
    with(|t| t.texture_va.get(&res).copied())
}

pub fn assign_texture_va(res: usize, len: u64, placed_va: Option<u64>) -> u64 {
    let va = placed_va.unwrap_or_else(|| {
        let span = (len.max(1) + 0xffff) & !0xffff;
        NEXT_SYNTH.fetch_add(span, Ordering::Relaxed)
    });
    with(|t| { t.texture_va.insert(res, va); });
    insert(Entry { start: va, len, res, buffer: false });
    va
}

static PATCHED: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];
static ORIGINAL: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];
static ORIGINAL_MAP: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];
const RESOURCE_MAP: usize = 8;

fn original_for(table: &[AtomicUsize; 16], this: *mut c_void) -> usize {
    let vt = unsafe { *(this as *const usize) };
    PATCHED.iter().position(|p| p.load(Ordering::Acquire) == vt)
        .map(|i| table[i].load(Ordering::Acquire)).unwrap_or(0)
}

unsafe extern "system" fn map_x(this: *mut c_void, sub: u32, range: *const c_void, out: *mut *mut u8) -> i32 {
    let orig = original_for(&ORIGINAL_MAP, this);
    if orig == 0 {
        return 0x8000_4005u32 as i32;
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut u8) -> i32 = unsafe { core::mem::transmute(orig) };
    let hr = unsafe { f(this, sub, range, out) };
    if hr >= 0 && sub == 0 && !out.is_null() && unsafe { !(*out).is_null() } {
        if crate::placed_offset::bytes(this as usize) != 0 {
            let q = crate::placed_offset::map_pointer(this as usize, unsafe { *out } as u64, crate::placed::lookup(this).map(|x| x.va));
            unsafe { *out = q as *mut u8 };
        }
        let p = unsafe { *out } as u64;
        if let Some(w) = with(|t| t.width.get(&(this as usize)).copied()) {
            insert_cpu(Entry { start: p, len: w, res: this as usize, buffer: true });
        }
    }
    hr
}

unsafe extern "system" fn get_gpu_va_x(this: *mut c_void) -> u64 {
    let orig = original_for(&ORIGINAL, this);
    let va = if orig == 0 {
        0
    } else {
        let f: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(orig) };
        unsafe { f(this) }
    };
    if va != 0 {
        return va + crate::placed_offset::bytes(this as usize);
    }
    if expose_texture_va() {
        texture_va(this as usize).filter(|&v| v < SYNTHETIC_TEXTURE_BASE).unwrap_or(0)
    } else {
        0
    }
}

pub fn expose_texture_va() -> bool {
    static ENV: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| match std::env::var("TOMBOX_TEXTURE_VA") {
        Ok(v) => Some(v == "1"),
        Err(_) => None,
    });
    if let Some(v) = env {
        return v;
    }
    static IS_XONE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if IS_XONE.load(Ordering::Relaxed) {
        return true;
    }
    if crate::xone_tex::is_xbox_one() {
        IS_XONE.store(true, Ordering::Relaxed);
        return true;
    }
    false
}

unsafe fn ensure_patched(res: *mut c_void) {
    extern "system" {
        fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
    }
    static LOCK: Mutex<()> = Mutex::new(());
    let vt = unsafe { *(res as *const usize) };
    if PATCHED.iter().any(|p| p.load(Ordering::Acquire) == vt) {
        return;
    }
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if PATCHED.iter().any(|p| p.load(Ordering::Acquire) == vt) {
        return;
    }
    let Some(i) = PATCHED.iter().position(|p| p.load(Ordering::Acquire) == 0) else { return };
    let slot = (vt as *mut usize).wrapping_add(RESOURCE_GET_GPU_VA);
    let map_slot = (vt as *mut usize).wrapping_add(RESOURCE_MAP);
    let orig = unsafe { *slot };
    let orig_map = unsafe { *map_slot };
    if orig == get_gpu_va_x as usize {
        return;
    }
    ORIGINAL[i].store(orig, Ordering::Release);
    ORIGINAL_MAP[i].store(orig_map, Ordering::Release);
    PATCHED[i].store(vt, Ordering::Release);
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(map_slot as *mut c_void, 0x20, 0x04, &mut old) != 0 {
            *slot = get_gpu_va_x as usize;
            *map_slot = map_x as usize;
            VirtualProtect(map_slot as *mut c_void, 0x20, old, &mut old);
        }
    }
}


pub fn xbox_texture_size(desc: &[u8]) -> u64 {
    let u16at = |o: usize| u16::from_le_bytes([desc[o], desc[o + 1]]) as u64;
    let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
    crate::device_wrap::alloc_size_for(
        u32at(0), u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap()).max(1), (u32at(0x18) as u64).max(1),
        u16at(0x1c).max(1), u16at(0x1e).max(1), u32at(0x20), (u32at(0x24) as u64).max(1))
}

pub fn console_size_with(desc: &[u8], xg: impl Fn(&[u8]) -> Result<(u64, u64), i32>) -> u64 {
    let est = xbox_texture_size(desc);
    let d = &desc[..crate::device_console_fix::RES_DESC_SIZE];
    let reported = crate::device_console_fix::alloc_info_with(d, 1, |d| crate::device_console_fix::texture_part(xg(d), || (est, 0x10000))).0;
    reported.max(est)
}

pub fn console_texture_size(desc: &[u8]) -> u64 {
    let fam = crate::device_console_fix::title_family();
    let size = if crate::device_console_fix::xg_available(fam) {
        console_size_with(desc, |d| crate::device_console_fix::xg_texture_alloc(d, fam))
    } else {
        xbox_texture_size(desc)
    };
    size.max(crate::xone_tex::xg_size(desc).unwrap_or(0))
}

pub fn is_texture_desc(desc: &[u8]) -> bool {
    desc.len() >= 0x38 && u32::from_le_bytes(desc[0..4].try_into().unwrap()) >= 2
}

pub unsafe fn alloc_backing(size: u64) -> Option<u64> {
    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, ty: u32, protect: u32) -> *mut c_void;
    }
    let size = (size.max(1) + 0xffff) & !0xffff;
    let p = unsafe { VirtualAlloc(core::ptr::null_mut(), size as usize, 0x3000, 0x04) };
    if p.is_null() { None } else { Some(p as u64) }
}

pub unsafe fn alloc_texture_backing(desc: &[u8]) -> Option<u64> {
    if !is_texture_desc(desc) {
        return None;
    }
    unsafe { alloc_backing(console_texture_size(desc)) }
}

pub unsafe fn back_committed_texture(desc: &[u8]) -> Option<u64> {
    if !expose_texture_va() {
        return None;
    }
    unsafe { alloc_texture_backing(desc) }
}

pub unsafe fn adopt_backing(res: *mut c_void, va: u64, xdesc: [u8; 0x38], init_state: u32) {
    let addref: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*(*(res as *const *const usize)).add(1)) };
    unsafe { addref(res) };
    COMMITTED_TEX.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|m| m.remove(&(res as usize)));
    crate::placed::register(res, crate::placed::Placed { va, desc: xdesc, init_state });
}

static COMMITTED_TEX: Mutex<Option<HashMap<usize, [u8; 0x38]>>> = Mutex::new(None);

pub fn note_committed_texture(res: *mut c_void, xdesc: [u8; 0x38]) {
    if !res.is_null() {
        COMMITTED_TEX.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(res as usize, xdesc);
    }
}

pub fn committed_texture_desc(res: *mut c_void) -> Option<[u8; 0x38]> {
    COMMITTED_TEX.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&(res as usize)).copied())
}

pub fn same_texture(xbox: &[u8], pc: &[u8]) -> bool {
    xbox.len() >= 0x24 && pc.len() >= 0x24 && xbox[0..4] == pc[0..4] && xbox[0x10..0x1c] == pc[0x10..0x1c] && xbox[0x20..0x24] == pc[0x20..0x24]
}

pub unsafe fn note_resource(real_device: *mut c_void, res: *mut c_void, desc: &[u8], init_state: u32, placed_va: Option<u64>) {
    let _ = real_device;
    if res.is_null() || desc.len() < 0x38 {
        return;
    }
    unsafe { watch(res) };
    set_state(res as usize, init_state);
    let buffer = u32::from_le_bytes(desc[0..4].try_into().unwrap()) == 1;
    let rvt = unsafe { *(res as *const *const usize) };
    let real_get: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(*rvt.add(RESOURCE_GET_GPU_VA)) };
    if buffer {
        let width = u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap());
        with(|t| { t.width.insert(res as usize, width); });
        unsafe { ensure_patched(res) };
        let pc_va = unsafe { real_get(res) };
        let pc_va = if pc_va == 0 || real_get as usize == get_gpu_va_x as usize { pc_va } else { pc_va + crate::placed_offset::bytes(res as usize) };
        if pc_va != 0 {
            insert(Entry { start: pc_va, len: width, res: res as usize, buffer: true });
        }
        if let Some(x) = placed_va {
            insert(Entry { start: x, len: width, res: res as usize, buffer: true });
        }
        crate::va_census::note(res as usize, desc, placed_va.unwrap_or(pc_va), width, placed_va.is_some());
        crate::firstuse::note(res as usize, crate::firstuse::CREATE);
        return;
    }
    unsafe { ensure_patched(res) };
    let len = xbox_texture_size(desc).max(1);
    let va = assign_texture_va(res as usize, len, placed_va);
    crate::va_census::note(res as usize, desc, va, len, va < SYNTHETIC_TEXTURE_BASE);
    crate::firstuse::note(res as usize, crate::firstuse::CREATE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_overlap_eviction() {
        insert(Entry { start: 0x1_0000_0000, len: 0x1000, res: 1, buffer: true });
        insert(Entry { start: 0x1_0000_2000, len: 0x1000, res: 2, buffer: true });
        assert_eq!(lookup(0x1_0000_0800).map(|(e, o)| (e.res, o)), Some((1, 0x800)));
        assert_eq!(lookup(0x1_0000_1800), None);
        insert(Entry { start: 0x1_0000_0f00, len: 0x1200, res: 3, buffer: true });
        assert_eq!(lookup(0x1_0000_0800), None);
        assert_eq!(lookup(0x1_0000_2050).map(|(e, _)| e.res), Some(3));
        assert_eq!(lookup(0x1_0000_2f00), None);
    }

    #[test]
    fn buffer_lookup_sees_through_a_texture_registered_inside_a_buffer_range() {
        let base = 0x5_0800_0000u64;
        insert(Entry { start: base, len: 0x600_0000, res: 0x30, buffer: true });
        insert(Entry { start: base + 0x544_0000, len: 0x2_0000, res: 0x31, buffer: false });
        assert_eq!(lookup(base + 0x544_c9c0).map(|(e, _)| e.res), Some(0x31));
        assert_eq!(lookup_buffer(base + 0x544_c9c0).map(|(e, o)| (e.res, o)), Some((0x30, 0x544_c9c0)));
        assert_eq!(lookup_buffer(base + 0x600_0000), None);
    }

    #[test]
    fn a_smaller_resource_never_evicts_the_larger_range_it_sits_in() {
        let base = 0x3_0800_0000u64;
        insert(Entry { start: base, len: 0x600_0000, res: 0x10, buffer: true });
        insert(Entry { start: base + 0x4f4_0000, len: 0x1_0000, res: 0x11, buffer: false });
        assert_eq!(lookup(base + 0x5_6830).map(|(e, o)| (e.res, o)), Some((0x10, 0x5_6830)));
        assert_eq!(lookup(base + 0x4f4_0100).map(|(e, o)| (e.res, o)), Some((0x11, 0x100)));
        assert_eq!(lookup(base + 0x5ff_fff0).map(|(e, _)| e.res), Some(0x10));
        insert(Entry { start: base + 0x4f4_8000, len: 0x1_0000, res: 0x12, buffer: false });
        assert_eq!(lookup(base + 0x4f4_0100).map(|(e, _)| e.res), Some(0x10));
        assert_eq!(lookup(base + 0x4f4_9000).map(|(e, _)| e.res), Some(0x12));
        insert(Entry { start: base + 0x100_0000, len: 0x800, res: 0x13, buffer: false });
        insert(Entry { start: base + 0x100_0200, len: 0x100, res: 0x14, buffer: false });
        assert_eq!(lookup(base + 0x100_0250).map(|(e, _)| e.res), Some(0x14));
        assert_eq!(lookup(base + 0x100_0400).map(|(e, _)| e.res), Some(0x13));
        assert_eq!(lookup(base + 0x100_0900).map(|(e, _)| e.res), Some(0x10));
        insert(Entry { start: base, len: 0x600_0000, res: 0x15, buffer: true });
        assert_eq!(lookup(base + 0x5_6830).map(|(e, _)| e.res), Some(0x15));
        assert_eq!(lookup(base + 0x100_0250).map(|(e, _)| e.res), Some(0x14));
        insert(Entry { start: base, len: 0x800_0000, res: 0x16, buffer: true });
        assert_eq!(lookup(base + 0x700_0000).map(|(e, _)| e.res), Some(0x16));
        assert_eq!(lookup(base + 0x5_6830).map(|(e, _)| e.res), Some(0x16));
    }

    #[test]
    fn synthetic_texture_ranges_do_not_collide() {
        let a = assign_texture_va(0x5000, 0x30000, None);
        let b = assign_texture_va(0x6000, 0x10, None);
        assert!(a >= SYNTHETIC_TEXTURE_BASE && b >= a + 0x30000);
        assert_eq!(texture_va(0x5000), Some(a));
        assert_eq!(lookup(a + 0x2ffff).map(|(e, _)| (e.res, e.buffer)), Some((0x5000, false)));
        let p = assign_texture_va(0x7000, 0x20000, Some(0x2_0000_0000));
        assert_eq!(p, 0x2_0000_0000);
    }

    fn tex2d(w: u64, h: u32, fmt: u32, mips: u16) -> [u8; 0x38] {
        let mut d = [0u8; 0x38];
        d[0..4].copy_from_slice(&3u32.to_le_bytes());
        d[0x10..0x18].copy_from_slice(&w.to_le_bytes());
        d[0x18..0x1c].copy_from_slice(&h.to_le_bytes());
        d[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
        d[0x1e..0x20].copy_from_slice(&mips.to_le_bytes());
        d[0x20..0x24].copy_from_slice(&fmt.to_le_bytes());
        d[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        d[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        d
    }

    #[test]
    fn committed_texture_backing_covers_the_xg_allocation_size() {
        let d = tex2d(1904, 1041, 28, 1);
        assert_eq!(xbox_texture_size(&d), 0x87_0000);
        let size = console_size_with(&d, |_| Ok((0x87_8000, 0x1_0000)));
        assert_eq!(size, 0x87_8000);
        assert_eq!(console_size_with(&d, |_| Err(crate::device_console_fix::XG_E_NOTIMPL)), 0x87_0000);
        assert_eq!(console_size_with(&d, |_| Ok((0x1000, 0x1000))), 0x87_0000);
        let va = unsafe { alloc_backing(size) }.expect("backing");
        unsafe { core::ptr::write_bytes(va as *mut u8, 0, size as usize) };
        assert_eq!(unsafe { *((va + size - 1) as *const u8) }, 0);
    }

    #[test]
    fn committed_texture_desc_matches_only_the_same_texture() {
        let x = tex2d(1904, 1041, 28, 1);
        let mut pc = x;
        pc[0x30..0x34].copy_from_slice(&0u32.to_le_bytes());
        pc[0x1e..0x20].copy_from_slice(&0u16.to_le_bytes());
        assert!(same_texture(&x, &pc));
        assert!(!same_texture(&x, &tex2d(1904, 1042, 28, 1)));
        assert!(!same_texture(&x, &tex2d(1904, 1041, 29, 1)));
        let res = 0x7777_0000usize as *mut c_void;
        note_committed_texture(res, x);
        assert_eq!(committed_texture_desc(res), Some(x));
        assert!(is_texture_desc(&x));
        let mut buf = x;
        buf[0..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(!is_texture_desc(&buf));
    }

    #[test]
    fn barrier_state_tracking() {
        let mut b = [0u8; 64];
        b[8..16].copy_from_slice(&0xabcu64.to_le_bytes());
        b[24..28].copy_from_slice(&0x400u32.to_le_bytes());
        b[32 + 4..32 + 8].copy_from_slice(&1u32.to_le_bytes());
        b[32 + 8..32 + 16].copy_from_slice(&0xabcu64.to_le_bytes());
        b[32 + 24..32 + 28].copy_from_slice(&0x80u32.to_le_bytes());
        set_state(0xabc, 0x4);
        note_barriers(2, b.as_ptr());
        assert_eq!(state(0xabc), 0x400);
    }

    #[test]
    fn forget_drops_only_the_ranges_of_that_resource() {
        let base = 0x6_1000_0000u64;
        insert(Entry { start: base, len: 0x1000, res: 0x51, buffer: true });
        insert(Entry { start: base + 0x10_0000, len: 0x1000, res: 0x51, buffer: true });
        insert_cpu(Entry { start: base + 0x20_0000, len: 0x100, res: 0x51, buffer: true });
        insert(Entry { start: base + 0x2000, len: 0x1000, res: 0x52, buffer: true });
        insert(Entry { start: base + 0x30_0000, len: 0x1_0000, res: 0x53, buffer: true });
        insert(Entry { start: base + 0x30_0000, len: 0x100, res: 0x51, buffer: true });
        assert_eq!(lookup(base + 0x30_0008).map(|(e, _)| e.res), Some(0x51));
        set_state(0x51, 4);
        forget(0x51);
        assert_eq!(lookup(base + 8), None);
        assert_eq!(lookup(base + 0x10_0008), None);
        assert_eq!(lookup(base + 0x20_0008), None);
        assert_eq!(lookup(base + 0x2008).map(|(e, _)| e.res), Some(0x52));
        assert_eq!(lookup(base + 0x30_0008).map(|(e, o)| (e.res, o)), Some((0x53, 8)));
        assert_eq!(state(0x51), 0);
        forget(0x51);
        assert_eq!(lookup(base + 0x2008).map(|(e, _)| e.res), Some(0x52));
    }

    #[test]
    fn a_destroyed_buffer_leaves_the_tables_and_a_second_note_keeps_the_watch() {
        use crate::device_wrap::Guid;
        const IID_DEVICE: Guid = Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
        const IID_RESOURCE: Guid = Guid { d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };
        unsafe {
            let slot = |o: *mut c_void, i: usize| *(*(o as *const *const usize)).add(i);
            let p = crate::host_proc(b"D3D12CreateDevice\0");
            assert!(p != 0);
            let mk: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(mk(core::ptr::null_mut(), 0xb000, &IID_DEVICE, &mut dev) >= 0 && !dev.is_null());
            let mut d = [0u8; 0x38];
            d[0..4].copy_from_slice(&1u32.to_le_bytes());
            d[0x10..0x18].copy_from_slice(&0x1_0000u64.to_le_bytes());
            d[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
            d[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
            d[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
            d[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
            d[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
            let props: [u32; 5] = [1, 0, 0, 1, 1];
            let create: unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u8, u32, *const u8, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(dev, 27));
            let mut res: *mut c_void = core::ptr::null_mut();
            let hr = create(dev, props.as_ptr(), 0, d.as_ptr(), 0, core::ptr::null(), &IID_RESOURCE, &mut res);
            assert!(hr >= 0 && !res.is_null(), "CreateCommittedResource hr={hr:#010x}");
            let placed = 0x6_9000_0000u64;
            note_resource(dev, res, &d, 0x40, Some(placed));
            note_resource(dev, res, &d, 0x40, Some(placed));
            assert_eq!(lookup(placed + 4).map(|(e, o)| (e.res, o)), Some((res as usize, 4)));
            assert_eq!(state(res as usize), 0x40);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(slot(res, 2));
            assert_eq!(release(res), 0);
            assert_eq!(lookup(placed + 4), None);
            assert_eq!(state(res as usize), 0);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(slot(dev, 2));
            release(dev);
        }
    }
}
