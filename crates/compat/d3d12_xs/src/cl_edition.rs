use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::HashMap;
use std::sync::Mutex;

use compat_common::record;

pub const SLOT_131: usize = 131;
pub const SLOT_132: usize = 132;
pub const NEW_LIST_LAYOUT_EDITION: u32 = 250400;
const PC_SLOT_RESOURCE_BARRIER: usize = 26;
const LIST_TYPE_COPY: u32 = 3;
const TAG: &[u8] = b"_xbld_edition_sdktype=GXDK,";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListLayout {
    Gxdk2021,
    Gxdk2504,
}

pub fn editions(bytes: &[u8]) -> Vec<u32> {
    let mut v: Vec<u32> = bytes
        .split(|&b| b == 0)
        .filter_map(|f| f.strip_prefix(TAG))
        .filter(|r| r.len() == 6 && r.iter().all(u8::is_ascii_digit))
        .filter_map(|r| core::str::from_utf8(r).ok()?.parse().ok())
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

pub fn layout_for(editions: &[u32]) -> ListLayout {
    if editions.iter().any(|&e| e >= NEW_LIST_LAYOUT_EDITION) {
        ListLayout::Gxdk2504
    } else {
        ListLayout::Gxdk2021
    }
}

pub fn xbld_range(headers: &[u8]) -> Option<(usize, usize)> {
    let rd16 = |o: usize| headers.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let rd32 = |o: usize| headers.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    if headers.get(0..2)? != b"MZ" {
        return None;
    }
    let nt = rd32(0x3c)?;
    if headers.get(nt..nt + 4)? != b"PE\0\0" {
        return None;
    }
    let count = rd16(nt + 6)?;
    let opt = rd16(nt + 20)?;
    let image_size = rd32(nt + 24 + 56)?;
    let table = nt + 24 + opt;
    for i in 0..count {
        let s = table + 40 * i;
        if headers.get(s..s + 8)? != b".xbld\0\0\0" {
            continue;
        }
        let (size, rva) = (rd32(s + 8)?, rd32(s + 12)?);
        if size == 0 || rva.checked_add(size)? > image_size {
            return None;
        }
        return Some((rva, size));
    }
    None
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThreadStackLimits(low: *mut usize, high: *mut usize);
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut *mut c_void, hash: *mut u32) -> u16;
    fn GetModuleHandleExW(flags: u32, addr: *const c_void, out: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(h: *mut c_void, buf: *mut u16, n: u32) -> u32;
    fn ReadProcessMemory(p: *mut c_void, base: *const c_void, buf: *mut c_void, n: usize, read: *mut usize) -> i32;
}

fn module_base(pc: usize) -> Option<usize> {
    let mut base: *mut c_void = core::ptr::null_mut();
    (unsafe { GetModuleHandleExW(6, pc as *const c_void, &mut base) } != 0 && !base.is_null()).then_some(base as usize)
}

fn read(addr: usize, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut got = 0usize;
    let ok = unsafe { ReadProcessMemory(-1isize as *mut c_void, addr as *const c_void, buf.as_mut_ptr() as *mut c_void, len, &mut got) } != 0;
    (ok && got == len).then_some(buf)
}

fn module_editions(base: usize) -> Vec<u32> {
    let Some((rva, size)) = read(base, 0x1000).and_then(|h| xbld_range(&h)) else { return Vec::new() };
    read(base + rva, size.min(1 << 20)).map(|b| editions(&b)).unwrap_or_default()
}

fn module_name(base: usize) -> String {
    let mut path = [0u16; 512];
    let n = unsafe { GetModuleFileNameW(base as *mut c_void, path.as_mut_ptr(), path.len() as u32) } as usize;
    if n == 0 || n >= path.len() {
        return format!("{base:#x}");
    }
    let s = String::from_utf16_lossy(&path[..n]);
    s.rsplit(['/', '\\']).next().unwrap_or("?").to_string()
}

pub fn location(pc: usize) -> String {
    match module_base(pc) {
        Some(b) => format!("{}+{:#x}", module_name(b), pc - b),
        None => format!("{pc:#x}"),
    }
}

pub fn after_call(code: &[u8; 8]) -> bool {
    let c = code.len();
    if code[c - 5] == 0xe8 {
        return true;
    }
    for k in [2usize, 3, 4, 6, 7] {
        if code[c - k] != 0xff {
            continue;
        }
        let m = code[c - k + 1];
        if (m >> 3) & 7 != 2 {
            continue;
        }
        let (md, rm) = (m >> 6, m & 7);
        let sib = (md != 3 && rm == 4) as usize;
        if sib == 1 && k < 3 {
            continue;
        }
        let len = match md {
            3 => 2,
            1 => 3 + sib,
            2 => 6 + sib,
            _ if rm == 5 => 6,
            _ if sib == 1 && code[c - k + 2] & 7 == 5 => 7,
            _ => 2 + sib,
        };
        if len == k {
            return true;
        }
    }
    false
}

fn is_return_address(pc: usize) -> bool {
    pc > 8 && read(pc - 8, 8).and_then(|b| <[u8; 8]>::try_from(b).ok()).is_some_and(|b| after_call(&b))
}

pub fn first_foreign(frames: &[usize], own: Option<usize>, base_of: impl Fn(usize) -> Option<usize>, is_ret: impl Fn(usize) -> bool) -> Option<usize> {
    frames.iter().copied().find(|&f| f != 0 && base_of(f).is_some() && base_of(f) != own && is_ret(f))
}

#[inline(never)]
pub(crate) fn caller_pc() -> usize {
    let own = module_base(caller_pc as *const () as usize);
    let mut frames = [core::ptr::null_mut::<c_void>(); 12];
    let n = unsafe { RtlCaptureStackBackTrace(1, frames.len() as u32, frames.as_mut_ptr(), core::ptr::null_mut()) } as usize;
    let walked: Vec<usize> = frames[..n].iter().map(|&p| p as usize).collect();
    if let Some(pc) = first_foreign(&walked, own, module_base, is_return_address) {
        return pc;
    }
    let anchor = 0usize;
    let sp = core::hint::black_box(&anchor) as *const usize as usize & !7;
    let (mut lo, mut hi) = (0usize, 0usize);
    unsafe { GetCurrentThreadStackLimits(&mut lo, &mut hi) };
    let end = hi.min(sp + 0x4000);
    let raw: Vec<usize> = if sp >= lo && sp < end { (sp..end).step_by(8).map(|a| unsafe { core::ptr::read_volatile(a as *const usize) }).collect() } else { Vec::new() };
    first_foreign(&raw, own, module_base, is_return_address).unwrap_or(0)
}

static LAYOUTS: Mutex<Option<HashMap<usize, ListLayout>>> = Mutex::new(None);

pub(crate) fn layout_at(pc: usize) -> ListLayout {
    let Some(base) = module_base(pc) else { return ListLayout::Gxdk2021 };
    let mut g = LAYOUTS.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(&l) = map.get(&base) {
        return l;
    }
    let eds = module_editions(base);
    let l = layout_for(&eds);
    map.insert(base, l);
    record("d3d12_xs", "cl_edition", &format!("{}: GXDK editions {eds:?} -> command-list layout {l:?}", module_name(base)));
    l
}

static PREV: [AtomicUsize; 2] = [const { AtomicUsize::new(0) }; 2];
static SYNC_LOG: AtomicUsize = AtomicUsize::new(0);
static LEGACY_LOG: [AtomicUsize; 2] = [const { AtomicUsize::new(0) }; 2];

unsafe fn sync_op(this: *mut c_void, slot: usize, a: u32, b: u32, base: u64, size: u64, pc: usize) {
    if SYNC_LOG.fetch_add(1, Ordering::Relaxed) < 16 {
        record(
            "d3d12_xs",
            "cl_edition",
            &format!("slot {slot} ({a:#x}, {b:#x}, base {base:#x}, size {size:#x}) from {}: GXDK 2504+ list synchronisation command; recorded as a global UAV barrier", location(pc)),
        );
    }
    if this.is_null() || unsafe { crate::fill::list_type(this) } == LIST_TYPE_COPY {
        return;
    }
    let barrier: [u64; 4] = [2, 0, 0, 0];
    let vt = unsafe { *(this as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const [u64; 4]) = unsafe { core::mem::transmute(*vt.add(PC_SLOT_RESOURCE_BARRIER)) };
    unsafe { f(this, 1, &barrier) };
}

type Raw = unsafe extern "system" fn(*mut c_void, usize, usize, u64, u64) -> usize;

unsafe fn dispatch(layout: ListLayout, prev: usize, this: *mut c_void, slot: usize, a: usize, b: usize, base: u64, size: u64, pc: usize) {
    match layout {
        ListLayout::Gxdk2504 => unsafe { sync_op(this, slot, a as u32, b as u32, base, size, pc) },
        ListLayout::Gxdk2021 => {
            let handled = prev != 0 && prev != crate::cmdlist_wrap::inserted_stub_addr();
            if LEGACY_LOG[slot - SLOT_131].fetch_add(1, Ordering::Relaxed) < 4 {
                let what = if handled { "forwarded to the handler registered before this layer" } else { "no implementation for this edition in this build; command ignored" };
                record("d3d12_xs", "cl_edition", &format!("slot {slot} from {}: GXDK 2021-layout command, {what}", location(pc)));
            }
            if handled {
                let f: Raw = unsafe { core::mem::transmute(prev) };
                unsafe { f(this, a, b, base, size) };
            }
        }
    }
}

#[inline(never)]
pub unsafe extern "system" fn slot131(this: *mut c_void, a: usize, b: usize, base: u64, size: u64) -> usize {
    let pc = caller_pc();
    unsafe { dispatch(layout_at(pc), PREV[0].load(Ordering::Relaxed), this, SLOT_131, a, b, base, size, pc) };
    0
}

#[inline(never)]
pub unsafe extern "system" fn slot132(this: *mut c_void, a: usize, b: usize, base: u64, size: u64) -> usize {
    let pc = caller_pc();
    unsafe { dispatch(layout_at(pc), PREV[1].load(Ordering::Relaxed), this, SLOT_132, a, b, base, size, pc) };
    0
}

pub fn install_cmdlist(vt: &[AtomicUsize]) {
    if vt.len() <= SLOT_132 {
        return;
    }
    for (slot, f) in [(SLOT_131, slot131 as *const () as usize), (SLOT_132, slot132 as *const () as usize)] {
        let prev = vt[slot].load(Ordering::Relaxed);
        if prev != f {
            PREV[slot - SLOT_131].store(prev, Ordering::Relaxed);
        }
        vt[slot].store(f, Ordering::Relaxed);
    }
}

pub const DEV_SLOT_127: usize = 127;
pub const DEV_SLOT_133: usize = 133;
pub const BARRIER_LAYOUT_UNDEFINED: u32 = 0xffff_ffff;
const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;

pub fn layout_state(layout: u32) -> u32 {
    match layout {
        1 | 19 | 25 => 0xac3,
        2 => 0x4,
        3 | 20 | 26 => 0x8,
        4 => 0x10,
        5 => 0x20,
        6 | 21 | 27 => 0xc0,
        7 | 22 | 28 => 0x800,
        8 | 23 | 29 => 0x400,
        9 => 0x2000,
        10 => 0x1000,
        11 => 0x100_0000,
        _ => 0,
    }
}

type Guid = crate::device_wrap::Guid;
type Device = crate::device_wrap::DeviceWrapper;
type PlacedFn = unsafe extern "system" fn(*mut Device, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32;
type CommittedFn = unsafe extern "system" fn(*mut Device, *const u32, u32, *const u32, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32;
type PrevPlaced = unsafe extern "system" fn(*mut c_void, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32;
type PrevCommitted = unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u32, u32, *const c_void, *mut c_void, u32, *const u32, *const Guid, *mut *mut c_void) -> i32;

static PREV_DEV: [AtomicUsize; 2] = [const { AtomicUsize::new(0) }; 2];
static DEV133_LOG: AtomicUsize = AtomicUsize::new(0);
static DEV127_LOG: AtomicUsize = AtomicUsize::new(0);

#[allow(clippy::too_many_arguments)]
unsafe fn route133(
    layout: ListLayout, prev: usize, create: PlacedFn, this: *mut c_void, va: u64, pdesc: *const u8, barrier_layout: u32,
    pclear: *const c_void, riid: *const Guid, ppv: *mut *mut c_void, pc: usize,
) -> i32 {
    match layout {
        ListLayout::Gxdk2504 => {
            let state = layout_state(barrier_layout);
            let hr = unsafe { create(this as *mut Device, va, pdesc, state, pclear, riid, ppv) };
            if DEV133_LOG.fetch_add(1, Ordering::Relaxed) < 8 {
                let dim = if pdesc.is_null() { 0 } else { unsafe { core::ptr::read_unaligned(pdesc as *const u32) } };
                record("d3d12_xs", "cl_edition", &format!("device slot 133 CreatePlacedResourceX (resource desc 1, barrier layout) va={va:#x} dim={dim} layout={barrier_layout:#x} -> state {state:#x} from {}: hr={hr:#010x}", location(pc)));
            }
            hr
        }
        ListLayout::Gxdk2021 => {
            if prev == 0 {
                return E_NOTIMPL;
            }
            let f: PrevPlaced = unsafe { core::mem::transmute(prev) };
            unsafe { f(this, va, pdesc, barrier_layout, pclear, riid, ppv) }
        }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn route127(
    layout: ListLayout, prev: usize, create: CommittedFn, this: *mut c_void, pheap: *const u32, heap_flags: u32, pdesc: *const u32,
    barrier_layout: u32, pclear: *const c_void, protected: *mut c_void, castable_count: u32, castable: *const u32,
    riid: *const Guid, ppv: *mut *mut c_void, pc: usize,
) -> i32 {
    match layout {
        ListLayout::Gxdk2504 => {
            let state = layout_state(barrier_layout);
            let hr = unsafe { create(this as *mut Device, pheap, heap_flags, pdesc, state, pclear, riid, ppv) };
            let extra = !protected.is_null() || castable_count != 0;
            if DEV127_LOG.fetch_add(1, Ordering::Relaxed) < 8 || extra {
                let dim = if pdesc.is_null() { 0 } else { unsafe { core::ptr::read_unaligned(pdesc) } };
                let dropped = if extra { format!("; protected session {protected:p} and {castable_count} castable formats ({castable:p}) not applied on the legacy path") } else { String::new() };
                record("d3d12_xs", "cl_edition", &format!("device slot 127 CreateCommittedResource3 heap flags {heap_flags:#x} dim={dim} layout={barrier_layout:#x} -> state {state:#x} from {}: hr={hr:#010x}{dropped}", location(pc)));
            }
            hr
        }
        ListLayout::Gxdk2021 => {
            if prev == 0 {
                return E_NOTIMPL;
            }
            let f: PrevCommitted = unsafe { core::mem::transmute(prev) };
            unsafe { f(this, pheap, heap_flags, pdesc, barrier_layout, pclear, protected, castable_count, castable, riid, ppv) }
        }
    }
}

#[inline(never)]
pub unsafe extern "system" fn dev133(this: *mut c_void, va: u64, pdesc: *const u8, barrier_layout: u32, pclear: *const c_void, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    let pc = caller_pc();
    unsafe { route133(layout_at(pc), PREV_DEV[1].load(Ordering::Relaxed), crate::device_wrap::create_placed_resource_x, this, va, pdesc, barrier_layout, pclear, riid, ppv, pc) }
}

#[inline(never)]
pub unsafe extern "system" fn dev127(
    this: *mut c_void, pheap: *const u32, heap_flags: u32, pdesc: *const u32, barrier_layout: u32, pclear: *const c_void,
    protected: *mut c_void, castable_count: u32, castable: *const u32, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let pc = caller_pc();
    unsafe { route127(layout_at(pc), PREV_DEV[0].load(Ordering::Relaxed), crate::device_wrap::create_committed_resource, this, pheap, heap_flags, pdesc, barrier_layout, pclear, protected, castable_count, castable, riid, ppv, pc) }
}

pub fn install_device(v: &mut [usize]) {
    for (i, (slot, f)) in [(DEV_SLOT_127, dev127 as *const () as usize), (DEV_SLOT_133, dev133 as *const () as usize)].into_iter().enumerate() {
        if v.len() > slot && v[slot] != f {
            PREV_DEV[i].store(v[slot], Ordering::Relaxed);
            v[slot] = f;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pe_with_xbld(rva: u32, size: u32, image: u32) -> Vec<u8> {
        let mut h = vec![0u8; 0x400];
        h[0..2].copy_from_slice(b"MZ");
        h[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        h[0x80..0x84].copy_from_slice(b"PE\0\0");
        h[0x86..0x88].copy_from_slice(&2u16.to_le_bytes());
        h[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes());
        h[0x98 + 56..0x98 + 60].copy_from_slice(&image.to_le_bytes());
        let t = 0x98 + 0xf0;
        h[t..t + 8].copy_from_slice(b".text\0\0\0");
        h[t + 40..t + 48].copy_from_slice(b".xbld\0\0\0");
        h[t + 48..t + 52].copy_from_slice(&size.to_le_bytes());
        h[t + 52..t + 56].copy_from_slice(&rva.to_le_bytes());
        h
    }

    #[test]
    fn editions_parse_only_exact_gxdk_fields() {
        let b = b"_xbld_edition_sdktype=GXDK,250402\0x\0_xbld_edition_sdktype=GXDK,260405\0_xbld_edition_sdktype=GRDK,260405\0_xbld_edition_sdktype=GXDK,2604051\0_xbld_edition_sdktype=GXDK,250402\0";
        assert_eq!(editions(b), vec![250402, 260405]);
        assert!(editions(b"_xbld_edition_sdktype=GXDK,26a405\0").is_empty());
        assert!(editions(b"").is_empty());
    }

    #[test]
    fn layout_switches_at_the_2504_editions() {
        assert_eq!(layout_for(&[210604]), ListLayout::Gxdk2021);
        assert_eq!(layout_for(&[220604]), ListLayout::Gxdk2021);
        assert_eq!(layout_for(&[]), ListLayout::Gxdk2021);
        assert_eq!(layout_for(&[250405]), ListLayout::Gxdk2504);
        assert_eq!(layout_for(&[250402, 260405]), ListLayout::Gxdk2504);
        assert_eq!(layout_for(&[261000]), ListLayout::Gxdk2504);
    }

    #[test]
    fn xbld_section_is_found_and_bounded_by_the_image() {
        assert_eq!(xbld_range(&pe_with_xbld(0x5000, 0x200, 0x10000)), Some((0x5000, 0x200)));
        assert_eq!(xbld_range(&pe_with_xbld(0xff00, 0x200, 0x10000)), None);
        assert_eq!(xbld_range(&pe_with_xbld(0x5000, 0, 0x10000)), None);
        let mut h = pe_with_xbld(0x5000, 0x200, 0x10000);
        h[0] = b'X';
        assert_eq!(xbld_range(&h), None);
        assert_eq!(xbld_range(&h[..0x40]), None);
    }

    #[test]
    fn editions_are_read_from_a_synthetic_image_in_memory() {
        let mut image = vec![0u8; 0x3000];
        let header = pe_with_xbld(0x2000, 0x100, 0x3000);
        image[..header.len()].copy_from_slice(&header);
        let body = b"_xbld_edition_sdktype=GXDK,210604\0_xbld_edition_sdktype=GXDK,250402\0";
        image[0x2000..0x2000 + body.len()].copy_from_slice(body);
        assert_eq!(module_editions(image.as_ptr() as usize), vec![210604, 250402]);
        assert_eq!(layout_for(&module_editions(image.as_ptr() as usize)), ListLayout::Gxdk2504);
        image[0x2000..0x2000 + body.len()].fill(0);
        assert!(module_editions(image.as_ptr() as usize).is_empty());
    }

    #[test]
    fn the_first_frame_outside_the_emulator_is_the_caller() {
        let base_of = |f: usize| (f >= 0x1_0000).then_some(f & !0xffff);
        let yes = |_: usize| true;
        assert_eq!(first_foreign(&[0x1_0010, 0x1_0020, 0x5_0030, 0x6_0000], Some(0x1_0000), base_of, yes), Some(0x5_0030));
        assert_eq!(first_foreign(&[0x1_0010, 0x1_0020], Some(0x1_0000), base_of, yes), None);
        assert_eq!(first_foreign(&[], Some(0x1_0000), base_of, yes), None);
        assert_eq!(first_foreign(&[0, 0x30, 0x7_0000], Some(0x1_0000), base_of, yes), Some(0x7_0000));
        assert_eq!(first_foreign(&[0x5_0030, 0x7_0040], Some(0x1_0000), base_of, |f| f == 0x7_0040), Some(0x7_0040));
    }

    #[test]
    fn return_addresses_must_follow_a_call() {
        let t = |tail: &[u8]| {
            let mut b = [0x90u8; 8];
            b[8 - tail.len()..].copy_from_slice(tail);
            after_call(&b)
        };
        assert!(t(&[0xe8, 1, 2, 3, 4]));
        assert!(t(&[0xff, 0x15, 1, 2, 3, 4]));
        assert!(t(&[0xff, 0xd0]));
        assert!(t(&[0x41, 0xff, 0xd3]));
        assert!(t(&[0xff, 0x50, 0x48]));
        assert!(t(&[0xff, 0x90, 0x18, 0x04, 0, 0]));
        assert!(t(&[0xff, 0x94, 0x24, 0x18, 0x04, 0, 0]));
        assert!(t(&[0xff, 0x54, 0x24, 0x20]));
        assert!(!t(&[0xff, 0x14]));
        assert!(t(&[0xff, 0x90, 0, 0, 0xff, 0x14]));
        assert!(!t(&[0x48, 0x8b, 0x05, 1, 2, 3, 4]));
        assert!(!t(&[0xff, 0x25, 1, 2, 3, 4]));
        assert!(!t(&[0xff, 0xe0]));
        assert!(!t(&[0x0f, 0xb6, 0x77, 0x06, 0x00, 0x00]));
    }

    #[test]
    fn modules_without_an_xbld_section_keep_the_2021_layout() {
        let pc = layout_for as *const () as usize;
        assert_eq!(layout_at(pc), ListLayout::Gxdk2021);
    }

    static BARRIERS: AtomicUsize = AtomicUsize::new(0);
    static BARRIER_SHAPE: AtomicUsize = AtomicUsize::new(0);
    static FORWARDED: AtomicUsize = AtomicUsize::new(0);
    static FORWARDED_ARGS: AtomicUsize = AtomicUsize::new(0);
    static FAKE_TYPE: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn fake_get_type(_this: *mut c_void) -> u32 {
        FAKE_TYPE.load(Ordering::Relaxed) as u32
    }

    unsafe extern "system" fn fake_barrier(_this: *mut c_void, n: u32, b: *const [u64; 4]) {
        BARRIERS.fetch_add(1, Ordering::Relaxed);
        BARRIER_SHAPE.store(((unsafe { (*b)[0] } as usize) << 8) | n as usize, Ordering::Relaxed);
    }

    unsafe extern "system" fn fake_prev(_this: *mut c_void, a: usize, b: usize, base: u64, size: u64) -> usize {
        FORWARDED.fetch_add(1, Ordering::Relaxed);
        FORWARDED_ARGS.store(a + b + base as usize + size as usize, Ordering::Relaxed);
        0
    }

    #[repr(C)]
    struct FakeList {
        vt: *const usize,
    }

    #[test]
    fn sync_commands_are_a_global_uav_barrier_only_on_2504_callers() {
        let mut vt = [0usize; 32];
        vt[8] = fake_get_type as *const () as usize;
        vt[PC_SLOT_RESOURCE_BARRIER] = fake_barrier as *const () as usize;
        let mut list = FakeList { vt: vt.as_ptr() };
        let this = &mut list as *mut FakeList as *mut c_void;
        let pc = layout_for as *const () as usize;

        for ty in [0usize, 2] {
            FAKE_TYPE.store(ty, Ordering::Relaxed);
            BARRIERS.store(0, Ordering::Relaxed);
            unsafe { dispatch(ListLayout::Gxdk2504, 0, this, SLOT_131, 1, 2, 3, 4, pc) };
            assert_eq!(BARRIERS.load(Ordering::Relaxed), 1);
            assert_eq!(BARRIER_SHAPE.load(Ordering::Relaxed), (2 << 8) | 1);
            unsafe { dispatch(ListLayout::Gxdk2504, 0, this, SLOT_132, 0, 0, 0, 0, pc) };
            assert_eq!(BARRIERS.load(Ordering::Relaxed), 2);
        }

        FAKE_TYPE.store(LIST_TYPE_COPY as usize, Ordering::Relaxed);
        BARRIERS.store(0, Ordering::Relaxed);
        unsafe { dispatch(ListLayout::Gxdk2504, 0, this, SLOT_131, 1, 2, 3, 4, pc) };
        assert_eq!(BARRIERS.load(Ordering::Relaxed), 0);
        unsafe { dispatch(ListLayout::Gxdk2504, 0, core::ptr::null_mut(), SLOT_131, 1, 2, 3, 4, pc) };
        assert_eq!(BARRIERS.load(Ordering::Relaxed), 0);

        FAKE_TYPE.store(0, Ordering::Relaxed);
        unsafe { dispatch(ListLayout::Gxdk2021, 0, this, SLOT_131, 1, 2, 3, 4, pc) };
        unsafe { dispatch(ListLayout::Gxdk2021, crate::cmdlist_wrap::inserted_stub_addr(), this, SLOT_132, 1, 2, 3, 4, pc) };
        assert_eq!(BARRIERS.load(Ordering::Relaxed), 0);
        assert_eq!(FORWARDED.load(Ordering::Relaxed), 0);

        unsafe { dispatch(ListLayout::Gxdk2021, fake_prev as *const () as usize, this, SLOT_131, 1, 2, 3, 4, pc) };
        assert_eq!(BARRIERS.load(Ordering::Relaxed), 0);
        assert_eq!(FORWARDED.load(Ordering::Relaxed), 1);
        assert_eq!(FORWARDED_ARGS.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn install_routes_both_slots_and_keeps_what_was_there() {
        let vt: Vec<AtomicUsize> = (0..160).map(|i| AtomicUsize::new(0x1000 + i)).collect();
        install_cmdlist(&vt);
        assert_eq!(vt[SLOT_131].load(Ordering::Relaxed), slot131 as *const () as usize);
        assert_eq!(vt[SLOT_132].load(Ordering::Relaxed), slot132 as *const () as usize);
        assert_eq!(PREV[0].load(Ordering::Relaxed), 0x1000 + SLOT_131);
        assert_eq!(PREV[1].load(Ordering::Relaxed), 0x1000 + SLOT_132);
        install_cmdlist(&vt);
        assert_eq!(PREV[0].load(Ordering::Relaxed), 0x1000 + SLOT_131);
        assert_eq!(PREV[1].load(Ordering::Relaxed), 0x1000 + SLOT_132);
        let short: Vec<AtomicUsize> = (0..100).map(AtomicUsize::new).collect();
        install_cmdlist(&short);
        assert_eq!(short[99].load(Ordering::Relaxed), 99);
    }

    static PLACED_ARGS: AtomicUsize = AtomicUsize::new(0);
    static COMMITTED_ARGS: AtomicUsize = AtomicUsize::new(0);
    static PREV_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "system" fn fake_place(_d: *mut Device, va: u64, _desc: *const u8, state: u32, _c: *const c_void, _r: *const Guid, _p: *mut *mut c_void) -> i32 {
        PLACED_ARGS.store(((va as usize) << 16) | state as usize, Ordering::Relaxed);
        0
    }

    unsafe extern "system" fn fake_commit(_d: *mut Device, _h: *const u32, flags: u32, _desc: *const u32, state: u32, _c: *const c_void, _r: *const Guid, _p: *mut *mut c_void) -> i32 {
        COMMITTED_ARGS.store(((flags as usize) << 16) | state as usize, Ordering::Relaxed);
        0
    }

    unsafe extern "system" fn prev_place(_t: *mut c_void, _va: u64, _d: *const u8, layout: u32, _c: *const c_void, _r: *const Guid, _p: *mut *mut c_void) -> i32 {
        PREV_CALLS.store(0x100 | layout as usize, Ordering::Relaxed);
        7
    }

    unsafe extern "system" fn prev_commit(_t: *mut c_void, _h: *const u32, _f: u32, _d: *const u32, layout: u32, _c: *const c_void, _p: *mut c_void, _n: u32, _cf: *const u32, _r: *const Guid, _o: *mut *mut c_void) -> i32 {
        PREV_CALLS.store(0x200 | layout as usize, Ordering::Relaxed);
        9
    }

    #[test]
    fn barrier_layouts_map_to_the_matching_resource_states() {
        assert_eq!(layout_state(BARRIER_LAYOUT_UNDEFINED), 0);
        assert_eq!(layout_state(0), 0);
        assert_eq!(layout_state(1), 0xac3);
        assert_eq!(layout_state(2), 0x4);
        assert_eq!(layout_state(3), 0x8);
        assert_eq!(layout_state(26), 0x8);
        assert_eq!(layout_state(4), 0x10);
        assert_eq!(layout_state(5), 0x20);
        assert_eq!(layout_state(6), 0xc0);
        assert_eq!(layout_state(27), 0xc0);
        assert_eq!(layout_state(7), 0x800);
        assert_eq!(layout_state(28), 0x800);
        assert_eq!(layout_state(23), 0x400);
        assert_eq!(layout_state(9), 0x2000);
        assert_eq!(layout_state(10), 0x1000);
        assert_eq!(layout_state(11), 0x100_0000);
        assert_eq!(layout_state(18), 0);
        assert_eq!(layout_state(30), 0);
    }

    #[test]
    fn placed_and_committed_creation_convert_the_layout_on_2504_callers_only() {
        let pc = layout_for as *const () as usize;
        let null = core::ptr::null_mut::<c_void>();
        let none = core::ptr::null::<Guid>();
        let hr = unsafe { route133(ListLayout::Gxdk2504, 0, fake_place, null, 0x40, core::ptr::null(), 3, core::ptr::null(), none, core::ptr::null_mut(), pc) };
        assert_eq!(hr, 0);
        assert_eq!(PLACED_ARGS.load(Ordering::Relaxed), (0x40 << 16) | 0x8);
        let hr = unsafe { route127(ListLayout::Gxdk2504, 0, fake_commit, null, core::ptr::null(), 0x4000, core::ptr::null(), 8, core::ptr::null(), null, 0, core::ptr::null(), none, core::ptr::null_mut(), pc) };
        assert_eq!(hr, 0);
        assert_eq!(COMMITTED_ARGS.load(Ordering::Relaxed), (0x4000 << 16) | 0x400);

        let hr = unsafe { route133(ListLayout::Gxdk2021, prev_place as *const () as usize, fake_place, null, 0x99, core::ptr::null(), 5, core::ptr::null(), none, core::ptr::null_mut(), pc) };
        assert_eq!(hr, 7);
        assert_eq!(PREV_CALLS.load(Ordering::Relaxed), 0x105);
        assert_eq!(PLACED_ARGS.load(Ordering::Relaxed), (0x40 << 16) | 0x8);
        let hr = unsafe { route127(ListLayout::Gxdk2021, prev_commit as *const () as usize, fake_commit, null, core::ptr::null(), 0x1, core::ptr::null(), 6, core::ptr::null(), null, 0, core::ptr::null(), none, core::ptr::null_mut(), pc) };
        assert_eq!(hr, 9);
        assert_eq!(PREV_CALLS.load(Ordering::Relaxed), 0x206);
        assert_eq!(COMMITTED_ARGS.load(Ordering::Relaxed), (0x4000 << 16) | 0x400);
    }

    #[test]
    fn old_layout_without_a_previous_handler_fails_honestly() {
        let pc = layout_for as *const () as usize;
        let null = core::ptr::null_mut::<c_void>();
        let none = core::ptr::null::<Guid>();
        assert_eq!(unsafe { route133(ListLayout::Gxdk2021, 0, fake_place, null, 0, core::ptr::null(), 0, core::ptr::null(), none, core::ptr::null_mut(), pc) }, E_NOTIMPL);
        assert_eq!(unsafe { route127(ListLayout::Gxdk2021, 0, fake_commit, null, core::ptr::null(), 0, core::ptr::null(), 0, core::ptr::null(), null, 0, core::ptr::null(), none, core::ptr::null_mut(), pc) }, E_NOTIMPL);
    }

    #[test]
    fn device_install_routes_slots_127_and_133_once() {
        let mut v: Vec<usize> = (0..160).map(|i| 0x2000 + i).collect();
        install_device(&mut v);
        assert_eq!(v[DEV_SLOT_127], dev127 as *const () as usize);
        assert_eq!(v[DEV_SLOT_133], dev133 as *const () as usize);
        assert_eq!(PREV_DEV[0].load(Ordering::Relaxed), 0x2000 + DEV_SLOT_127);
        assert_eq!(PREV_DEV[1].load(Ordering::Relaxed), 0x2000 + DEV_SLOT_133);
        install_device(&mut v);
        assert_eq!(PREV_DEV[0].load(Ordering::Relaxed), 0x2000 + DEV_SLOT_127);
        assert_eq!(PREV_DEV[1].load(Ordering::Relaxed), 0x2000 + DEV_SLOT_133);
        let mut short: Vec<usize> = (0..100).collect();
        install_device(&mut short);
        assert_eq!(short[99], 99);
    }
}
