use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use compat_common::record;

const XBOX_SLOT_RESET: usize = 10;
const XBOX_SLOT_CLEAR_STATE: usize = 11;
const XBOX_SLOT_SET_PSO: usize = 25;
const XBOX_SLOT_DISPATCH: usize = 14;
const PC_SET_MARKER: usize = 56;
pub const PREFIX: &str = "tbpso ";
pub const DIMS: &str = "tbdispatch ";
const OP_SETMARKER: u32 = 0;
const OP_DISPATCH: u32 = 6;
const KEEP_RECORDINGS: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hashes {
    pub vs: u64,
    pub ps: u64,
    pub cs: u64,
    pub other: u64,
}

static PREV_SET: AtomicUsize = AtomicUsize::new(0);
static PREV_RESET: AtomicUsize = AtomicUsize::new(0);
static PREV_CLEAR: AtomicUsize = AtomicUsize::new(0);
static PREV_DISPATCH: AtomicUsize = AtomicUsize::new(0);
static PREV_DISPATCH_X: AtomicUsize = AtomicUsize::new(0);
static MARKS: AtomicUsize = AtomicUsize::new(0);

fn table() -> &'static Mutex<HashMap<usize, (Hashes, bool)>> {
    static T: OnceLock<Mutex<HashMap<usize, (Hashes, bool)>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn recordings() -> &'static Mutex<HashMap<usize, VecDeque<Vec<String>>>> {
    static R: OnceLock<Mutex<HashMap<usize, VecDeque<Vec<String>>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn begin_recording(list: usize) {
    if enabled() {
        begin_recording_now(list);
    }
}

fn begin_recording_now(list: usize) {
    let mut r = recordings().lock().unwrap_or_else(|e| e.into_inner());
    let q = r.entry(list).or_default();
    q.push_back(Vec::new());
    while q.len() > KEEP_RECORDINGS {
        q.pop_front();
    }
}

fn note_marker(list: usize, s: String) {
    let mut r = recordings().lock().unwrap_or_else(|e| e.into_inner());
    let q = r.entry(list).or_default();
    if q.is_empty() {
        q.push_back(Vec::new());
    }
    q.back_mut().unwrap().push(s);
}

pub fn recorded(list: usize) -> Vec<Vec<String>> {
    recordings().lock().unwrap_or_else(|e| e.into_inner()).get(&list).map(|q| q.iter().cloned().collect()).unwrap_or_default()
}

pub fn fallback_contexts(list: usize, hist: &[u32], used: &mut Vec<(usize, usize)>) -> (Vec<(u32, String)>, String) {
    let want = hist.iter().filter(|&&o| o == OP_SETMARKER).count();
    let recs = recorded(list);
    if want == 0 {
        return (Vec::new(), "no SETMARKER ops in this list".into());
    }
    for (k, r) in recs.iter().enumerate() {
        if r.len() == want && !used.contains(&(list, k)) {
            used.push((list, k));
            return (from_side_table(hist, r), format!("contexts from the side table: recording {} of {} kept for this list ({want} markers)", k + 1, recs.len()));
        }
    }
    let lens: Vec<usize> = recs.iter().map(|r| r.len()).collect();
    (Vec::new(), format!("no breadcrumb contexts and no kept recording with {want} markers (kept: {lens:?}); other code writes markers into this list"))
}

pub fn enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| crate::dred::mode() == crate::dred::Mode::Markers)
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

unsafe fn blob_hash(p: u64, n: u64) -> u64 {
    if p == 0 || n == 0 || n > 0x40_0000 {
        return 0;
    }
    crate::shader_xlat::fnv1a(unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) })
}

pub unsafe fn gfx_hashes(desc: *const u8) -> Hashes {
    if desc.is_null() {
        return Hashes::default();
    }
    let d = unsafe { core::slice::from_raw_parts(desc, 0x58) };
    let h = |o: usize| unsafe { blob_hash(rd64(d, o), rd64(d, o + 8)) };
    Hashes { vs: h(0x08), ps: h(0x18), cs: 0, other: h(0x28) ^ h(0x38).rotate_left(21) ^ h(0x48).rotate_left(42) }
}

