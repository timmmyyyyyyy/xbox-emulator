#![allow(non_snake_case)]
use core::ffi::c_void;

#[cfg(not(pixrt_classic))]
pub mod driver;
#[cfg(not(pixrt_classic))]
pub mod etw;
#[cfg(not(pixrt_classic))]
pub mod runtime;
#[cfg(not(pixrt_classic))]
pub mod session;
#[cfg(not(pixrt_classic))]
pub mod transport_stream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

pub const E_OUTOFMEMORY: i32 = 0x8007_000Eu32 as i32;
pub const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
pub const S_OK: i32 = 0;
pub const XVCMGMT: &str = "\\\\.\\xvcmgmt";
pub const IOCTL_OVERFLOW: u32 = 0x0015_0068;
pub const TOOLS_HEAP_FLAGS: u64 = 0x8001;
pub const OVERFLOW_ATTRIBUTES: u64 = 0x008e_0000;
pub const LOW_TOOLS_MEMORY: u64 = 0x2000_0000;
pub const HEAP_NAME: &[u8; 8] = b"PIXTOOL\0";
pub const DBGBREAK_FILE: &[u8] = b"d:\\dbgbreakonnotoolingmemory\0";
pub const EXPORTS_TXT: &str = include_str!(env!("PIXRT_EXPORTS"));
pub const DLL_FILE: &str = concat!(env!("CARGO_CRATE_NAME"), ".dll");
pub const CLASSIC: bool = cfg!(pixrt_classic);

pub const MSG_LOW: &str = "PIX: Less than 512 MB of tooling memory is available.\n";
pub const MSG_UNAVAILABLE: &str = "PIX: Tooling memory unavailable.\n";
pub const MSG_NO_OVERFLOW: &str = "PIX: Tooling memory overflow is disabled.\n";
pub const MSG_SOME_FEATURES: &str = "PIX: Some features may not work with current tooling memory configuration.\n";
pub const MSG_NO_HEAP: &str = "PIX: Failed to create tooling memory heap.\n";
pub const MSG_QUERY_FAILED: &str = "PIX: failed to query tooling memory overflow config setting.\n";
pub const MSG_ASSUME_OVERFLOW: &str = "PIX: assuming tooling memory overflow allowed.\n";

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(n: *const u16, a: u32, s: u32, sa: *mut c_void, d: u32, f: u32, t: *mut c_void) -> isize;
    fn DeviceIoControl(h: isize, code: u32, i: *mut c_void, il: u32, o: *mut c_void, ol: u32, ret: *mut u32, ov: *mut c_void) -> i32;
    fn CloseHandle(h: isize) -> i32;
    fn OutputDebugStringW(s: *const u16);
    fn GetFileAttributesA(n: *const u8) -> u32;
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlAllocateHeap(h: *mut c_void, flags: u32, size: usize) -> *mut c_void;
    fn RtlFreeHeap(h: *mut c_void, flags: u32, p: *mut c_void) -> u8;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(n: *const u16) -> *mut c_void;
    fn LoadLibraryW(n: *const u16) -> *mut c_void;
    fn GetProcAddress(m: *mut c_void, n: *const u8) -> *mut c_void;
}

pub fn xmem_module() -> usize {
    static M: OnceLock<usize> = OnceLock::new();
    *M.get_or_init(|| unsafe {
        let n = wide("xmem.dll");
        let h = GetModuleHandleW(n.as_ptr());
        if !h.is_null() {
            return h as usize;
        }
        LoadLibraryW(n.as_ptr()) as usize
    })
}

fn xmem_fn(name: &[u8]) -> usize {
    let m = xmem_module();
    if m == 0 {
        return 0;
    }
    unsafe { GetProcAddress(m as *mut c_void, name.as_ptr()) as usize }
}

unsafe fn XMemCreateWin32Heap(flags: u64, base: *mut c_void) -> *mut c_void {
    match xmem_fn(b"XMemCreateWin32Heap\0") {
        0 => core::ptr::null_mut(),
        f => core::mem::transmute::<usize, unsafe extern "system" fn(u64, *mut c_void) -> *mut c_void>(f)(flags, base),
    }
}

unsafe fn XMemSetWin32HeapName(heap: *mut c_void, name: *const u8, len: usize) -> i32 {
    match xmem_fn(b"XMemSetWin32HeapName\0") {
        0 => 0,
        f => core::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void, *const u8, usize) -> i32>(f)(heap, name, len),
    }
}

unsafe fn XMemGetWorkingSetStatistics(flags: u32, out: *mut c_void) -> i32 {
    match xmem_fn(b"XMemGetWorkingSetStatistics\0") {
        0 => 0,
        f => core::mem::transmute::<usize, unsafe extern "system" fn(u32, *mut c_void) -> i32>(f)(flags, out),
    }
}

