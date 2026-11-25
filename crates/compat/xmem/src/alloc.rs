use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::sys::*;
use compat_common::record;

const M: &str = "xmem";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attr(pub u64);

impl Attr {
    pub fn object_type(self) -> u32 {
        (self.0 & 0x3fff) as u32
    }
    pub fn page_size(self) -> u32 {
        ((self.0 >> 14) & 3) as u32
    }
    pub fn allocator_id(self) -> u32 {
        ((self.0 >> 16) & 0xff) as u32
    }
    pub fn align_log2(self) -> u32 {
        ((self.0 >> 24) & 0x1f) as u32
    }
    pub fn memory_type(self) -> u32 {
        ((self.0 >> 29) & 0xf) as u32
    }
    pub fn alignment(self) -> usize {
        1usize << self.align_log2()
    }
    pub fn page_bytes(self) -> usize {
        match self.page_size() {
            1 => 0x1_0000,
            2 | 3 => 0x20_0000,
            _ => 0x1000,
        }
    }
    pub fn type_valid(self) -> bool {
        self.memory_type() <= 12
    }
    pub fn plain_heap(self) -> bool {
        self.memory_type() == 0 && self.align_log2() <= 4 && self.page_size() == 0
    }
    pub fn effective_alignment(self) -> usize {
        if self.page_size() == 0 {
            self.alignment().max(16)
        } else {
            self.alignment().max(self.page_bytes())
        }
    }
}

const SMALL_LIMIT: usize = 0x2_0000;
const SMALL_ALIGN_LIMIT: usize = 0x1000;

#[derive(Clone, Copy)]
enum Kind {
    Virtual(usize),
    Aligned(usize, usize),
}

static TRACK: Mutex<Option<HashMap<usize, (Kind, u32)>>> = Mutex::new(None);

pub struct TypeStats {
    pub bytes: AtomicU64,
    pub count: AtomicU64,
    pub peak: AtomicU64,
}

pub static STATS: [TypeStats; 16] = [const { TypeStats { bytes: AtomicU64::new(0), count: AtomicU64::new(0), peak: AtomicU64::new(0) } }; 16];

fn account(ty: u32, bytes: usize, add: bool) {
    let s = &STATS[(ty & 0xf) as usize];
    if add {
        let now = s.bytes.fetch_add(bytes as u64, Ordering::Relaxed) + bytes as u64;
        s.count.fetch_add(1, Ordering::Relaxed);
        s.peak.fetch_max(now, Ordering::Relaxed);
    } else {
        s.bytes.fetch_sub(bytes as u64, Ordering::Relaxed);
        s.count.fetch_sub(1, Ordering::Relaxed);
    }
}

fn track(p: *mut c_void, kind: Kind, ty: u32) {
    TRACK.lock().unwrap().get_or_insert_with(HashMap::new).insert(p as usize, (kind, ty));
}

