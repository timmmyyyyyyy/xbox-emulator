use core::ffi::c_void;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use compat_common::record;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid(u32, u16, u16, [u8; 8]);
const IID_DEVICE: Guid = Guid(0x189819f1, 0x1db6, 0x4b57, [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7]);
const IID_PSO: Guid = Guid(0x765a30f3, 0xf624, 0x4c6f, [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45]);

#[derive(Default)]
struct ListState {
    pso: usize,
    root_sig: usize,
    rtv: Vec<usize>,
    rt_count: u32,
    single: bool,
    dsv: Option<usize>,
    viewports: Vec<[f32; 6]>,
    scissors: Vec<[i32; 4]>,
    topo: Option<u32>,
    dirty: Vec<(usize, u32, u32)>,
    lazy: Vec<Lazy>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Lazy {
    res: usize,
    view: Option<[u8; 24]>,
    fmt: u32,
    samples: u32,
}

#[derive(Debug, PartialEq)]
enum Sel {
    None,
    All,
    Only(Vec<usize>),
}

type View = (usize, Option<[u8; 24]>);

const IID_DESC_HEAP: Guid = Guid(0x8efb471d, 0x616c, 0x4f49, [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51]);
const ST_RENDER_TARGET: u32 = 4;

static LISTS: Mutex<Option<HashMap<usize, ListState>>> = Mutex::new(None);
static RTVS: Mutex<Option<HashMap<usize, View>>> = Mutex::new(None);
static OWN_RTV: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);
static LAZY_PASSES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static TEST_LAZY: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
static TEST_PS: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
#[cfg(test)]
static TEST_REAL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

pub fn lazy() -> bool {
    #[cfg(test)]
    {
        let t = TEST_LAZY.load(core::sync::atomic::Ordering::Relaxed);
        if t != 0 {
            return t == 2;
        }
    }
    static L: OnceLock<bool> = OnceLock::new();
    *L.get_or_init(|| {
        let on = std::env::var("TOMBOX_BLEND_CLAMP_LAZY").map(|v| v.trim() == "1").unwrap_or(false);
        if on {
            record("d3d12_xs", "blendclamp", "lazy clamping (TOMBOX_BLEND_CLAMP_LAZY=1): additive/min/max blends into a small-float target clamp once, when the target is read (a barrier out of RENDER_TARGET, a copy or resolve from it, or Close); blends whose factors read the destination still clamp at every boundary and before the draw");
        }
        on
    })
}

pub fn lazy_passes() -> u64 {
    LAZY_PASSES.load(core::sync::atomic::Ordering::Relaxed)
}

pub unsafe fn note_rtv(handle: usize, res: usize, pdesc: *const u8) {
    if !lazy() || handle == 0 {
        return;
    }
    let view = if pdesc.is_null() {
        None
    } else {
        let mut v = [0u8; 24];
        unsafe { core::ptr::copy_nonoverlapping(pdesc, v.as_mut_ptr(), 24) };
        Some(v)
    };
    let mut g = RTVS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if res == 0 {
        m.remove(&handle);
    } else {
        m.insert(handle, (res, view));
    }
}

fn rtv_view(handle: usize) -> Option<View> {
    RTVS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&handle).copied())
}

fn lazy_safe(d: &[u8], b: usize, fmt: u32) -> bool {
    let u = |o: usize| u32::from_le_bytes(d[b + o..b + o + 4].try_into().unwrap());
    let reads_dst = |f: u32| matches!(f, 7..=11);
    let ok = |src: u32, dst: u32, op: u32| match op {
        4 | 5 => true,
        1 => dst == 2 && !reads_dst(src),
        _ => false,
    };
    let alpha_used = fmt != 26 && d[b + 0x24] & 8 != 0;
    ok(u(0x08), u(0x0c), u(0x10)) && (!alpha_used || ok(u(0x14), u(0x18), u(0x1c)))
}

fn barrier_hits(b: &[u8], dirty: &[usize]) -> Sel {
    if dirty.is_empty() {
        return Sel::None;
    }
    let mut hit = Vec::new();
    for e in b.chunks_exact(32) {
        let u = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
        let p = |o: usize| u64::from_le_bytes(e[o..o + 8].try_into().unwrap()) as usize;
        match u(0) {
            0 => {
                let r = p(8);
                if u(20) & ST_RENDER_TARGET != 0 && u(24) & ST_RENDER_TARGET == 0 && dirty.contains(&r) {
                    hit.push(r);
                }
            }
            1 => {
                let (a, z) = (p(8), p(16));
                if a == 0 || z == 0 {
                    return Sel::All;
                }
                hit.extend([a, z].into_iter().filter(|r| dirty.contains(r)));
            }
            2 => {
                let r = p(8);
                if r != 0 && dirty.contains(&r) {
                    hit.push(r);
                }
            }
            _ => return Sel::All,
        }
    }
    hit.dedup();
    if hit.is_empty() { Sel::None } else { Sel::Only(hit) }
}
static PSOS: Mutex<Option<HashMap<(usize, u32, u32, bool), usize>>> = Mutex::new(None);
static RTV_INC: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static PASSES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

