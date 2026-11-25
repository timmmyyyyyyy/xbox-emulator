use super::runtime::{self, capture_flags, context, layout, singleton, thread_event_buffer, Singleton, DRIVER_TABLE_BYTES, E_FAIL, E_POINTER, E_UNEXPECTED, EXT_BLOCK_BYTES, S_FALSE};
use super::{os_build, state, wide, E_NOTIMPL, E_OUTOFMEMORY, S_OK};
use core::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

pub const PIX_GATE_DIR: &str = "c:\\pix\\";
pub const GATE_BUFFER: usize = 0x104;
pub const PIXIE_DLL: &str = ".\\d3d12_xs.dll";
pub const PIXIE_EXPORT: &[u8] = b"PixGetPixieEntryPoints\0";
pub const PIXIE_TABLE_BYTES: usize = 0x100;
pub const PIXIE_UNCHECKED: [usize; 3] = [0xa0, 0xd8, 0xe8];
pub const MSG_SIMULTANEOUS: &str = "Multple simultaneous instrumented/validated device creation not supported.\n";
pub const TIMING_CONFIG_BYTES: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitLayout {
    pub build: &'static str,
    pub entry: u32,
    pub gate: u32,
    pub driver_init: u32,
    pub table_bytes: usize,
    pub sets_overflow_alloc: bool,
    pub slots: &'static [u32],
    pub capture_slots: &'static [u32],
}

pub const SLOTS_22000: [u32; 42] = [
    0x216f0, 0x21760, 0x217a0, 0x217d0, 0x21850, 0x21880, 0x218b0, 0x21930, 0x21980, 0, 0x219b0, 0x21a10, 0x21da0, 0x21db0, 0x21e90, 0x21f90, 0x22120, 0x221d0, 0x22230, 0x22240, 0x22570,
    0x22540, 0x22580, 0x225b0, 0x218f0, 0x225e0, 0x22610, 0x22640, 0x21a70, 0x21ab0, 0x21b90, 0x21be0, 0x21c20, 0x22670, 0x226a0, 0x21c50, 0x21cb0, 0x22330, 0x22340, 0x223e0, 0x21d60, 0x21d00,
];

pub const CAPTURE_SLOTS_22000: [u32; 42] = [
    0x216f0, 0x21760, 0x217a0, 0x217d0, 0x21850, 0x21880, 0x218b0, 0x21930, 0x21980, 0x21d40, 0x219b0, 0x21a10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x21a70, 0x21ab0, 0x21b90, 0x21be0,
    0x21c20, 0x22670, 0x226a0, 0x21c50, 0x21cb0, 0, 0, 0, 0, 0x21d00,
];

pub const SLOTS_22621: [u32; 43] = [
    0x26680, 0x266f0, 0x26730, 0x26760, 0x267e0, 0x26820, 0x26850, 0x268d0, 0x26920, 0, 0x26950, 0x269b0, 0x26ca0, 0x26cb0, 0x26e20, 0x26f20, 0x270b0, 0x27160, 0x271c0, 0x271d0, 0x27470,
    0x27440, 0x27480, 0x274b0, 0x26890, 0x274e0, 0x27510, 0x27540, 0x26a10, 0x26a50, 0x26a90, 0x26ae0, 0x26b20, 0x27570, 0x275a0, 0x26b50, 0x26bb0, 0x272c0, 0x272d0, 0x27370, 0x26c60, 0x26c00,
    0x27430,
];

pub const CAPTURE_SLOTS_22621: [u32; 43] = [
    0x26680, 0x266f0, 0x26730, 0x26760, 0x267e0, 0x26820, 0x26850, 0x268d0, 0x26920, 0x26c40, 0x26950, 0x269b0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x26a10, 0x26a50, 0x26a90, 0x26ae0,
    0x26b20, 0x27570, 0x275a0, 0x26b50, 0x26bb0, 0, 0, 0, 0, 0x26c00, 0,
];

pub const INIT_22000: InitLayout = InitLayout {
    build: "22000 (XDefiant, Terraria)",
    entry: 0x228a0,
    gate: 0x227c0,
    driver_init: 0x2376c,
    table_bytes: 0x150,
    sets_overflow_alloc: true,
    slots: &SLOTS_22000,
    capture_slots: &CAPTURE_SLOTS_22000,
};