pub unsafe fn compute_hashes(desc: *const u8) -> Hashes {
    if desc.is_null() {
        return Hashes::default();
    }
    let d = unsafe { core::slice::from_raw_parts(desc, 0x18) };
    Hashes { cs: unsafe { blob_hash(rd64(d, 0x08), rd64(d, 0x10)) }, ..Hashes::default() }
}

pub fn note(pso: *mut c_void, h: Hashes, placeholder: bool) {
    if !enabled() || pso.is_null() {
        return;
    }
    table().lock().unwrap_or_else(|e| e.into_inner()).insert(pso as usize, (h, placeholder));
}

pub fn inherit(new: *mut c_void, base: *mut c_void) {
    if !enabled() || new.is_null() {
        return;
    }
    let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(h, _)) = t.get(&(base as usize)) {
        t.insert(new as usize, (h, false));
    }
}

pub fn hashes(pso: usize) -> Option<Hashes> {
    if !enabled() {
        return None;
    }
    table().lock().unwrap_or_else(|e| e.into_inner()).get(&pso).map(|e| e.0)
}

pub fn forget(pso: usize) {
    if !enabled() {
        return;
    }
    table().lock().unwrap_or_else(|e| e.into_inner()).remove(&pso);
}

pub fn text(pso: usize, entry: Option<(Hashes, bool)>) -> String {
    let Some((h, ph)) = entry else {
        return if pso == 0 { format!("{PREFIX}none") } else { format!("{PREFIX}{pso:#x} unknown (not created through CreateGraphicsPSO_X/CreateComputePSO_X)") };
    };
    let mut s = format!("{PREFIX}{pso:#x}");
    if h.cs != 0 {
        s.push_str(&format!(" cs={:016x}", h.cs));
    } else {
        s.push_str(&format!(" vs={:016x} ps={:016x}", h.vs, h.ps));
    }
    if h.other != 0 {
        s.push_str(&format!(" ds/hs/gs={:016x}", h.other));
    }
    if ph {
        s.push_str(" PLACEHOLDER (real PSO still building)");
    }
    s
}

unsafe fn vslot(obj: *mut c_void, i: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(i) }
}

pub unsafe fn remark(list: *mut c_void, pso: *mut c_void) {
    if enabled() {
        unsafe { mark(list, pso) };
    }
}

pub unsafe fn mark(list: *mut c_void, pso: *mut c_void) {
    if list.is_null() {
        return;
    }
    let entry = table().lock().unwrap_or_else(|e| e.into_inner()).get(&(pso as usize)).copied();
    unsafe { write(list, text(pso as usize, entry)) };
}

pub fn dims_text(x: u32, y: u32, z: u32) -> String {
    format!("{DIMS}{x}x{y}x{z}")
}

unsafe fn write(list: *mut c_void, t: String) {
    let w: Vec<u16> = t.encode_utf16().chain(Some(0)).collect();
    note_marker(list as usize, t);
    let f: unsafe extern "system" fn(*mut c_void, u32, *const c_void, u32) = unsafe { core::mem::transmute(crate::cmdlist_wrap::pc_method(list, PC_SET_MARKER)) };
    unsafe { f(list, 0, w.as_ptr() as *const c_void, (w.len() * 2) as u32) };
    MARKS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "system" fn set_pso(list: *mut c_void, pso: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(PREV_SET.load(Ordering::Relaxed)) };
    unsafe { f(list, pso) };
    unsafe { mark(list, pso) };
}