static LIVE: crate::live_sw::Armed = crate::live_sw::Armed::new("TOMBOX_BLEND_CLAMP", "blend_clamp", true);

pub fn ps_mode() -> bool {
    #[cfg(test)]
    {
        let t = TEST_PS.load(core::sync::atomic::Ordering::Relaxed);
        if t != 0 {
            return t == 2;
        }
    }
    static P: OnceLock<bool> = OnceLock::new();
    *P.get_or_init(|| {
        let v = compat_common::live::value("TOMBOX_BLEND_CLAMP", "blend_clamp").map(|v| v.trim().to_ascii_lowercase());
        if v.as_deref() == Some("auto") {
            let o = crate::blend_probe::overflow();
            let on = o == Some(crate::blend_probe::Overflow::Saturates);
            record("d3d12_xs", "blendclamp", &if on {
                "TOMBOX_BLEND_CLAMP=auto: this adapter saturates blend overflow like the console, so ps mode is used (shader-side Inf/NaN clamp, no full-target passes)".to_string()
            } else {
                format!("TOMBOX_BLEND_CLAMP=auto: blend overflow probe gave {o:?}, so the eager full-target clamp passes are kept")
            });
            return on;
        }
        let on = v.as_deref() == Some("ps");
        if on {
            record("d3d12_xs", "blendclamp", "TOMBOX_BLEND_CLAMP=ps: pixel shaders writing fp16 or R11G11B10 targets clamp their colour outputs to the largest finite value (NaN to +max, as the eager clamp pass gives), and the full-target clamp passes are off; needs export_fmt on (the default); read once at start, not live");
        }
        on
    })
}

pub fn enabled() -> bool {
    if ps_mode() {
        return false;
    }
    static E: OnceLock<bool> = OnceLock::new();
    E.get_or_init(|| {
        let on = LIVE.armed();
        record("d3d12_xs", "blendclamp", if on {
            "small-float blend results clamp to the format's largest finite value (console round-toward-zero output conversion); live.cfg blend_clamp=0/1 toggles it"
        } else {
            "disabled by TOMBOX_BLEND_CLAMP=0"
        });
        on
    });
    static WAS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);
    let on = LIVE.on();
    if WAS.swap(on, core::sync::atomic::Ordering::Relaxed) != on && on {
        *LISTS.lock().unwrap() = None;
    }
    on
}

pub fn passes() -> u64 {
    PASSES.load(core::sync::atomic::Ordering::Relaxed)
}

fn small_float(fmt: u32) -> Option<bool> {
    match fmt {
        26 => Some(false),
        10 | 34 | 54 => Some(true),
        _ => None,
    }
}

fn with<R>(list: usize, f: impl FnOnce(&mut ListState) -> R) -> R {
    let mut g = LISTS.lock().unwrap();
    f(g.get_or_insert_with(HashMap::new).entry(list).or_default())
}

pub fn on_reset(list: usize) {
    if !enabled() {
        return;
    }
    let mut g = LISTS.lock().unwrap();
    if let Some(m) = g.as_mut() {
        m.remove(&list);
    }
}

pub fn on_set_pso(list: usize, pso: usize) {
    if enabled() {
        with(list, |s| s.pso = pso);
    }
}

pub fn on_set_root_sig(list: usize, rs: usize) {
    if enabled() {
        with(list, |s| s.root_sig = rs);
    }
}

pub fn on_topology(list: usize, t: u32) {
    if enabled() {
        with(list, |s| s.topo = Some(t));
    }
}

pub unsafe fn on_viewports(list: usize, n: u32, p: *const [f32; 6]) {
    if !enabled() || p.is_null() {
        return;
    }
    let v = unsafe { core::slice::from_raw_parts(p, (n as usize).min(16)) }.to_vec();
    with(list, |s| s.viewports = v);
}

pub unsafe fn on_scissors(list: usize, n: u32, p: *const [i32; 4]) {
    if !enabled() || p.is_null() {
        return;
    }
    let v = unsafe { core::slice::from_raw_parts(p, (n as usize).min(16)) }.to_vec();
    with(list, |s| s.scissors = v);
}