pub const INIT_22621: InitLayout = InitLayout {
    build: "22621 (Trackmania)",
    entry: 0x277a0,
    gate: 0x276c0,
    driver_init: 0x286ac,
    table_bytes: 0x158,
    sets_overflow_alloc: false,
    slots: &SLOTS_22621,
    capture_slots: &CAPTURE_SLOTS_22621,
};

pub fn init_layout(build: u32) -> Option<InitLayout> {
    match build {
        22621.. => Some(INIT_22621),
        22000.. => Some(INIT_22000),
        _ => None,
    }
}

#[link(name = "kernel32")]
extern "system" {
    fn GetVersion() -> u32;
    fn GetCurrentProcess() -> usize;
    fn K32GetProcessImageFileNameA(p: usize, buf: *mut u8, size: u32) -> u32;
    fn AreFileApisANSI() -> i32;
    fn MultiByteToWideChar(page: u32, flags: u32, input: *const u8, bytes: i32, output: *mut u16, capacity: i32) -> i32;
    fn LoadLibraryExW(n: *const u16, f: usize, flags: u32) -> usize;
    fn GetProcAddress(m: usize, n: *const u8) -> usize;
    fn OutputDebugStringW(s: *const u16);
}

fn record(what: &str, detail: &str) {
    compat_common::record(env!("CARGO_CRATE_NAME"), what, detail);
}

fn unported(slot: usize, rva: u32, what: &str) {
    record("PixDllInitialize table", &format!("slot {slot} (+{:#x}, 22000 {rva:#x}): {what} is not ported (docs/BLOCKERS.md)", slot * 8));
}

static REFS: AtomicI32 = AtomicI32::new(0);

pub fn refs() -> i32 {
    REFS.load(Ordering::SeqCst)
}

pub fn image_tail(image: &[u8]) -> Option<Vec<u8>> {
    let n = image.len();
    let start = image.iter().rposition(|&c| c == b'\\').map(|i| i + 1)?;
    if start >= n {
        return None;
    }
    let tail = &image[start..];
    if tail.len() >= GATE_BUFFER - PIX_GATE_DIR.len() {
        return None;
    }
    Some(tail.to_vec())
}

fn marker_path(image: &[u8], code_page: u32) -> Option<String> {
    let tail = image_tail(image)?;
    let mut wide = [0u16; GATE_BUFFER];
    let count = unsafe { MultiByteToWideChar(code_page, 0, tail.as_ptr(), tail.len() as i32, wide.as_mut_ptr(), wide.len() as i32) };
    if count == 0 {
        return None;
    }
    Some(format!("{PIX_GATE_DIR}{}", String::from_utf16(&wide[..count as usize]).ok()?))
}

#[cfg(test)]
#[test]
fn marker_uses_the_file_api_code_page() {
    assert_eq!(marker_path(b"\\Device\\Caf\xe9.exe", 1252).as_deref(), Some("c:\\pix\\Café.exe"));
    assert_eq!(marker_path(b"\\Device\\Caf\x82.exe", 437).as_deref(), Some("c:\\pix\\Café.exe"));
    assert_eq!(marker_path("\\Device\\Café.exe".as_bytes(), 65001).as_deref(), Some("c:\\pix\\Café.exe"));
}

pub fn gate_path() -> Option<String> {
    let mut buf = [0u8; GATE_BUFFER];
    let n = unsafe { K32GetProcessImageFileNameA(GetCurrentProcess(), buf.as_mut_ptr(), GATE_BUFFER as u32) } as usize;
    let file_code_page = if unsafe { AreFileApisANSI() } != 0 { 0 } else { 1 };
    marker_path(&buf[..n.min(GATE_BUFFER)], file_code_page)
}

pub fn gate_host_path() -> Option<std::path::PathBuf> {
    let p = gate_path()?;
    console_sandbox::fs::try_resolve(&p).ok().map(|(_, h)| h)
}

pub fn gate() -> bool {
    gate_host_path().is_some_and(|h| h.symlink_metadata().is_ok())
}

pub unsafe fn pixie_entry_points() -> bool {
    let m = LoadLibraryExW(wide(PIXIE_DLL).as_ptr(), 0, 0);
    if m == 0 {
        return false;
    }
    let f = GetProcAddress(m, PIXIE_EXPORT.as_ptr());
    if f == 0 {
        return false;
    }
    let mut t = [0u64; PIXIE_TABLE_BYTES / 8];
    t[0] = PIXIE_TABLE_BYTES as u64;
    let get: unsafe extern "system" fn(*mut u8) -> i32 = core::mem::transmute(f);
    if get(t.as_mut_ptr() as *mut u8) < 0 {
        return false;
    }
    (1..PIXIE_TABLE_BYTES / 8).filter(|i| !PIXIE_UNCHECKED.contains(&(i * 8))).all(|i| t[i] != 0)
}