unsafe extern "system" fn dispatch(list: *mut c_void, x: u32, y: u32, z: u32) {
    if !list.is_null() {
        unsafe { write(list, dims_text(x, y, z)) };
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = unsafe { core::mem::transmute(PREV_DISPATCH.load(Ordering::Relaxed)) };
    unsafe { f(list, x, y, z) };
}

unsafe extern "system" fn dispatch_x(list: *mut c_void, x: u32, y: u32, z: u32, flags: u32) {
    if !list.is_null() {
        unsafe { write(list, dims_text(x, y, z)) };
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = unsafe { core::mem::transmute(PREV_DISPATCH_X.load(Ordering::Relaxed)) };
    unsafe { f(list, x, y, z, flags) };
}

unsafe extern "system" fn reset(list: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(PREV_RESET.load(Ordering::Relaxed)) };
    let hr = unsafe { f(list, alloc, pso) };
    if hr >= 0 {
        begin_recording_now(list as usize);
        unsafe { mark(list, pso) };
    }
    hr
}

unsafe extern "system" fn clear_state(list: *mut c_void, pso: *mut c_void) {
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(PREV_CLEAR.load(Ordering::Relaxed)) };
    unsafe { f(list, pso) };
    unsafe { mark(list, pso) };
}

pub fn install(vt: &[AtomicUsize]) {
    if !enabled() {
        return;
    }
    let (s, r, c) = (vt[XBOX_SLOT_SET_PSO].load(Ordering::Relaxed), vt[XBOX_SLOT_RESET].load(Ordering::Relaxed), vt[XBOX_SLOT_CLEAR_STATE].load(Ordering::Relaxed));
    if s == 0 || r == 0 || c == 0 {
        record("d3d12_xs", "dred", "command-list slots 10/11/25 missing; no PSO markers");
        return;
    }
    PREV_SET.store(s, Ordering::Relaxed);
    PREV_RESET.store(r, Ordering::Relaxed);
    PREV_CLEAR.store(c, Ordering::Relaxed);
    vt[XBOX_SLOT_SET_PSO].store(set_pso as usize, Ordering::Relaxed);
    vt[XBOX_SLOT_RESET].store(reset as usize, Ordering::Relaxed);
    vt[XBOX_SLOT_CLEAR_STATE].store(clear_state as usize, Ordering::Relaxed);
    for (slot, prev, hook) in [(XBOX_SLOT_DISPATCH, &PREV_DISPATCH, dispatch as usize), (crate::console_slots::CL_DISPATCH_X, &PREV_DISPATCH_X, dispatch_x as usize)] {
        let cur = vt[slot].load(Ordering::Relaxed);
        if cur != 0 {
            prev.store(cur, Ordering::Relaxed);
            vt[slot].store(hook, Ordering::Relaxed);
        }
    }
    record("d3d12_xs", "dred", "TOMBOX_DRED=2: every SetPipelineState/Reset/ClearState also writes a SetMarker naming the bound PSO's Xbox shader hashes and every Dispatch/DispatchX one with its group counts; a removal report names the shaders and dispatch sizes of the unfinished work");
}

pub fn marks() -> usize {
    MARKS.load(Ordering::Relaxed)
}

pub fn is_work(op: u32) -> bool {
    matches!(op, 3 | 4 | 5 | 6 | 34 | 42 | 47)
}

pub fn governing<'a>(ctx: &'a [(u32, String)], i: u32, ours: bool) -> Option<&'a str> {
    ctx.iter().filter(|(k, s)| *k <= i && !s.starts_with(DIMS) && s.starts_with(PREFIX) == ours).max_by_key(|(k, _)| *k).map(|(_, s)| s.as_str())
}

pub fn dispatch_dims<'a>(hist: &[u32], ctx: &'a [(u32, String)], i: u32) -> Option<&'a str> {
    let (k, s) = ctx.iter().filter(|(k, s)| *k < i && s.starts_with(DIMS)).max_by_key(|(k, _)| *k)?;
    if (k + 1..i).any(|j| hist.get(j as usize).is_some_and(|&op| is_work(op))) {
        return None;
    }
    Some(&s[DIMS.len()..])
}

pub fn from_side_table(hist: &[u32], recorded: &[String]) -> Vec<(u32, String)> {
    let at: Vec<u32> = (0..hist.len() as u32).filter(|&i| hist[i as usize] == OP_SETMARKER).collect();
    if at.len() != recorded.len() {
        return Vec::new();
    }
    at.into_iter().zip(recorded.iter().cloned()).collect()
}