pub unsafe fn on_set_targets(list: usize, n: u32, handles: *const usize, single: bool, dsv: *const usize) {
    if !enabled() {
        return;
    }
    let n = n.min(8);
    let rtv = if handles.is_null() || n == 0 {
        Vec::new()
    } else if single {
        vec![unsafe { *handles }]
    } else {
        unsafe { core::slice::from_raw_parts(handles, n as usize) }.to_vec()
    };
    let d = if dsv.is_null() { None } else { Some(unsafe { *dsv }) };
    with(list, |s| {
        s.rtv = rtv;
        s.rt_count = n;
        s.single = single;
        s.dsv = d;
    });
}

pub fn on_draw(list: usize) {
    if !enabled() {
        return;
    }
    let mut g = LISTS.lock().unwrap();
    let s = g.get_or_insert_with(HashMap::new).entry(list).or_default();
    if s.pso == 0 || s.rt_count == 0 {
        return;
    }
    let Some((d, _keep)) = crate::device_wrap::gfx_pso_desc(s.pso) else { return };
    let samples = u32::from_le_bytes(d[0x268..0x26c].try_into().unwrap()).max(1);
    let lz = lazy();
    for (h, fmt, b) in blended_targets(&d, s) {
        if lz && lazy_safe(&d, b, fmt) {
            if let Some((res, view)) = rtv_view(h) {
                let e = Lazy { res, view, fmt, samples };
                if !s.lazy.contains(&e) {
                    s.lazy.push(e);
                }
                continue;
            }
        }
        if !s.dirty.iter().any(|e| e.0 == h) {
            s.dirty.push((h, fmt, samples));
        }
    }
}

fn blended_targets(d: &[u8], s: &ListState) -> Vec<(usize, u32, usize)> {
    let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let independent = u(0x7c) != 0;
    let n = u(0x240).min(s.rt_count).min(8);
    let inc = RTV_INC.load(core::sync::atomic::Ordering::Relaxed);
    let mut out = Vec::new();
    for i in 0..n as usize {
        let fmt = u(0x244 + 4 * i);
        if small_float(fmt).is_none() {
            continue;
        }
        let b = if independent { 0x80 + 0x28 * i } else { 0x80 };
        let (en, op, opa, mask) = (u(b), u(b + 0x10), u(b + 0x1c), d[b + 0x24]);
        if en == 0 || mask == 0 || (matches!(op, 4 | 5) && matches!(opa, 4 | 5)) {
            continue;
        }
        let h = if s.single {
            if inc == 0 {
                continue;
            }
            s.rtv[0] + i * inc
        } else {
            match s.rtv.get(i) {
                Some(&h) => h,
                None => continue,
            }
        };
        if h != 0 {
            out.push((h, fmt, b));
        }
    }
    out
}

unsafe fn pre_draw(list: *mut c_void) {
    if !lazy() || !enabled() {
        return;
    }
    let hit: Vec<usize> = {
        let mut g = LISTS.lock().unwrap();
        let Some(s) = g.as_mut().and_then(|m| m.get_mut(&(list as usize))) else { return };
        if s.lazy.is_empty() || s.pso == 0 || s.rt_count == 0 {
            return;
        }
        let Some((d, _keep)) = crate::device_wrap::gfx_pso_desc(s.pso) else { return };
        let mut hit = Vec::new();
        for (h, fmt, b) in blended_targets(&d, s) {
            if lazy_safe(&d, b, fmt) {
                continue;
            }
            if let Some((res, _)) = rtv_view(h) {
                if s.lazy.iter().any(|e| e.res == res) && !hit.contains(&res) {
                    hit.push(res);
                }
            }
        }
        hit
    };
    if !hit.is_empty() {
        unsafe { flush_sel(list, false, Sel::Only(hit)) };
    }
}

unsafe fn flush_reads(list: *mut c_void, src: usize) {
    if src == 0 || !lazy() {
        return;
    }
    let dirty = {
        let g = LISTS.lock().unwrap();
        g.as_ref().and_then(|m| m.get(&(list as usize))).is_some_and(|s| s.lazy.iter().any(|e| e.res == src))
    };
    if dirty {
        unsafe { flush_sel(list, false, Sel::Only(vec![src])) };
    }
}

static NEXT: [core::sync::atomic::AtomicUsize; 64] = [const { core::sync::atomic::AtomicUsize::new(0) }; 64];

fn next(slot: usize) -> usize {
    NEXT[slot].load(core::sync::atomic::Ordering::Relaxed)
}