pub fn default_alloc(size: usize, attributes: u64) -> *mut c_void {
    let a = Attr(attributes);
    if size == 0 || !a.type_valid() {
        unsafe { SetLastError(ERROR_INVALID_PARAMETER) };
        return core::ptr::null_mut();
    }
    let ty = a.memory_type();
    let align = a.effective_alignment();
    if a.plain_heap() {
        let p = unsafe { HeapAlloc(GetProcessHeap(), 0, size) };
        if !p.is_null() {
            account(ty, size, true);
            track(p, Kind::Aligned(p as usize, size), ty);
        }
        return p;
    }
    if size <= SMALL_LIMIT && align <= SMALL_ALIGN_LIMIT && a.page_size() == 0 {
        let raw = unsafe { HeapAlloc(GetProcessHeap(), 0, size + align) };
        if raw.is_null() {
            return raw;
        }
        let p = round_up(raw as usize + 1, align) as *mut c_void;
        track(p, Kind::Aligned(raw as usize, size), ty);
        account(ty, size, true);
        return p;
    }
    let len = round_up(size, align.max(0x1000));
    if let Some(p) = crate::pool::take(ty as usize, len, align) {
        track(p, Kind::Virtual(len), ty);
        account(ty, len, true);
        crate::write_note::committed(p as usize, len);
        return p;
    }
    let p = crate::write_watch::reserve(len, align, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
    if p.is_null() {
        let err = unsafe { GetLastError() };
        record(M, "XMemAllocDefault", &format!("size={size:#x} attrs={attributes:#x} align={align:#x} -> NULL (err={err})"));
        return p;
    }
    track(p, Kind::Virtual(len), ty);
    account(ty, len, true);
    crate::write_note::committed(p as usize, len);
    p
}

pub fn default_free(address: *mut c_void, _attributes: u64) {
    if address.is_null() {
        return;
    }
    let found = TRACK.lock().unwrap().as_mut().and_then(|m| m.remove(&(address as usize)));
    match found {
        Some((Kind::Virtual(len), ty)) => {
            account(ty, len, false);
            crate::epoch::bump_range(address as usize, address as usize + len);
            if !crate::pool::give(ty as usize, address as usize, len) {
                crate::write_watch::forget(address as usize);
                unsafe { VirtualFree(address, 0, MEM_RELEASE) };
            }
        }
        Some((Kind::Aligned(raw, size), ty)) => {
            account(ty, size, false);
            unsafe { HeapFree(GetProcessHeap(), 0, raw as *mut c_void) };
        }
        None => {
            record(M, "XMemFreeDefault", &format!("untracked {address:p} -> HeapFree"));
            unsafe { HeapFree(GetProcessHeap(), 0, address) };
        }
    }
}

pub fn is_tracked(address: *const c_void) -> bool {
    TRACK.lock().unwrap().as_ref().map_or(false, |m| m.contains_key(&(address as usize)))
}

type AllocHook = unsafe extern "system" fn(usize, u64) -> *mut c_void;
type FreeHook = unsafe extern "system" fn(*mut c_void, u64);

static ALLOC_HOOK: AtomicUsize = AtomicUsize::new(0);
static FREE_HOOK: AtomicUsize = AtomicUsize::new(0);

fn alloc_log_type() -> Option<u32> {
    static T: AtomicUsize = AtomicUsize::new(usize::MAX);
    let v = T.load(Ordering::Relaxed);
    if v != usize::MAX {
        return (v != usize::MAX - 1).then_some(v as u32);
    }
    let t = std::env::var("TOMBOX_XMEM_ALLOC_LOG").ok().and_then(|s| s.trim().parse::<u32>().ok()).filter(|t| *t < 16);
    T.store(t.map_or(usize::MAX - 1, |t| t as usize), Ordering::Relaxed);
    t
}

fn log_alloc(size: usize, attributes: u64, p: *mut c_void, hooked: bool) {
    static LOGGED: AtomicUsize = AtomicUsize::new(0);
    let a = Attr(attributes);
    if alloc_log_type() != Some(a.memory_type()) || LOGGED.fetch_add(1, Ordering::Relaxed) >= 64 {
        return;
    }
    record(
        M,
        "XMemAlloc",
        &format!(
            "type={} size={size:#x} end={:#x} attrs={attributes:#x} page_size={} align={:#x} -> {p:p}{}",
            a.memory_type(),
            p as usize + size,
            a.page_size(),
            a.effective_alignment(),
            if hooked { " (title allocation hook)" } else { "" }
        ),
    );
}

#[no_mangle]
pub extern "system" fn XMemAlloc(size: usize, attributes: u64) -> *mut c_void {
    let h = ALLOC_HOOK.load(Ordering::Acquire);
    let p = if h != 0 {
        let f: AllocHook = unsafe { core::mem::transmute(h) };
        unsafe { f(size, attributes) }
    } else {
        default_alloc(size, attributes)
    };
    log_alloc(size, attributes, p, h != 0);
    p
}

#[no_mangle]
pub extern "system" fn XMemFree(address: *mut c_void, attributes: u64) {
    let h = FREE_HOOK.load(Ordering::Acquire);
    if h != 0 {
        let f: FreeHook = unsafe { core::mem::transmute(h) };
        unsafe { f(address, attributes) };
        return;
    }
    default_free(address, attributes)
}

#[no_mangle]
pub extern "system" fn XMemAllocDefault(size: usize, attributes: u64) -> *mut c_void {
    default_alloc(size, attributes)
}

#[no_mangle]
pub extern "system" fn XMemFreeDefault(address: *mut c_void, attributes: u64) {
    default_free(address, attributes)
}

#[no_mangle]
pub extern "system" fn XMemSetAllocationHooks(alloc: usize, free: usize) {
    let free = if alloc == 0 { 0 } else { free };
    ALLOC_HOOK.store(alloc, Ordering::Release);
    FREE_HOOK.store(free, Ordering::Release);
    record(M, "XMemSetAllocationHooks", &format!("alloc={alloc:#x} free={free:#x}"));
}

fn tag_char(c: u8) -> u32 {
    match c {
        b'a'..=b'z' => (c - b'a') as u32,
        b'A'..=b'Z' => (c - b'A' + 26) as u32,
        b'0'..=b'9' => (c - b'0' + 52) as u32,
        b'$' => 62,
        _ => 63,
    }
}

#[no_mangle]
pub extern "system" fn XMemMakeTag(chars: u32) -> u64 {
    if chars == 0 {
        return 0;
    }
    let b = chars.to_le_bytes();
    let hi = (tag_char(b[0]) << 8) | (tag_char(b[1]) << 14) | (tag_char(b[2]) << 20) | (tag_char(b[3]) << 26);
    (hi as u64) << 32
}

fn pool_index(memory_type: u32, alt: i32) -> Option<usize> {
    if memory_type & !0xf != 0 {
        unsafe { SetLastError(ERROR_INVALID_PARAMETER) };
        return None;
    }
    Some(if alt != 0 { (memory_type | 0x10) as usize } else { memory_type as usize })
}

#[no_mangle]
pub extern "system" fn XMemGetAllocationHysteresis(memory_type: u32, alt: i32) -> usize {
    match pool_index(memory_type, alt) {
        Some(i) => {
            let b = crate::pool::hysteresis_bytes(i);
            unsafe { SetLastError(0) };
            b as usize
        }
        None => usize::MAX,
    }
}

#[no_mangle]
pub extern "system" fn XMemSetAllocationHysteresis(memory_type: u32, alt: i32, bytes: usize) -> i32 {
    match pool_index(memory_type, alt) {
        Some(i) => {
            crate::pool::set_hysteresis(i, bytes);
            unsafe { SetLastError(0) };
            1
        }
        None => 0,
    }
}

#[no_mangle]
pub extern "system" fn XMemPreallocateFreeSpace(memory_type: u32, alt: i32) -> i32 {
    match pool_index(memory_type, alt) {
        Some(i) => {
            if !crate::pool::preallocate(i) {
                let err = unsafe { GetLastError() };
                record(M, "XMemPreallocateFreeSpace", &format!("type={memory_type} alt={alt}: commit failed after {} cached granules (err={err})", crate::pool::cached_count(i)));
                unsafe { SetLastError(err) };
                return 0;
            }
            unsafe { SetLastError(0) };
            1
        }
        None => 0,
    }
}

#[repr(C)]
pub struct AllocationStatistics {
    pub size: u32,
    pub pad: u32,
    pub allocated_bytes: u64,
    pub cached_bytes: u64,
    pub allocation_count: u32,
    pub released_pages: u32,
}

#[no_mangle]
pub unsafe extern "system" fn XMemGetAllocationStatistics(memory_type: u32, alt: i32, out: *mut AllocationStatistics) -> i32 {
    if out.is_null() || (*out).size != 0x20 {
        SetLastError(ERROR_BAD_LENGTH);
        return 0;
    }
    let o = &mut *out;
    match pool_index(memory_type, alt) {
        Some(i) => {
            let s = &STATS[memory_type as usize];
            o.allocated_bytes = if alt != 0 { 0 } else { s.bytes.load(Ordering::Relaxed) };
            o.cached_bytes = crate::pool::cached_low_bytes(i);
            o.allocation_count = if alt != 0 { 0 } else { s.count.load(Ordering::Relaxed) as u32 };
            o.released_pages = crate::pool::released(i);
        }
        None if compat_common::os_host::routes::current_build() == 19041 => {
            SetLastError(ERROR_INVALID_PARAMETER);
            return 0;
        }
        None => {
            o.allocated_bytes = 0;
            o.cached_bytes = 0;
            o.allocation_count = 0;
            o.released_pages = 0;
        }
    }
    SetLastError(0);
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_allocator_restores_the_default_free_hook() {
        thread_local! {
            static FREE_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        unsafe extern "system" fn counting_free(address: *mut c_void, attributes: u64) {
            FREE_CALLS.with(|calls| calls.set(calls.get() + 1));
            XMemFreeDefault(address, attributes);
        }
        let free = counting_free as *const () as usize;
        XMemSetAllocationHooks(XMemAllocDefault as *const () as usize, free);
        XMemFree(XMemAlloc(32, 0), 0);
        XMemSetAllocationHooks(0, free);
        XMemFree(XMemAlloc(32, 0), 0);
        XMemSetAllocationHooks(0, 0);
        assert_eq!(FREE_CALLS.with(|calls| calls.get()), 1);
    }

    #[test]
    fn attribute_fields_match_console_decode() {
        let a = Attr(0x30804000);
        assert_eq!(a.align_log2(), 16);
        assert_eq!(a.memory_type(), 1);
        assert_eq!(a.page_size(), 1);
        assert_eq!(a.allocator_id(), 0x80);
        let b = Attr(0x0480009e);
        assert_eq!(b.align_log2(), 4);
        assert_eq!(b.memory_type(), 0);
        assert!(b.plain_heap());
        assert_eq!(b.object_type(), 0x9e);
        let c = Attr(0x150004000);
        assert_eq!(c.memory_type(), 10);
        assert_eq!(c.page_size(), 1);
        assert!(!Attr(13u64 << 29).type_valid());
        assert!(Attr(12u64 << 29).type_valid());
    }

    #[test]
    fn alignment_is_honoured_for_every_path() {
        for &(size, attrs) in &[
            (0x40usize, 0x0480009eu64),
            (0x100, 0x150004000),
            (0x10000, 0xb0004000),
            (0x123, 0x0800_0000 | (1u64 << 29)),
            (0x5000, 0x0c00_0000 | (2u64 << 29)),
            (0x30_0000, 0x1500_0000),
            (0x1000, 0x30804000),
        ] {
            let p = XMemAlloc(size, attrs);
            assert!(!p.is_null(), "size={size:#x} attrs={attrs:#x}");
            let align = Attr(attrs).effective_alignment();
            assert_eq!(p as usize % align, 0, "size={size:#x} attrs={attrs:#x}");
            unsafe { core::ptr::write_bytes(p as *mut u8, 0xab, size) };
            XMemFree(p, attrs);
        }
    }

    #[test]
    fn free_default_handles_virtual_and_aligned_blocks() {
        let p = XMemAllocDefault(0x40_0000, 0x30804000);
        assert!(is_tracked(p));
        XMemFreeDefault(p, 0);
        assert!(!is_tracked(p));
        let q = XMemAllocDefault(0x80, 0x0800_0000 | (3u64 << 29));
        assert_eq!(q as usize % 256, 0);
        XMemFreeDefault(q, 0);
    }

    #[test]
    fn invalid_requests_return_null() {
        assert!(XMemAllocDefault(0, 0).is_null());
        assert!(XMemAllocDefault(16, 14u64 << 29).is_null());
    }

    #[test]
    fn make_tag_matches_console_encoding() {
        assert_eq!(XMemMakeTag(0), 0);
        let t = XMemMakeTag(u32::from_le_bytes(*b"aZ9$"));
        let hi = (t >> 32) as u32;
        assert_eq!(t & 0xffff_ffff, 0);
        assert_eq!((hi >> 8) & 0x3f, 0);
        assert_eq!((hi >> 14) & 0x3f, 51);
        assert_eq!((hi >> 20) & 0x3f, 61);
        assert_eq!(hi >> 26, 62);
        let u = XMemMakeTag(u32::from_le_bytes(*b"A-b "));
        let hu = (u >> 32) as u32;
        assert_eq!((hu >> 8) & 0x3f, 26);
        assert_eq!((hu >> 14) & 0x3f, 63);
        assert_eq!((hu >> 20) & 0x3f, 1);
        assert_eq!(hu >> 26, 63);
    }

    #[test]
    fn hysteresis_round_trips_in_pages() {
        assert_eq!(XMemGetAllocationHysteresis(4, 0), 0x400_0000);
        assert_eq!(XMemGetAllocationHysteresis(8, 1), 0x80_0000);
        assert_eq!(XMemSetAllocationHysteresis(3, 0, 0x1_8000), 1);
        assert_eq!(XMemGetAllocationHysteresis(3, 0), 0x8_0000);
        assert_eq!(XMemGetAllocationHysteresis(16, 0), usize::MAX);
        assert_eq!(unsafe { GetLastError() }, ERROR_INVALID_PARAMETER);
        assert_eq!(XMemSetAllocationHysteresis(17, 1, 5), 0);
        assert_eq!(XMemPreallocateFreeSpace(16, 0), 0);
        assert_eq!(unsafe { GetLastError() }, ERROR_INVALID_PARAMETER);
    }

    #[test]
    fn preallocate_commits_granules_that_allocations_reuse() {
        let g = crate::pool::granule();
        let mut s = AllocationStatistics { size: 0x20, pad: 0, allocated_bytes: 0, cached_bytes: 0, allocation_count: 0, released_pages: 0 };
        assert_eq!(XMemSetAllocationHysteresis(11, 0, 2 * g), 1);
        assert_eq!(unsafe { XMemGetAllocationStatistics(11, 0, &mut s) }, 1);
        assert_eq!(s.cached_bytes, 0);
        unsafe { SetLastError(5) };
        assert_eq!(XMemPreallocateFreeSpace(11, 0), 1);
        assert_eq!(unsafe { GetLastError() }, 0);
        assert_eq!(unsafe { XMemGetAllocationStatistics(11, 0, &mut s) }, 1);
        assert_eq!(s.cached_bytes, 2 * g as u64);
        let attrs = (11u64 << 29) | (1 << 14) | (16 << 24);
        let p = XMemAllocDefault(g, attrs);
        assert_eq!(p as usize % g, 0);
        assert_eq!(crate::pool::cached_count(11), 1);
        unsafe { core::ptr::write_bytes(p as *mut u8, 0x5a, g) };
        assert_eq!(unsafe { XMemGetAllocationStatistics(11, 0, &mut s) }, 1);
        assert_eq!(s.cached_bytes, g as u64);
        XMemFreeDefault(p, attrs);
        assert_eq!(crate::pool::cached_count(11), 2);
        assert_eq!(unsafe { *(p as *const u8).add(g - 1) }, 0x5a);
        assert_eq!(XMemSetAllocationHysteresis(11, 0, 0), 1);
        assert_eq!(unsafe { XMemGetAllocationStatistics(11, 0, &mut s) }, 1);
        assert_eq!(s.cached_bytes, 0);
        assert_eq!(s.released_pages, 2);
        assert_eq!(crate::pool::cached_count(11), 0);
    }

    #[test]
    fn allocation_statistics_rejects_invalid_memory_types_on_19041() {
        if std::env::var_os("XMEM_BUILD_TEST_CHILD").is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "alloc::tests::allocation_statistics_rejects_invalid_memory_types_on_19041"])
                .env("XMEM_BUILD_TEST_CHILD", "1")
                .env("TOMBOX_CONSOLE_OSBUILD", "19041")
                .env("TOMBOX_GI_OSBUILD", "22000")
                .status().unwrap();
            assert!(status.success());
            return;
        }
        assert_eq!(XMemSetAllocationHysteresis(11, 0, 1), 1);
        assert_eq!(XMemGetAllocationHysteresis(11, 0), 4 << 20);
        for (memory_type, alt, size, expected_error) in [
            (16, 0, 32, ERROR_INVALID_PARAMETER),
            (u32::MAX, 1, 32, ERROR_INVALID_PARAMETER),
            (16, 0, 31, ERROR_BAD_LENGTH),
        ] {
            let mut words = [0xa5a5_a5a5_a5a5_a5a5u64; 4];
            words[0] = (words[0] & !0xffff_ffff) | size;
            let initial = words;
            unsafe {
                SetLastError(0);
                assert_eq!(XMemGetAllocationStatistics(memory_type, alt, words.as_mut_ptr().cast()), 0);
                assert_eq!(GetLastError(), expected_error);
            }
            assert_eq!(words, initial);
        }
    }

    #[test]
    fn allocation_statistics_checks_size_field() {
        let mut s = AllocationStatistics { size: 0, pad: 0, allocated_bytes: 0, cached_bytes: 0, allocation_count: 0, released_pages: 0 };
        assert_eq!(unsafe { XMemGetAllocationStatistics(0, 0, &mut s) }, 0);
        s.size = 0x20;
        let p = XMemAllocDefault(0x2_0000, 0x1000_0000 | (5u64 << 29));
        assert_eq!(unsafe { XMemGetAllocationStatistics(5, 0, &mut s) }, 1);
        assert!(s.allocated_bytes >= 0x2_0000);
        XMemFreeDefault(p, 0);
    }
}