unsafe fn XMemAlloc(size: usize, attributes: u64) -> *mut c_void {
    match xmem_fn(b"XMemAlloc\0") {
        0 => core::ptr::null_mut(),
        f => core::mem::transmute::<usize, unsafe extern "system" fn(usize, u64) -> *mut c_void>(f)(size, attributes),
    }
}

unsafe fn XMemFree(p: *mut c_void, attributes: u64) {
    if let f @ 1.. = xmem_fn(b"XMemFree\0") {
        core::mem::transmute::<usize, unsafe extern "system" fn(*mut c_void, u64)>(f)(p, attributes)
    }
}

extern "C" {
    fn malloc(n: usize) -> *mut c_void;
    fn free(p: *mut c_void);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    pub assume_overflow_without_device: bool,
    pub hypervisor_check: bool,
    pub name_heap: bool,
    pub stats_flags: u32,
    pub stats_bytes: usize,
    pub dbgbreak: bool,
}

pub fn variant(classic: bool, os_build: u32) -> Variant {
    let new = os_build >= 22000;
    Variant {
        assume_overflow_without_device: !classic,
        hypervisor_check: !classic && new,
        name_heap: new,
        stats_flags: if new { 0x2000_8002 } else { 0x1000_8002 },
        stats_bytes: if new { 0x78 } else { 0x50 },
        dbgbreak: !classic && new,
    }
}

pub struct ToolState {
    pub heap: usize,
    pub overflow: bool,
    pub overflow_used: std::sync::atomic::AtomicBool,
    pub overflow_alloc: std::sync::atomic::AtomicBool,
    pub words: [std::sync::atomic::AtomicU16; 6],
    pub tools_bytes: u64,
    pub messages: Vec<&'static str>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn overflow_probe() -> Option<bool> {
    let n = wide(XVCMGMT);
    let h = CreateFileW(n.as_ptr(), 0xC000_0000, 0, core::ptr::null_mut(), 3, 0, core::ptr::null_mut());
    if h == -1 {
        return None;
    }
    let mut out = 0u64;
    let mut ret = 0u32;
    let ok = DeviceIoControl(h, IOCTL_OVERFLOW, core::ptr::null_mut(), 0, &mut out as *mut u64 as *mut c_void, 8, &mut ret, core::ptr::null_mut()) != 0;
    CloseHandle(h);
    Some(ok && out & 1 != 0)
}

fn hypervisor_forces_overflow() -> bool {
    let r = core::arch::x86_64::__cpuid_count(0x4000_000d, 0);
    r.ecx as u16 == 0x60
}

fn dbgbreak_if_flagged(v: &Variant) {
    if v.dbgbreak && unsafe { GetFileAttributesA(DBGBREAK_FILE.as_ptr()) } != u32::MAX {
        unsafe { core::arch::asm!("int3") };
    }
}

pub fn build_state(v: &Variant) -> ToolState {
    let mut messages = Vec::new();
    let mut overflow = match unsafe { overflow_probe() } {
        Some(b) => b,
        None if v.assume_overflow_without_device => {
            messages.push(MSG_QUERY_FAILED);
            messages.push(MSG_ASSUME_OVERFLOW);
            true
        }
        None => false,
    };
    if v.hypervisor_check && hypervisor_forces_overflow() {
        overflow = true;
    }
    let heap = unsafe { XMemCreateWin32Heap(TOOLS_HEAP_FLAGS, core::ptr::null_mut()) } as usize;
    if heap != 0 && v.name_heap {
        unsafe { XMemSetWin32HeapName(heap as *mut c_void, HEAP_NAME.as_ptr(), 8) };
    }
    let mut stats = vec![0u8; v.stats_bytes];
    let ok = unsafe { XMemGetWorkingSetStatistics(v.stats_flags, stats.as_mut_ptr() as *mut c_void) } != 0;
    let size = if ok { u64::from_le_bytes(stats[0x10..0x18].try_into().unwrap()) } else { 0 };
    if size != 0 {
        if size < LOW_TOOLS_MEMORY {
            messages.push(MSG_LOW);
        }
    } else {
        dbgbreak_if_flagged(v);
        messages.push(MSG_UNAVAILABLE);
    }
    if !overflow {
        messages.push(MSG_NO_OVERFLOW);
    }
    if size < LOW_TOOLS_MEMORY && !overflow {
        messages.push(MSG_SOME_FEATURES);
    }
    if heap == 0 {
        dbgbreak_if_flagged(v);
        messages.push(MSG_NO_HEAP);
    }
    for m in &messages {
        unsafe { OutputDebugStringW(wide(m).as_ptr()) };
    }
    ToolState { heap, overflow, overflow_used: std::sync::atomic::AtomicBool::new(false), overflow_alloc: std::sync::atomic::AtomicBool::new(false), words: [const { std::sync::atomic::AtomicU16::new(0) }; 6], tools_bytes: size, messages }
}