pub unsafe fn install() {
    if !enabled() {
        return;
    }
    let mut hooks: Vec<(usize, usize)> = vec![
        (9, h_close as usize), (10, h_reset as usize), (12, h_draw as usize), (13, h_draw_indexed as usize),
        (20, h_topo as usize), (21, h_viewports as usize), (22, h_scissors as usize), (25, h_set_pso as usize),
        (26, h_barrier as usize), (34, h_root_sig as usize), (50, h_targets as usize), (59, h_indirect as usize),
    ];
    if lazy() {
        hooks.extend([(16, h_copy_texture as usize), (17, h_copy_resource as usize), (19, h_resolve as usize)]);
    }
    for &(slot, _) in &hooks {
        let prev = crate::cmdlist_wrap::vt_get(slot);
        if prev == 0 {
            record("d3d12_xs", "blendclamp", &format!("command-list slot {slot} empty - clamp hooks not installed"));
            return;
        }
        NEXT[slot].store(prev, core::sync::atomic::Ordering::Relaxed);
    }
    for (slot, f) in hooks {
        crate::cmdlist_wrap::vt_set(slot, f);
    }
}

unsafe extern "system" fn h_copy_texture(this: *mut c_void, dst: *const u8, x: u32, y: u32, z: u32, src: *const u8, b: *const c_void) {
    if !src.is_null() {
        unsafe { flush_reads(this, *(src as *const usize)) };
    }
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void) = unsafe { core::mem::transmute(next(16)) };
    unsafe { f(this, dst, x, y, z, src, b) }
}

unsafe extern "system" fn h_copy_resource(this: *mut c_void, dst: *mut c_void, src: *mut c_void) {
    unsafe { flush_reads(this, src as usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) = unsafe { core::mem::transmute(next(17)) };
    unsafe { f(this, dst, src) }
}

unsafe extern "system" fn h_resolve(this: *mut c_void, dst: *mut c_void, dst_sub: u32, src: *mut c_void, src_sub: u32, fmt: u32) {
    unsafe { flush_reads(this, src as usize) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u32, u32) = unsafe { core::mem::transmute(next(19)) };
    unsafe { f(this, dst, dst_sub, src, src_sub, fmt) }
}

unsafe extern "system" fn h_close(this: *mut c_void) -> i32 {
    unsafe { flush(this) };
    let f: unsafe extern "system" fn(*mut c_void) -> i32 = unsafe { core::mem::transmute(next(9)) };
    unsafe { f(this) }
}

unsafe extern "system" fn h_reset(this: *mut c_void, alloc: *mut c_void, pso: *mut c_void) -> i32 {
    on_reset(this as usize);
    if !pso.is_null() {
        on_set_pso(this as usize, pso as usize);
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = unsafe { core::mem::transmute(next(10)) };
    unsafe { f(this, alloc, pso) }
}

unsafe extern "system" fn h_draw(this: *mut c_void, a: u32, b: u32, c: u32, d: u32) {
    unsafe { pre_draw(this) };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = unsafe { core::mem::transmute(next(12)) };
    unsafe { f(this, a, b, c, d) };
    on_draw(this as usize);
}

unsafe extern "system" fn h_draw_indexed(this: *mut c_void, a: u32, b: u32, c: u32, d: i32, e: u32) {
    unsafe { pre_draw(this) };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32) = unsafe { core::mem::transmute(next(13)) };
    unsafe { f(this, a, b, c, d, e) };
    on_draw(this as usize);
}

unsafe extern "system" fn h_topo(this: *mut c_void, t: u32) {
    on_topology(this as usize, t);
    let f: unsafe extern "system" fn(*mut c_void, u32) = unsafe { core::mem::transmute(next(20)) };
    unsafe { f(this, t) }
}

unsafe extern "system" fn h_viewports(this: *mut c_void, n: u32, p: *const [f32; 6]) {
    unsafe { on_viewports(this as usize, n, p) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const [f32; 6]) = unsafe { core::mem::transmute(next(21)) };
    unsafe { f(this, n, p) }
}

unsafe extern "system" fn h_scissors(this: *mut c_void, n: u32, p: *const [i32; 4]) {
    unsafe { on_scissors(this as usize, n, p) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const [i32; 4]) = unsafe { core::mem::transmute(next(22)) };
    unsafe { f(this, n, p) }
}

unsafe extern "system" fn h_set_pso(this: *mut c_void, pso: *mut c_void) {
    on_set_pso(this as usize, pso as usize);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(next(25)) };
    unsafe { f(this, pso) }
}

unsafe extern "system" fn h_barrier(this: *mut c_void, n: u32, b: *const u8) {
    let sel = if b.is_null() || n == 0 {
        Sel::None
    } else {
        let dirty: Vec<usize> = with(this as usize, |s| s.lazy.iter().map(|e| e.res).collect());
        barrier_hits(unsafe { core::slice::from_raw_parts(b, n.min(4096) as usize * 32) }, &dirty)
    };
    unsafe { flush_sel(this, true, sel) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) = unsafe { core::mem::transmute(next(26)) };
    unsafe { f(this, n, b) }
}