pub fn window_lines(hist: &[u32], last: u32, ctx: &[(u32, String)]) -> Vec<String> {
    let lo = last.saturating_sub(4);
    let hi = (last + 8).min(hist.len() as u32);
    let mut out = Vec::new();
    let mut psos: Vec<&str> = Vec::new();
    for i in lo..hi {
        let op = hist[i as usize];
        if !is_work(op) {
            continue;
        }
        let p = governing(ctx, i, true);
        let ev = governing(ctx, i, false).map(|e| format!(" (title marker {e:?})")).unwrap_or_default();
        let tag = if i == last { " FIRST UNFINISHED" } else if i < last { " done" } else { " queued" };
        let groups = dispatch_dims(hist, ctx, i).filter(|_| op == OP_DISPATCH).map(|d| format!(" groups={d}")).unwrap_or_default();
        out.push(format!("op #{i} {}{tag}: {}{groups}{ev}", crate::dred::op_name(op), p.unwrap_or("no PSO marker before this op")));
        if let Some(p) = p {
            if !psos.contains(&p) {
                psos.push(p);
            }
        }
    }
    let n = ctx.iter().filter(|(_, s)| s.starts_with(PREFIX)).count();
    out.push(format!("{} distinct PSO(s) in ops {lo}..{hi}; {n} PSO marker(s) in this list", psos.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_wrap::Guid;

    #[test]
    fn markers_name_the_shaders_of_each_draw_in_the_window() {
        let mut hist = vec![46u32, 15, 0, 4, 4, 0, 4, 1, 4, 0, 3, 6, 4, 4];
        hist.extend([4u32; 6]);
        let ctx = vec![
            (2u32, format!("{PREFIX}0x10 vs=00000000000000aa ps=00000000000000bb")),
            (5, format!("{PREFIX}0x20 vs=00000000000000cc ps=00000000000000dd PLACEHOLDER (real PSO still building)")),
            (7, "ShadowPass".to_string()),
            (9, format!("{PREFIX}0x30 cs=00000000000000ee")),
        ];
        let out = window_lines(&hist, 8, &ctx);
        assert_eq!(out[0], format!("op #4 DRAWINDEXEDINSTANCED done: {PREFIX}0x10 vs=00000000000000aa ps=00000000000000bb"));
        assert!(out[1].starts_with("op #6 DRAWINDEXEDINSTANCED done: tbpso 0x20") && out[1].contains("PLACEHOLDER"), "{}", out[1]);
        assert_eq!(out[2], format!("op #8 DRAWINDEXEDINSTANCED FIRST UNFINISHED: {PREFIX}0x20 vs=00000000000000cc ps=00000000000000dd PLACEHOLDER (real PSO still building) (title marker \"ShadowPass\")"));
        assert_eq!(out[3], format!("op #10 DRAWINSTANCED queued: {PREFIX}0x30 cs=00000000000000ee (title marker \"ShadowPass\")"));
        assert!(out[4].starts_with("op #11 DISPATCH queued"));
        assert_eq!(out.last().unwrap(), "3 distinct PSO(s) in ops 4..16; 3 PSO marker(s) in this list");
        assert_eq!(window_lines(&[4, 4], 0, &[])[0], "op #0 DRAWINDEXEDINSTANCED FIRST UNFINISHED: no PSO marker before this op");
    }

    #[test]
    fn dispatch_markers_give_the_group_counts_of_the_dispatch_they_precede() {
        let hist = vec![0u32, 0, 6, 0, 46, 6, 0, 6, 6, 0, 3];
        let ctx = vec![
            (0u32, format!("{PREFIX}0x40 cs=0123456789abcdef")),
            (1, dims_text(4096, 1, 1)),
            (3, dims_text(7, 2, 3)),
            (6, dims_text(1, 1, 1)),
            (9, dims_text(9, 9, 9)),
        ];
        let out = window_lines(&hist, 5, &ctx);
        assert_eq!(out[0], format!("op #2 DISPATCH done: {PREFIX}0x40 cs=0123456789abcdef groups=4096x1x1"));
        assert_eq!(out[1], format!("op #5 DISPATCH FIRST UNFINISHED: {PREFIX}0x40 cs=0123456789abcdef groups=7x2x3"));
        assert_eq!(out[2], format!("op #7 DISPATCH queued: {PREFIX}0x40 cs=0123456789abcdef groups=1x1x1"));
        assert_eq!(out[3], format!("op #8 DISPATCH queued: {PREFIX}0x40 cs=0123456789abcdef"));
        assert!(out[4].starts_with("op #10 DRAWINSTANCED queued") && !out[4].contains("groups=") && !out[4].contains("title marker"), "{}", out[4]);
        assert_eq!(dims_text(65535, 2, 1), "tbdispatch 65535x2x1");
    }

    #[test]
    fn side_table_maps_the_kth_setmarker_only_when_the_counts_agree() {
        let hist = [46u32, 0, 4, 0, 4];
        let rec = vec!["a".to_string(), "b".to_string()];
        assert_eq!(from_side_table(&hist, &rec), vec![(1, "a".to_string()), (3, "b".to_string())]);
        assert!(from_side_table(&hist, &rec[..1]).is_empty());
    }

    #[test]
    fn marker_text_names_hashes_placeholders_and_unknowns() {
        let h = Hashes { vs: 0x3d3a_c0c8_e461_a6f6, ps: 0x1, cs: 0, other: 0 };
        assert_eq!(text(0x1234, Some((h, false))), "tbpso 0x1234 vs=3d3ac0c8e461a6f6 ps=0000000000000001");
        assert_eq!(text(0x1234, Some((h, true))), "tbpso 0x1234 vs=3d3ac0c8e461a6f6 ps=0000000000000001 PLACEHOLDER (real PSO still building)");
        assert_eq!(text(0x9, Some((Hashes { cs: 0xabc, ..Hashes::default() }, false))), "tbpso 0x9 cs=0000000000000abc");
        assert!(text(0x77, None).contains("0x77 unknown"));
        assert_eq!(text(0, None), "tbpso none");
    }

    #[test]
    fn descriptor_hashes_are_fnv1a_of_the_original_blobs() {
        let vs = vec![1u8, 2, 3, 4, 5];
        let ps = vec![9u8; 40];
        let mut d = vec![0u8; 0x290];
        d[0x08..0x10].copy_from_slice(&(vs.as_ptr() as u64).to_le_bytes());
        d[0x10..0x18].copy_from_slice(&(vs.len() as u64).to_le_bytes());
        d[0x18..0x20].copy_from_slice(&(ps.as_ptr() as u64).to_le_bytes());
        d[0x20..0x28].copy_from_slice(&(ps.len() as u64).to_le_bytes());
        let h = unsafe { gfx_hashes(d.as_ptr()) };
        assert_eq!(h, Hashes { vs: crate::shader_xlat::fnv1a(&vs), ps: crate::shader_xlat::fnv1a(&ps), cs: 0, other: 0 });
        let c = unsafe { compute_hashes(d.as_ptr()) };
        assert_eq!(c.cs, crate::shader_xlat::fnv1a(&vs));
    }

    const IID_DEVICE: Guid = Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
    const IID_ALLOCATOR: Guid = Guid { d1: 0x6102_dee4, d2: 0xaf59, d3: 0x4b09, d4: [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24] };
    const IID_LIST: Guid = Guid { d1: 0x5b16_0d0f, d2: 0xac1b, d3: 0x4185, d4: [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55] };
    const IID_FACTORY4: Guid = Guid { d1: 0x1bc6_ea02, d2: 0xef36, d3: 0x464f, d4: [0xbf, 0x0c, 0x21, 0xca, 0x39, 0xe5, 0x16, 0x8a] };
    const IID_ADAPTER: Guid = Guid { d1: 0x2411_e7e1, d2: 0x12ac, d3: 0x4ccf, d4: [0xbd, 0x14, 0x97, 0x98, 0xe8, 0x53, 0x4d, 0xc0] };

    extern "system" {
        fn LoadLibraryW(p: *const u16) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
    }

    macro_rules! call {
        ($o:expr, $i:expr, ($($t:ty),*) -> $r:ty $(, $a:expr)*) => {{
            let o = $o as *mut c_void;
            let f: unsafe extern "system" fn(*mut c_void $(, $t)*) -> $r = core::mem::transmute(vslot(o, $i));
            f(o $(, $a)*)
        }};
    }

    unsafe fn warp_device() -> *mut c_void {
        let name: Vec<u16> = "dxgi.dll\0".encode_utf16().collect();
        let h = LoadLibraryW(name.as_ptr());
        assert!(!h.is_null());
        let p = GetProcAddress(h, b"CreateDXGIFactory2\0".as_ptr());
        let create: unsafe extern "system" fn(u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
        let mut f: *mut c_void = core::ptr::null_mut();
        assert!(create(0, &IID_FACTORY4, &mut f) >= 0);
        let mut a: *mut c_void = core::ptr::null_mut();
        assert!(call!(f, 27, (*const Guid, *mut *mut c_void) -> i32, &IID_ADAPTER, &mut a) >= 0);
        let mk: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(crate::host_proc(b"D3D12CreateDevice\0"));
        let mut dev: *mut c_void = core::ptr::null_mut();
        assert!(mk(a, 0xb000, &IID_DEVICE, &mut dev) >= 0 && !dev.is_null());
        dev
    }

    #[test]
    fn markers_go_into_a_real_list_and_its_side_table_recording() {
        assert!(crate::dred::enable_now(crate::dred::Mode::Markers));
        unsafe {
            let dev = warp_device();
            let mut a: *mut c_void = core::ptr::null_mut();
            assert!(call!(dev, 9, (u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut a) >= 0);
            let mut l: *mut c_void = core::ptr::null_mut();
            assert!(call!(dev, 12, (u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, a, core::ptr::null_mut(), &IID_LIST, &mut l) >= 0);
            let fake = 0x5150usize as *mut c_void;
            table().lock().unwrap().insert(fake as usize, (Hashes { vs: 0xfeed, ps: 0xbeef, cs: 0, other: 0 }, true));
            let before = marks();
            mark(l, core::ptr::null_mut());
            mark(l, fake);
            assert_eq!(marks() - before, 2);
            assert!(call!(l, 9, () -> i32) >= 0, "Close after SetMarker");
            assert!(call!(a, 8, () -> i32) >= 0);
            assert!(call!(l, 10, (*mut c_void, *mut c_void) -> i32, a, core::ptr::null_mut()) >= 0);
            begin_recording_now(l as usize);
            mark(l, fake);
            assert!(call!(l, 9, () -> i32) >= 0);
            let r = recorded(l as usize);
            assert_eq!(r.len(), 2);
            assert_eq!(r[0], vec!["tbpso none".to_string(), "tbpso 0x5150 vs=000000000000feed ps=000000000000beef PLACEHOLDER (real PSO still building)".to_string()]);
            assert_eq!(r[1].len(), 1);
            let mut used = Vec::new();
            let (c, why) = fallback_contexts(l as usize, &[46, 0, 4, 0, 4], &mut used);
            assert_eq!(c.len(), 2, "{why}");
            assert_eq!(c[1].0, 3);
            let (c, _) = fallback_contexts(l as usize, &[46, 0, 4], &mut used);
            assert_eq!(c, vec![(1, r[1][0].clone())]);
            let (c, why) = fallback_contexts(l as usize, &[46, 0, 0, 0], &mut used);
            assert!(c.is_empty() && why.contains("kept: [2, 1]"), "{why}");
            for _ in 0..KEEP_RECORDINGS + 2 {
                begin_recording_now(l as usize);
            }
            assert_eq!(recorded(l as usize).len(), KEEP_RECORDINGS);
        }
    }
}