pub fn os_build() -> u32 {
    compat_common::os_host::k32legacy::console_os_build()
}

pub fn state() -> &'static ToolState {
    static S: OnceLock<ToolState> = OnceLock::new();
    S.get_or_init(|| {
        let s = build_state(&variant(CLASSIC, os_build()));
        compat_common::record(
            env!("CARGO_CRATE_NAME"),
            "tooling memory",
            &format!("heap={:#x} overflow={} tools_bytes={:#x}; {}", s.heap, s.overflow, s.tools_bytes, s.messages.concat().replace('\n', " ")),
        );
        s
    })
}

pub fn guard_fails(s: &ToolState) -> bool {
    s.heap == 0 && !s.overflow
}

static MALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

pub fn malloc_bytes() -> u64 {
    MALLOC_BYTES.load(Ordering::Relaxed)
}

pub unsafe extern "system" fn tool_alloc(size: usize) -> *mut c_void {
    let s = state();
    let total = size.wrapping_add(8);
    if s.heap != 0 {
        let p = RtlAllocateHeap(s.heap as *mut c_void, 0, total) as *mut u64;
        if !p.is_null() {
            *p = 1;
            return p.add(1) as *mut c_void;
        }
    }
    let overflow_alloc = s.overflow_alloc.load(Ordering::Relaxed);
    if s.overflow {
        if overflow_alloc {
            let p = XMemAlloc(total, OVERFLOW_ATTRIBUTES) as *mut u64;
            if p.is_null() {
                return core::ptr::null_mut();
            }
            *p = 2;
            s.overflow_used.store(true, Ordering::Relaxed);
            return p.add(1) as *mut c_void;
        }
    } else if overflow_alloc {
        return core::ptr::null_mut();
    }
    if size == 0 {
        return core::ptr::null_mut();
    }
    let p = malloc(total) as *mut u64;
    if p.is_null() {
        return core::ptr::null_mut();
    }
    MALLOC_BYTES.fetch_add(total as u64, Ordering::Relaxed);
    *p = 3;
    p.add(1) as *mut c_void
}

pub unsafe extern "system" fn tool_free(p: *mut c_void) {
    let s = state();
    if p.is_null() {
        return;
    }
    let block = (p as *mut u64).sub(1);
    match *block {
        1 => {
            RtlFreeHeap(s.heap as *mut c_void, 0, block as *mut c_void);
        }
        2 => XMemFree(block as *mut c_void, OVERFLOW_ATTRIBUTES),
        3 => free(block as *mut c_void),
        _ => {}
    }
}

pub fn tool_table() -> [usize; 2] {
    [tool_alloc as *const () as usize, tool_free as *const () as usize]
}

unsafe fn copy_table(dst: *mut c_void, src: &[usize; 2], bytes: u32) {
    if dst.is_null() {
        return;
    }
    let n = (bytes as usize).min(core::mem::size_of_val(src));
    core::ptr::copy_nonoverlapping(src.as_ptr() as *const u8, dst as *mut u8, n);
}

#[cfg(pixrt_classic)]
fn unreproduced(api: &str) -> i32 {
    compat_common::record("pixrt", api, "tooling memory present: the console's PIX capture runtime is not reproduced (docs/BLOCKERS.md); E_NOTIMPL");
    E_NOTIMPL
}

#[cfg(pixrt_classic)]
#[no_mangle]
pub unsafe extern "system" fn PixDllInitialize(_a: u32, _b: u32, _c: u32, _d: u32, _e: u32, _f: u32, _table: *mut c_void, _bytes: u32) -> i32 {
    if guard_fails(state()) {
        return E_OUTOFMEMORY;
    }
    unreproduced("PixDllInitialize")
}

#[cfg(pixrt_classic)]
#[no_mangle]
pub unsafe extern "system" fn PixDllInitializeFromTitle(_magic: u32, _version: u32, _flags: u32, _table: *mut c_void, _bytes: u32) -> i32 {
    if guard_fails(state()) {
        return E_OUTOFMEMORY;
    }
    unreproduced("PixDllInitializeFromTitle")
}

#[no_mangle]
pub unsafe extern "system" fn PixDllInitializeToolMemory(_magic: u32, _version: u32, flags: u32, table: *mut c_void, bytes: u32) -> i32 {
    if CLASSIC {
        if guard_fails(state()) {
            return E_OUTOFMEMORY;
        }
        copy_table(table, &tool_table(), bytes);
    } else {
        let t = if flags & 1 != 0 { [0usize; 2] } else { tool_table() };
        copy_table(table, &t, bytes);
    }
    S_OK
}