unsafe extern "system" fn h_root_sig(this: *mut c_void, rs: *mut c_void) {
    if with(this as usize, |s| s.root_sig) != rs as usize {
        unsafe { flush(this) };
    }
    on_set_root_sig(this as usize, rs as usize);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = unsafe { core::mem::transmute(next(34)) };
    unsafe { f(this, rs) }
}

unsafe extern "system" fn h_targets(this: *mut c_void, n: u32, handles: *const usize, single: i32, dsv: *const usize) {
    unsafe { flush_sel(this, true, Sel::None) };
    if single != 0 && RTV_INC.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        unsafe { device_of(this) };
    }
    unsafe { on_set_targets(this as usize, n, handles, single != 0, dsv) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const usize, i32, *const usize) = unsafe { core::mem::transmute(next(50)) };
    unsafe { f(this, n, handles, single, dsv) }
}

unsafe extern "system" fn h_indirect(this: *mut c_void, sig: *mut c_void, n: u32, buf: *mut c_void, off: u64, cbuf: *mut c_void, coff: u64) {
    unsafe { pre_draw(this) };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u64, *mut c_void, u64) = unsafe { core::mem::transmute(next(59)) };
    unsafe { f(this, sig, n, buf, off, cbuf, coff) };
    on_draw(this as usize);
}

unsafe fn real(slot: usize) -> usize {
    #[cfg(test)]
    {
        let p = TEST_REAL.load(core::sync::atomic::Ordering::Relaxed);
        if p != 0 {
            return unsafe { *(p as *const usize).add(slot) };
        }
    }
    unsafe { crate::cmdlist_wrap::real_method(slot) }
}

unsafe fn device_of(list: *mut c_void) -> *mut c_void {
    let mut dev: *mut c_void = core::ptr::null_mut();
    let f: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(real(7)) };
    if unsafe { f(list, &IID_DEVICE, &mut dev) } < 0 || dev.is_null() {
        return core::ptr::null_mut();
    }
    let vt = unsafe { *(dev as *const *const usize) };
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*vt.add(2)) };
    if RTV_INC.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        let inc: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { core::mem::transmute(*vt.add(15)) };
        RTV_INC.store(unsafe { inc(dev, 2) } as usize, core::sync::atomic::Ordering::Relaxed);
    }
    unsafe { release(dev) };
    dev
}

fn shaders() -> Option<&'static (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)> {
    static S: OnceLock<Option<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>)>> = OnceLock::new();
    S.get_or_init(|| {
        let vs = "float4 main(uint id : SV_VertexID) : SV_Position { float2 p = float2((id << 1) & 2, id & 2); return float4(p * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0); }";
        let ps11 = "float4 main() : SV_Target { return float4(65024.0, 65024.0, 64512.0, 65504.0); }";
        let ps16 = "float4 main() : SV_Target { return float4(65504.0, 65504.0, 65504.0, 65504.0); }";
        let ps16n = "float4 main() : SV_Target { return float4(-65504.0, -65504.0, -65504.0, -65504.0); }";
        let c = |src: &str, p: &str| tombox_xshader::dxc::compile(src, p);
        match (c(vs, "vs_6_0"), c(ps11, "ps_6_0"), c(ps16, "ps_6_0"), c(ps16n, "ps_6_0")) {
            (Ok(a), Ok(b), Ok(d), Ok(e)) => Some((a, b, d, e)),
            (a, b, d, e) => {
                record("d3d12_xs", "blendclamp", &format!("shader compile failed: {:?} {:?} {:?} {:?}", a.err(), b.err(), d.err(), e.err()));
                None
            }
        }
    })
    .as_ref()
}