unsafe fn instrument_init(s: &Singleton) -> i32 {
    let i = &s.instrument;
    if i.started.load(Ordering::SeqCst) {
        OutputDebugStringW(wide(MSG_SIMULTANEOUS).as_ptr());
        return E_NOTIMPL;
    }
    if !pixie_entry_points() {
        return E_UNEXPECTED;
    }
    i.started.store(true, Ordering::SeqCst);
    record(
        "PixDllInitialize",
        "instrumented device init (a5 != 0): .\\d3d12_xs.dll PixGetPixieEntryPoints passed 0x2c64; the rest of 0x3c20 (pixie open 0x3ca2, 0x2fd8 XMemAlloc/XMemVirtualAlloc regions, driver-table copy +0x80c68), 0x300a4/0x3151c (NtDirectGraphicsCall 0x3764c) and the 0x176d8/0x1764c InitOnce worker are not ported",
    );
    E_NOTIMPL
}

#[allow(clippy::too_many_arguments)]
unsafe fn driver_init(s: &'static Singleton, l: &InitLayout, magic: u32, version: u32, flags: u32, a4: u32, a5: u32, a6: u32, table: *const u8, bytes: u32) -> i32 {
    if l.sets_overflow_alloc {
        state().overflow_alloc.store(true, Ordering::SeqCst);
    }
    let rt = {
        let mut r = s.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if r.is_none() {
            if s.constructing.load(Ordering::SeqCst) {
                record("PixDllInitialize", "an earlier runtime allocation failed and left the construct flag set; the console writes through the null runtime here (0x23822)");
                return E_OUTOFMEMORY;
            }
            s.constructing.store(true, Ordering::SeqCst);
            match runtime::runtime_new(&s.layout, magic, version, flags) {
                Some(rt) => {
                    *r = Some(rt);
                    s.constructing.store(false, Ordering::SeqCst);
                }
                None => return E_OUTOFMEMORY,
            }
        }
        r.unwrap()
    };
    rt.instrument_mode.store(a5, Ordering::SeqCst);
    let copy = {
        let mut d = s.driver_table.lock().unwrap_or_else(|e| e.into_inner());
        if !table.is_null() {
            let n = (bytes as usize).min(DRIVER_TABLE_BYTES);
            core::ptr::copy_nonoverlapping(table, d.as_mut_ptr(), n);
        }
        d.as_ptr() as usize
    };
    rt.driver_table.store(copy, Ordering::SeqCst);
    s.magic.store(magic, Ordering::SeqCst);
    s.arg4.store(a4, Ordering::SeqCst);
    s.arg5.store(a5, Ordering::SeqCst);
    s.arg6.store(a6, Ordering::SeqCst);
    if a5 == 0 {
        return S_OK;
    }
    instrument_init(s)
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn initialize(magic: u32, version: u32, flags: u32, a4: u32, a5: u32, a6: u32, driver_table: *const u8, driver_bytes: u32, out: *mut c_void, out_bytes: u32) -> i32 {
    GetVersion();
    let (Some(l), Some(rl)) = (init_layout(os_build()), layout(os_build())) else {
        record("PixDllInitialize", "this console build's pixrt10 (19041: 0xae8-byte singleton at 0x19530, driver init 0x1a238) is not ported");
        return E_NOTIMPL;
    };
    if flags & 1 != 0 && !gate() {
        return E_NOTIMPL;
    }
    context();
    let (s, hr) = {
        let mut g = runtime::init_lock();
        let s = match runtime::create_singleton(&mut g, rl) {
            Ok(s) => s,
            Err(e) => return e,
        };
        REFS.fetch_add(1, Ordering::SeqCst);
        (s, driver_init(s, &l, magic, version, flags, a4, a5, a6, driver_table, driver_bytes))
    };
    if hr < 0 {
        return hr;
    }
    let t = table(&l, s.capturing.load(Ordering::SeqCst));
    if !out.is_null() {
        let n = (out_bytes as usize).min(t.len() * 8);
        core::ptr::copy_nonoverlapping(t.as_ptr() as *const u8, out as *mut u8, n);
    }
    S_OK
}

pub fn slot_fns() -> [usize; 43] {
    [
        teardown as *const () as usize,
        slot1 as *const () as usize,
        slot2 as *const () as usize,
        slot3 as *const () as usize,
        slot4 as *const () as usize,
        slot5 as *const () as usize,
        slot6 as *const () as usize,
        slot7 as *const () as usize,
        slot8 as *const () as usize,
        slot9 as *const () as usize,
        slot10 as *const () as usize,
        slot11 as *const () as usize,
        slot12 as *const () as usize,
        slot13 as *const () as usize,
        slot14 as *const () as usize,
        slot15 as *const () as usize,
        slot16 as *const () as usize,
        slot17 as *const () as usize,
        slot18 as *const () as usize,
        slot19 as *const () as usize,
        thread_event_buffer as *const () as usize,
        capture_flags as *const () as usize,
        slot22 as *const () as usize,
        slot23 as *const () as usize,
        slot24 as *const () as usize,
        slot25 as *const () as usize,
        slot26 as *const () as usize,
        slot27 as *const () as usize,
        slot28 as *const () as usize,
        slot29 as *const () as usize,
        slot30 as *const () as usize,
        slot31 as *const () as usize,
        slot32 as *const () as usize,
        slot33 as *const () as usize,
        slot34 as *const () as usize,
        slot35 as *const () as usize,
        slot36 as *const () as usize,
        slot37 as *const () as usize,
        slot38 as *const () as usize,
        slot39 as *const () as usize,
        slot40 as *const () as usize,
        slot41 as *const () as usize,
        slot42 as *const () as usize,
    ]
}

pub fn table(l: &InitLayout, capturing: bool) -> Vec<usize> {
    let f = slot_fns();
    let rvas = if capturing { l.capture_slots } else { l.slots };
    rvas.iter().enumerate().map(|(i, &r)| if r == 0 { 0 } else { f[i] }).collect()
}

fn sg() -> Option<&'static Singleton> {
    singleton()
}