unsafe fn clamp_pso(dev: *mut c_void, root_sig: usize, fmt: u32, samples: u32, neg: bool) -> usize {
    let key = (root_sig, fmt, samples, neg);
    if let Some(&p) = PSOS.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return p;
    }
    let Some((vs, ps11, ps16, ps16n)) = shaders() else { return 0 };
    let ps = if neg { ps16n } else if fmt == 26 { ps11 } else { ps16 };
    let mut d = vec![0u8; 0x290];
    let put32 = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put64(&mut d, 0x00, root_sig as u64);
    put64(&mut d, 0x08, vs.as_ptr() as u64);
    put64(&mut d, 0x10, vs.len() as u64);
    put64(&mut d, 0x18, ps.as_ptr() as u64);
    put64(&mut d, 0x20, ps.len() as u64);
    put32(&mut d, 0x80, 1);
    put32(&mut d, 0x88, 2);
    put32(&mut d, 0x8c, 2);
    put32(&mut d, 0x90, if neg { 5 } else { 4 });
    put32(&mut d, 0x94, 2);
    put32(&mut d, 0x98, 2);
    put32(&mut d, 0x9c, if neg { 5 } else { 4 });
    put32(&mut d, 0xa0, 4);
    d[0xa4] = 0x0f;
    put32(&mut d, 0x1c0, 0xffff_ffff);
    put32(&mut d, 0x1c4, 3);
    put32(&mut d, 0x1c8, 1);
    put32(&mut d, 0x1dc, 1);
    put32(&mut d, 0x23c, 3);
    put32(&mut d, 0x240, 1);
    put32(&mut d, 0x244, fmt);
    put32(&mut d, 0x268, samples);
    let vt = unsafe { *(dev as *const *const usize) };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(*vt.add(10)) };
    let mut out: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { f(dev, d.as_ptr(), &IID_PSO, &mut out) };
    if hr < 0 || out.is_null() {
        record("d3d12_xs", "blendclamp", &format!("clamp PSO create failed hr={hr:#010x} fmt={fmt} samples={samples} neg={neg}"));
        PSOS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, 0);
        return 0;
    }
    PSOS.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, out as usize);
    out as usize
}

pub unsafe fn flush(list: *mut c_void) {
    unsafe { flush_sel(list, true, Sel::All) };
}

unsafe fn own_rtv(dev: *mut c_void, res: usize, view: &Option<[u8; 24]>, heaps: &mut HashMap<usize, usize>) -> usize {
    let vt = unsafe { *(dev as *const *const usize) };
    let h = match heaps.get(&(dev as usize)) {
        Some(&h) => h,
        None => {
            let hd = [2u32, 1, 0, 0];
            let mut heap: *mut c_void = core::ptr::null_mut();
            let create: unsafe extern "system" fn(*mut c_void, *const u32, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(*vt.add(14)) };
            let hr = unsafe { create(dev, hd.as_ptr(), &IID_DESC_HEAP, &mut heap) };
            let mut h = 0usize;
            if hr >= 0 && !heap.is_null() {
                let start: unsafe extern "system" fn(*mut c_void, *mut usize) -> *mut usize = unsafe { core::mem::transmute(*(*(heap as *const *const usize)).add(9)) };
                unsafe { start(heap, &mut h) };
            } else {
                record("d3d12_xs", "blendclamp", &format!("lazy clamp RTV heap create failed hr={hr:#010x}; lazy targets are left unclamped"));
            }
            heaps.insert(dev as usize, h);
            h
        }
    };
    if h != 0 {
        let rtv: unsafe extern "system" fn(*mut c_void, usize, *const u8, usize) = unsafe { core::mem::transmute(*vt.add(20)) };
        unsafe { rtv(dev, res, view.as_ref().map_or(core::ptr::null(), |v| v.as_ptr()), h) };
    }
    h
}

unsafe fn flush_sel(list: *mut c_void, eager: bool, sel: Sel) {
    let _ij = crate::inject::mark(crate::inject::DRAW);
    if !enabled() {
        return;
    }
    let (dirty, pso, root_sig, rtv, rt_count, single, dsv, vps, scs, topo) = {
        let mut g = LISTS.lock().unwrap();
        let Some(s) = g.as_mut().and_then(|m| m.get_mut(&(list as usize))) else { return };
        let mut t: Vec<(Option<usize>, View, u32, u32)> = Vec::new();
        if eager {
            t.extend(core::mem::take(&mut s.dirty).into_iter().map(|(h, f, n)| (Some(h), (0, None), f, n)));
        }
        let picked: Vec<Lazy> = match sel {
            Sel::None => Vec::new(),
            Sel::All => core::mem::take(&mut s.lazy),
            Sel::Only(r) => {
                let (hit, keep): (Vec<Lazy>, Vec<Lazy>) = core::mem::take(&mut s.lazy).into_iter().partition(|e| r.contains(&e.res));
                s.lazy = keep;
                hit
            }
        };
        t.extend(picked.into_iter().map(|e| (None, (e.res, e.view), e.fmt, e.samples)));
        if t.is_empty() {
            return;
        }
        (t, s.pso, s.root_sig, s.rtv.clone(), s.rt_count, s.single, s.dsv, s.viewports.clone(), s.scissors.clone(), s.topo)
    };
    if root_sig == 0 {
        return;
    }
    let dev = unsafe { device_of(list) };
    if dev.is_null() {
        return;
    }
    let set_pso: unsafe extern "system" fn(*mut c_void, usize) = unsafe { core::mem::transmute(real(25)) };
    let om: unsafe extern "system" fn(*mut c_void, u32, *const usize, i32, *const usize) = unsafe { core::mem::transmute(real(46)) };
    let set_vp: unsafe extern "system" fn(*mut c_void, u32, *const [f32; 6]) = unsafe { core::mem::transmute(real(21)) };
    let set_sc: unsafe extern "system" fn(*mut c_void, u32, *const [i32; 4]) = unsafe { core::mem::transmute(real(22)) };
    let set_topo: unsafe extern "system" fn(*mut c_void, u32) = unsafe { core::mem::transmute(real(20)) };
    let draw: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = unsafe { core::mem::transmute(real(12)) };
    let vp = [0.0f32, 0.0, 16384.0, 16384.0, 0.0, 1.0];
    let sc = [0i32, 0, 16384, 16384];
    let mut did = false;
    for (handle, view, fmt, samples) in dirty {
        let neg_too = small_float(fmt) == Some(true);
        let passes: &[bool] = if neg_too { &[false, true] } else { &[false] };
        let mut bound = false;
        for &neg in passes {
            let p = unsafe { clamp_pso(dev, root_sig, fmt, samples, neg) };
            if p == 0 {
                continue;
            }
            if !did {
                unsafe { crate::gpuprof::pass_begin(list, crate::gpuprof::TAG_BLENDCLAMP) };
            }
            if !bound {
                let ok = match handle {
                    Some(h) => {
                        unsafe { om(list, 1, &h, 0, core::ptr::null()) };
                        true
                    }
                    None => {
                        let mut g = OWN_RTV.lock().unwrap_or_else(|e| e.into_inner());
                        let h = unsafe { own_rtv(dev, view.0, &view.1, g.get_or_insert_with(HashMap::new)) };
                        if h != 0 {
                            unsafe { om(list, 1, &h, 0, core::ptr::null()) };
                        }
                        h != 0
                    }
                };
                if !ok {
                    break;
                }
                bound = true;
            }
            unsafe { set_pso(list, p) };
            if handle.is_none() {
                LAZY_PASSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
            let h = handle.unwrap_or(view.0);
            unsafe {
                set_vp(list, 1, &vp);
                set_sc(list, 1, &sc);
                set_topo(list, 4);
                draw(list, 3, 1, 0, 0);
            }
            did = true;
            if PASSES.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 4 {
                record("d3d12_xs", "blendclamp", &format!("clamp pass on rtv={h:#x} fmt={fmt} samples={samples} neg={neg}"));
            }
        }
    }
    if !did {
        unsafe { crate::gpuprof::pass_end(list) };
        return;
    }
    unsafe {
        if pso != 0 {
            set_pso(list, pso);
        }
        let dsv_ptr = dsv.as_ref().map_or(core::ptr::null(), |d| d as *const usize);
        om(list, rt_count, if rtv.is_empty() { core::ptr::null() } else { rtv.as_ptr() }, single as i32, dsv_ptr);
        if !vps.is_empty() {
            set_vp(list, vps.len() as u32, vps.as_ptr());
        }
        if !scs.is_empty() {
            set_sc(list, scs.len() as u32, scs.as_ptr());
        }
        if let Some(t) = topo {
            set_topo(list, t);
        }
        crate::gpuprof::pass_end(list);
    }
}

#[cfg(test)]
pub(crate) mod test_api {
    use super::*;
    use core::sync::atomic::Ordering::Relaxed;

    pub(crate) static LOCK: Mutex<()> = Mutex::new(());

    pub(crate) unsafe fn wire(pc_vt: *const usize) {
        TEST_REAL.store(pc_vt as usize, Relaxed);
        for x in [9usize, 10, 12, 13, 16, 17, 19, 20, 21, 22, 25, 26, 34, 50, 59] {
            let pc = crate::cmdlist_wrap::map_slot(x).unwrap();
            NEXT[x].store(unsafe { *pc_vt.add(pc) }, Relaxed);
        }
    }

    pub(crate) fn hook(x: usize) -> usize {
        match x {
            9 => h_close as usize,
            10 => h_reset as usize,
            12 => h_draw as usize,
            13 => h_draw_indexed as usize,
            16 => h_copy_texture as usize,
            17 => h_copy_resource as usize,
            19 => h_resolve as usize,
            20 => h_topo as usize,
            21 => h_viewports as usize,
            22 => h_scissors as usize,
            25 => h_set_pso as usize,
            26 => h_barrier as usize,
            34 => h_root_sig as usize,
            50 => h_targets as usize,
            59 => h_indirect as usize,
            _ => panic!("no blendclamp hook on slot {x}"),
        }
    }

    pub(crate) fn set_ps(on: bool) {
        TEST_PS.store(if on { 2 } else { 1 }, Relaxed);
    }

    pub(crate) fn set_lazy(on: bool) {
        TEST_LAZY.store(if on { 2 } else { 1 }, Relaxed);
    }

    pub(crate) fn reset() {
        TEST_REAL.store(0, Relaxed);
        TEST_LAZY.store(0, Relaxed);
        TEST_PS.store(0, Relaxed);
        *LISTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *PSOS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *OWN_RTV.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *RTVS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn passes() -> u64 {
        PASSES.load(Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blend(src: u32, dst: u32, op: u32, srca: u32, dsta: u32, opa: u32, mask: u8) -> Vec<u8> {
        let mut d = vec![0u8; 0x290];
        for (o, v) in [(0x80usize, 1u32), (0x88, src), (0x8c, dst), (0x90, op), (0x94, srca), (0x98, dsta), (0x9c, opa)] {
            d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        d[0xa4] = mask;
        d
    }

    fn transition(res: usize, before: u32, after: u32) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[8..16].copy_from_slice(&(res as u64).to_le_bytes());
        e[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        e[20..24].copy_from_slice(&before.to_le_bytes());
        e[24..28].copy_from_slice(&after.to_le_bytes());
        e
    }

    #[test]
    fn additive_min_and_max_blends_may_defer_their_clamp() {
        assert!(lazy_safe(&blend(2, 2, 1, 2, 2, 1, 0xf), 0x80, 26));
        assert!(lazy_safe(&blend(5, 2, 1, 2, 2, 1, 0xf), 0x80, 10));
        assert!(lazy_safe(&blend(2, 2, 4, 2, 2, 5, 0xf), 0x80, 10));
        assert!(lazy_safe(&blend(2, 2, 1, 2, 6, 1, 0xf), 0x80, 26));
    }

    #[test]
    fn blends_that_scale_or_subtract_the_destination_clamp_at_every_boundary() {
        assert!(!lazy_safe(&blend(5, 6, 1, 2, 2, 1, 0xf), 0x80, 26));
        assert!(!lazy_safe(&blend(9, 2, 1, 2, 2, 1, 0xf), 0x80, 26));
        assert!(!lazy_safe(&blend(11, 2, 1, 2, 2, 1, 0xf), 0x80, 26));
        assert!(!lazy_safe(&blend(2, 2, 2, 2, 2, 1, 0xf), 0x80, 26));
        assert!(!lazy_safe(&blend(2, 2, 3, 2, 2, 1, 0xf), 0x80, 26));
        assert!(!lazy_safe(&blend(2, 2, 1, 2, 6, 1, 0xf), 0x80, 10));
        assert!(lazy_safe(&blend(2, 2, 1, 2, 6, 1, 0x7), 0x80, 10));
    }

    #[test]
    fn only_a_barrier_that_takes_a_dirty_target_out_of_render_target_flushes_it() {
        let dirty = [0x1000usize, 0x2000];
        let b = [transition(0x3000, 4, 0x80), transition(0x1000, 4, 4)];
        assert_eq!(barrier_hits(&b.concat(), &dirty), Sel::None);
        let b = [transition(0x1000, 0x400, 4)];
        assert_eq!(barrier_hits(&b.concat(), &dirty), Sel::None);
        let b = [transition(0x3000, 4, 0x80), transition(0x2000, 4, 0x80)];
        assert_eq!(barrier_hits(&b.concat(), &dirty), Sel::Only(vec![0x2000]));
        assert_eq!(barrier_hits(&transition(0x2000, 4, 0x80), &[]), Sel::None);
    }

    #[test]
    fn aliasing_uav_and_unknown_barriers_are_handled_conservatively() {
        let dirty = [0x1000usize];
        let mut a = [0u8; 32];
        a[0] = 1;
        a[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
        a[16..24].copy_from_slice(&0x5000u64.to_le_bytes());
        assert_eq!(barrier_hits(&a, &dirty), Sel::Only(vec![0x1000]));
        a[8..16].fill(0);
        assert_eq!(barrier_hits(&a, &dirty), Sel::All);
        let mut u = [0u8; 32];
        u[0] = 2;
        assert_eq!(barrier_hits(&u, &dirty), Sel::None);
        u[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
        assert_eq!(barrier_hits(&u, &dirty), Sel::Only(vec![0x1000]));
        let mut x = [0u8; 32];
        x[0] = 7;
        assert_eq!(barrier_hits(&x, &dirty), Sel::All);
    }
}