pub unsafe extern "system" fn teardown() {
    let Some(s) = sg() else { return };
    if REFS.fetch_sub(1, Ordering::SeqCst) != 1 {
        return;
    }
    *s.driver_table.lock().unwrap_or_else(|e| e.into_inner()) = [0; DRIVER_TABLE_BYTES];
    s.arg4.store(0, Ordering::SeqCst);
    s.arg5.store(0, Ordering::SeqCst);
    s.arg6.store(0, Ordering::SeqCst);
    unported(0, 0x216f0, "the last release: 0x241c0 destroys the runtime through vtable+0xb8 (threads, Xbtp listener) and frees the singleton; the emulator keeps both");
}

pub unsafe extern "system" fn slot1(_a: usize, _b: usize, _c: usize) -> i32 {
    let Some(_s) = sg() else { return E_UNEXPECTED };
    if runtime::runtime().is_none() {
        return E_FAIL;
    }
    unported(1, 0x21760, "runtime 0x1bff8");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot2(_a: usize, _b: u32) -> i32 {
    if sg().is_none() {
        return E_UNEXPECTED;
    }
    unported(2, 0x217a0, "0x2159c");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot3(_a: i32) -> i32 {
    if sg().is_none() {
        return E_UNEXPECTED;
    }
    unported(3, 0x217d0, "0x5898 on +0x128 and runtime 0x1c720/0x1bed8");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot4(_a: u32, _b: usize) -> i32 {
    if sg().is_none() {
        return E_UNEXPECTED;
    }
    unported(4, 0x21850, "0xd2f4 (types 1, 6..0x16 at 0xd3c0, others at 0xd4f0)");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot5(kind: u32, _p: usize) -> i32 {
    let Some(s) = sg() else { return E_UNEXPECTED };
    let i = &s.instrument;
    i.acquire();
    let hr = if kind != 1 {
        E_FAIL
    } else if i.instrumenting() || i.pending.load(Ordering::SeqCst) {
        unported(5, 0xd6c4, "the instrumented branch of 0xd64c");
        E_NOTIMPL
    } else {
        unported(5, 0xda47, "the forward through driver-table copy +0x80c68 offset 0x40 (the copy is made only by the unported a5 != 0 init; with a5 == 0 the console calls uninitialised memory)");
        E_NOTIMPL
    };
    i.release();
    hr
}

unsafe fn dae4(slot: usize, rva: u32) -> i32 {
    let Some(s) = sg() else { return E_UNEXPECTED };
    let i = &s.instrument;
    i.acquire();
    let hr = if i.instrumenting() {
        unported(slot, rva, "the instrumented branch of 0xdae4");
        E_NOTIMPL
    } else {
        S_FALSE
    };
    i.release();
    hr
}

pub unsafe extern "system" fn slot6(_p: usize, _b: u8) -> i32 {
    dae4(6, 0x218b0)
}

pub unsafe extern "system" fn slot24(_p: usize, _b: u8) -> i32 {
    dae4(24, 0x218f0)
}

pub unsafe extern "system" fn slot7(_a: usize, _b: usize, _c: u32, _d: usize) -> i32 {
    match sg() {
        Some(s) if s.instrument.active.load(Ordering::SeqCst) => {
            unported(7, 0x21930, "0xf8c4");
            E_NOTIMPL
        }
        _ => E_UNEXPECTED,
    }
}

pub unsafe extern "system" fn slot8() -> i32 {
    match sg() {
        None => E_UNEXPECTED,
        Some(s) if s.instrument.active.load(Ordering::SeqCst) => E_FAIL,
        Some(_) => E_UNEXPECTED,
    }
}

pub unsafe extern "system" fn slot9() -> i32 {
    if sg().is_some() {
        E_NOTIMPL
    } else {
        E_UNEXPECTED
    }
}

pub unsafe extern "system" fn slot10() -> i32 {
    if sg().is_none() {
        return E_UNEXPECTED;
    }
    unported(10, 0x219b0, "0x2c134(0x887a0006) while capturing and runtime 0x1b368 (resets +0x638 and both transports via 0x3b264)");
    E_NOTIMPL
}

unsafe fn session_slot(slot: usize, rva: u32) -> i32 {
    let Some(rt) = sg().and(runtime::runtime()) else { return E_UNEXPECTED };
    if rt.capture_session.load(Ordering::SeqCst) == 0 || rt.session.load(Ordering::SeqCst) == 0 {
        return S_FALSE;
    }
    unported(slot, rva, "the capture-session call");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot11(_a: u32, _b: usize) -> i32 {
    session_slot(11, 0x21a10)
}

pub unsafe extern "system" fn slot35(_a: u32, _b: usize) -> i32 {
    session_slot(35, 0x21c50)
}

pub unsafe extern "system" fn slot12() -> i32 {
    S_OK
}

static TIMING_CONFIG: Mutex<[u8; TIMING_CONFIG_BYTES]> = Mutex::new([0; TIMING_CONFIG_BYTES]);

pub fn timing_config() -> [u8; TIMING_CONFIG_BYTES] {
    *TIMING_CONFIG.lock().unwrap_or_else(|e| e.into_inner())
}

pub unsafe extern "system" fn slot13(cfg: *const u8, out: *mut u8) -> i32 {
    if cfg.is_null() || out.is_null() {
        return E_POINTER;
    }
    let mut c = [0u8; TIMING_CONFIG_BYTES];
    core::ptr::copy_nonoverlapping(cfg, c.as_mut_ptr(), TIMING_CONFIG_BYTES);
    *TIMING_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = c;
    context();
    unported(13, 0x21db0, "the timing-capture start 0x291b4, 0x2d7dc and 0x250d4");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot14() {
    *TIMING_CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = [0; TIMING_CONFIG_BYTES];
}

pub unsafe extern "system" fn slot15(_b: u8) -> usize {
    unported(15, 0x21f90, "the default event buffer 0x25e94 (0x28d5c XMemVirtualAlloc block) and the capture-active event block");
    0
}

pub unsafe extern "system" fn slot16(_b: u8) -> usize {
    unported(16, 0x22120, "the default event buffer 0x25e94 and the capture-active event block");
    0
}

pub unsafe extern "system" fn slot17(_b: usize) {
    let c = context();
    if c.pool.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
        return;
    }
    unported(17, 0x221d0, "the block return against the default buffer 0x25e94");
}

pub unsafe extern "system" fn slot18() {}

unsafe fn stream(slot: usize, rva: u32, i: usize) {
    let g = context().streams[i].lock().unwrap_or_else(|e| e.into_inner());
    if g.is_some() {
        unported(slot, rva, "the write into a capture stream buffer");
    }
}

pub unsafe extern "system" fn slot19(p: *const u64, n: usize) {
    if p.is_null() || n == 0 {
        return;
    }
    stream(19, 0x22240, 0)
}

pub unsafe extern "system" fn slot22(v: u16) -> i32 {
    state().words[1].swap(v, Ordering::SeqCst);
    S_OK
}

pub unsafe extern "system" fn slot23(v: u16) -> i32 {
    state().words[0].swap(v, Ordering::SeqCst);
    S_OK
}

unsafe fn forward(slot: usize, rva: u32, offset: usize) -> i32 {
    let Some(s) = sg() else { return E_UNEXPECTED };
    let i = &s.instrument;
    i.acquire();
    if i.instrumenting() {
        unported(slot, rva, "the instrumented branch");
    } else {
        unported(slot, rva, &format!("the forward through driver-table copy +0x80c68 offset {offset:#x} (made only by the unported a5 != 0 init; with a5 == 0 the console calls uninitialised memory)"));
    }
    i.release();
    E_NOTIMPL
}

pub unsafe extern "system" fn slot25(_out: *mut i32, _desc: usize) -> i32 {
    forward(25, 0x225e0, 0x98)
}

pub unsafe extern "system" fn slot26(_out: *mut i32, _desc: usize) -> i32 {
    forward(26, 0x22610, 0xa0)
}

pub unsafe extern "system" fn slot27(_out: *mut i32, _desc: usize) -> i32 {
    forward(27, 0x22640, 0xa8)
}

unsafe fn plain(slot: usize, rva: u32, what: &str) -> i32 {
    if sg().is_none() {
        return E_UNEXPECTED;
    }
    unported(slot, rva, what);
    E_NOTIMPL
}

pub unsafe extern "system" fn slot28(_a: usize) -> i32 {
    plain(28, 0x21a70, "0x45df4 on +0x130")
}

pub unsafe extern "system" fn slot29(_a: usize, _b: u32, _c: usize) -> i32 {
    plain(29, 0x21ab0, "0x45bb4 on +0x130")
}

pub unsafe extern "system" fn slot30(_a: usize, _b: u32, _c: usize) -> i32 {
    plain(30, 0x21b90, "+0x128 vtable slot 1")
}

pub unsafe extern "system" fn slot31() -> i32 {
    plain(31, 0x21be0, "0x459a8 on +0x130")
}

pub unsafe extern "system" fn slot32(_a: usize, _b: usize) -> i32 {
    plain(32, 0x21c20, "0x23ccc")
}

pub unsafe extern "system" fn slot33(_a: usize) -> i32 {
    plain(33, 0x22670, "0x11b5c on +0x128")
}

pub unsafe extern "system" fn slot34(_a: usize) -> i32 {
    let Some(s) = sg() else { return E_UNEXPECTED };
    let i = &s.instrument;
    i.acquire();
    let hr = if i.instrumenting() {
        unported(34, 0x122d3, "the instrumented branch of 0x1227c");
        E_NOTIMPL
    } else {
        S_OK
    };
    i.release();
    hr
}

pub unsafe extern "system" fn slot36(_a: u32, _b: usize, _c: usize, _d: usize, _e: usize) -> i32 {
    if sg().and(runtime::runtime()).is_none() {
        return E_UNEXPECTED;
    }
    unported(36, 0x21cb0, "runtime 0x1df28");
    E_NOTIMPL
}

pub unsafe extern "system" fn slot37(p: *const u32, _name: *const u16, _b: u8) {
    if p.is_null() || !matches!(*p, 0x14 | 0xf) {
        return;
    }
    unported(37, 0x2cbb4, "the 0x14/0xf record write under the +0x540 stream lock");
}

pub unsafe extern "system" fn slot38(p: usize) {
    if p == 0 {
        return;
    }
    stream(38, 0x22340, 1)
}

pub unsafe extern "system" fn slot39(p: *const u16) {
    if p.is_null() {
        return;
    }
    stream(39, 0x223e0, 2)
}

pub unsafe extern "system" fn slot40(p: *const u8) {
    let Some(s) = sg() else { return };
    let mut b = s.ext_block.lock().unwrap_or_else(|e| e.into_inner());
    core::ptr::copy_nonoverlapping(p, b.as_mut_ptr(), EXT_BLOCK_BYTES);
}

pub unsafe extern "system" fn slot41() -> u8 {
    if sg().is_none() {
        return 1;
    }
    match runtime::runtime() {
        Some(rt) => (rt.device_5bc.load(Ordering::SeqCst) != 0 && rt.device_5b8.load(Ordering::SeqCst) == 0) as u8,
        None => 0,
    }
}

pub unsafe extern "system" fn slot42(_a: usize) -> i32 {
    unported(42, 0x27430, "22621 0x332dc");
    E_NOTIMPL
}
