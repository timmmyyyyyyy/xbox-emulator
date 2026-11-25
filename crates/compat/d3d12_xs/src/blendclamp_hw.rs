use core::ffi::c_void;

use crate::blendclamp::test_api;
use crate::device_wrap::Guid;

const IID_DEVICE: Guid = Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
const IID_QUEUE: Guid = Guid { d1: 0x0ec8_70a6, d2: 0x5d7e, d3: 0x4c22, d4: [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed] };
const IID_ALLOCATOR: Guid = Guid { d1: 0x6102_dee4, d2: 0xaf59, d3: 0x4b09, d4: [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24] };
const IID_LIST: Guid = Guid { d1: 0x5b16_0d0f, d2: 0xac1b, d3: 0x4185, d4: [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55] };
const IID_RESOURCE: Guid = Guid { d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };
const IID_FENCE: Guid = Guid { d1: 0x0a75_3dcf, d2: 0xc4d8, d3: 0x4b91, d4: [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76] };
const IID_ROOT_SIGNATURE: Guid = Guid { d1: 0xc54a_6b66, d2: 0x72df, d3: 0x4ee8, d4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14] };
const IID_PSO: Guid = Guid { d1: 0x765a_30f3, d2: 0xf624, d3: 0x4c6f, d4: [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45] };
const IID_DESC_HEAP: Guid = Guid { d1: 0x8efb_471d, d2: 0x616c, d3: 0x4f49, d4: [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51] };
const IID_FACTORY4: Guid = Guid { d1: 0x1bc6_ea02, d2: 0xef36, d3: 0x464f, d4: [0xbf, 0x0c, 0x21, 0xca, 0x39, 0xe5, 0x16, 0x8a] };
const IID_ADAPTER: Guid = Guid { d1: 0x2411_e7e1, d2: 0x12ac, d3: 0x4ccf, d4: [0xbd, 0x14, 0x97, 0x98, 0xe8, 0x53, 0x4d, 0xc0] };

const W: u32 = 64;
const H: u32 = 64;
const FMT_R11G11B10: u32 = 26;
const FMT_RGBA16F: u32 = 10;
const FMT_RGBA32F: u32 = 2;
const ST_RT: u32 = 4;
const ST_COPY_SOURCE: u32 = 0x800;
const ST_COPY_DEST: u32 = 0x400;
const PC_ROOT_CONSTANTS: usize = 36;
const PC_CLEAR_RTV: usize = 48;

extern "system" {
    fn LoadLibraryW(p: *const u16) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
}

unsafe fn slot(o: usize, i: usize) -> usize {
    unsafe { *(*(o as *const *const usize)).add(i) }
}

macro_rules! vc {
    ($o:expr, $i:expr, fn($($t:ty),*) -> $r:ty $(, $a:expr)*) => {{
        let o = $o as usize;
        let f: unsafe extern "system" fn(*mut c_void $(, $t)*) -> $r = core::mem::transmute(slot(o, $i));
        f(o as *mut c_void $(, $a)*)
    }};
}

unsafe fn warp_adapter() -> usize {
    let name: Vec<u16> = "dxgi.dll\0".encode_utf16().collect();
    let h = unsafe { LoadLibraryW(name.as_ptr()) };
    assert!(!h.is_null());
    let p = unsafe { GetProcAddress(h, b"CreateDXGIFactory2\0".as_ptr()) };
    assert!(!p.is_null());
    let create: unsafe extern "system" fn(u32, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(p) };
    let mut f: *mut c_void = core::ptr::null_mut();
    assert!(unsafe { create(0, &IID_FACTORY4, &mut f) } >= 0);
    let mut a: *mut c_void = core::ptr::null_mut();
    assert!(unsafe { vc!(f, 27, fn(*const Guid, *mut *mut c_void) -> i32, &IID_ADAPTER, &mut a) } >= 0);
    a as usize
}

unsafe fn committed(dev: usize, heap: u32, dim: u32, w: u64, h: u32, fmt: u32, flags: u32, state: u32) -> usize {
    let props: [u32; 5] = [heap, 0, 0, 1, 1];
    let mut d = [0u8; 56];
    d[0..4].copy_from_slice(&dim.to_le_bytes());
    d[0x10..0x18].copy_from_slice(&w.to_le_bytes());
    d[0x18..0x1c].copy_from_slice(&h.to_le_bytes());
    d[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    d[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    d[0x20..0x24].copy_from_slice(&fmt.to_le_bytes());
    d[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    d[0x2c..0x30].copy_from_slice(&(if dim == 1 { 1u32 } else { 0 }).to_le_bytes());
    d[0x30..0x34].copy_from_slice(&flags.to_le_bytes());
    let mut clear = [0u8; 24];
    clear[0..4].copy_from_slice(&fmt.to_le_bytes());
    let cv = if dim == 1 { core::ptr::null() } else { clear.as_ptr() };
    let mut r: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { vc!(dev, 27, fn(*const u32, u32, *const u8, u32, *const u8, *const Guid, *mut *mut c_void) -> i32, props.as_ptr(), 0, d.as_ptr(), state, cv, &IID_RESOURCE, &mut r) };
    assert!(hr >= 0 && !r.is_null(), "CreateCommittedResource dim={dim} hr={hr:#010x}");
    r as usize
}

const RS: &str = "#define RS \"RootFlags(0), RootConstants(num32BitConstants=4, b0)\"\n";
const VS: &str = "[RootSignature(RS)]\nfloat4 main(uint id : SV_VertexID) : SV_Position { float2 p = float2((id << 1) & 2, id & 2); return float4(p * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0); }\n";
const PS: &str = "cbuffer C : register(b0) { float4 c; };\nfloat4 main() : SV_Target { return c; }\n";

struct Kit {
    dev: usize,
    queue: usize,
    alloc: usize,
    list: usize,
    fence: usize,
    value: u64,
    rs: usize,
    rt: usize,
    rtv: usize,
    aux: usize,
    rb: usize,
    pso: usize,
    fmt: u32,
}

unsafe fn rtv_for(dev: usize, res: usize) -> usize {
    unsafe {
        let hd = [2u32, 1, 0, 0];
        let mut heap: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 14, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, hd.as_ptr(), &IID_DESC_HEAP, &mut heap) >= 0);
        let mut h = 0usize;
        vc!(heap, 9, fn(*mut usize) -> *mut usize, &mut h);
        vc!(dev, 20, fn(*mut c_void, *const c_void, usize) -> (), res as *mut c_void, core::ptr::null(), h);
        h
    }
}

fn bpp(fmt: u32) -> u32 {
    match fmt {
        FMT_RGBA32F => 16,
        FMT_RGBA16F => 8,
        _ => 4,
    }
}

unsafe fn kit(fmt: u32) -> Kit {
    unsafe { kit_on(warp_adapter(), fmt, true) }
}

unsafe fn kit_on(adapter: usize, fmt: u32, blend: bool) -> Kit {
    unsafe { kit_with(adapter, fmt, blend, PS) }
}

unsafe fn kit_with(adapter: usize, fmt: u32, blend: bool, ps_src: &str) -> Kit {
    unsafe {
        let p = crate::host_proc(b"D3D12CreateDevice\0");
        let create: extern "system" fn(usize, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
        let mut dev: *mut c_void = core::ptr::null_mut();
        let hr = create(adapter, 0xb000, &IID_DEVICE, &mut dev);
        assert!(hr >= 0 && !dev.is_null(), "D3D12CreateDevice (adapter {adapter:#x}) hr={hr:#010x}");
        let dev = dev as usize;
        let qd = [0u32; 4];
        let mut queue: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 8, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &IID_QUEUE, &mut queue) >= 0);
        let mut alloc: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut alloc) >= 0);
        let mut list: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, alloc, core::ptr::null_mut(), &IID_LIST, &mut list) >= 0);
        assert!(vc!(list, 9, fn() -> i32) >= 0);
        let mut fence: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut fence) >= 0);
        let vs = tombox_xshader::dxc::compile(&format!("{RS}{VS}"), "vs_6_0").expect("vs");
        let ps = tombox_xshader::dxc::compile(ps_src, "ps_6_0").unwrap_or_else(|e| panic!("ps: {e}"));
        let mut rs: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, vs.as_ptr(), vs.len(), &IID_ROOT_SIGNATURE, &mut rs) >= 0);
        let rt = committed(dev, 1, 3, W as u64, H, fmt, 1, ST_RT);
        let aux = committed(dev, 1, 3, W as u64, H, fmt, 1, ST_RT);
        let rtv = rtv_for(dev, rt);
        let rb = committed(dev, 3, 1, (W * bpp(fmt) * H) as u64, 1, 0, 0, ST_COPY_DEST);
        let mut d = vec![0u8; 0x290];
        d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
        d[0x08..0x10].copy_from_slice(&(vs.as_ptr() as u64).to_le_bytes());
        d[0x10..0x18].copy_from_slice(&(vs.len() as u64).to_le_bytes());
        d[0x18..0x20].copy_from_slice(&(ps.as_ptr() as u64).to_le_bytes());
        d[0x20..0x28].copy_from_slice(&(ps.len() as u64).to_le_bytes());
        for (o, v) in [(0x80usize, blend as u32), (0x88, 2), (0x8c, 2), (0x90, 1), (0x94, 2), (0x98, 2), (0x9c, 1), (0xa0, 0), (0x1c0, u32::MAX), (0x1c4, 3), (0x1c8, 1), (0x1dc, 1), (0x23c, 3), (0x240, 1), (0x244, fmt), (0x268, 1)] {
            d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        d[0xa4] = 0xf;
        let mut pso: *mut c_void = core::ptr::null_mut();
        let hr = vc!(dev, 10, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &IID_PSO, &mut pso);
        assert!(hr >= 0 && !pso.is_null(), "CreateGraphicsPipelineState hr={hr:#010x}");
        let shaders = vec![std::sync::Arc::new(vs.clone()), std::sync::Arc::new(ps.clone())];
        d[0x08..0x10].copy_from_slice(&(shaders[0].as_ptr() as u64).to_le_bytes());
        d[0x18..0x20].copy_from_slice(&(shaders[1].as_ptr() as u64).to_le_bytes());
        crate::device_wrap::register_gfx_pso(pso, d, shaders);
        Kit { dev, queue: queue as usize, alloc: alloc as usize, list: list as usize, fence: fence as usize, value: 0, rs: rs as usize, rt, rtv, aux, rb, pso: pso as usize, fmt }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Raw,
    Eager,
    Lazy,
}

unsafe fn method(k: &Kit, mode: Mode, x: usize) -> usize {
    if mode == Mode::Raw {
        unsafe { slot(k.list, crate::cmdlist_wrap::map_slot(x).unwrap()) }
    } else {
        test_api::hook(x)
    }
}

unsafe fn transition(k: &Kit, mode: Mode, res: usize, before: u32, after: u32) {
    let mut e = [0u8; 32];
    e[8..16].copy_from_slice(&(res as u64).to_le_bytes());
    e[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    e[20..24].copy_from_slice(&before.to_le_bytes());
    e[24..28].copy_from_slice(&after.to_le_bytes());
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) = unsafe { core::mem::transmute(method(k, mode, 26)) };
    unsafe { f(k.list as *mut c_void, 1, e.as_ptr()) };
}

unsafe fn run(k: &mut Kit, mode: Mode) -> Vec<[u32; 4]> {
    unsafe { run_adds(k, mode, [40000.0, f32::INFINITY, 40000.0]) }
}

unsafe fn run_adds(k: &mut Kit, mode: Mode, adds: [f32; 3]) -> Vec<[u32; 4]> {
    let l = k.list as *mut c_void;
    unsafe {
        assert!(vc!(k.alloc, 8, fn() -> i32) >= 0);
        let reset: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32 = core::mem::transmute(method(k, mode, 10));
        assert!(reset(l, k.alloc as *mut c_void, core::ptr::null_mut()) >= 0);
        vc!(l, PC_CLEAR_RTV, fn(usize, *const f32, u32, *const c_void) -> (), k.rtv, [0f32; 4].as_ptr(), 0, core::ptr::null());
        let targets: unsafe extern "system" fn(*mut c_void, u32, *const usize, i32, *const usize) = core::mem::transmute(method(k, mode, 50));
        targets(l, 1, &k.rtv, 0, core::ptr::null());
        let vp = [0f32, 0.0, W as f32, H as f32, 0.0, 1.0];
        let f: unsafe extern "system" fn(*mut c_void, u32, *const f32) = core::mem::transmute(method(k, mode, 21));
        f(l, 1, vp.as_ptr());
        let sc = [0i32, 0, W as i32, H as i32];
        let f: unsafe extern "system" fn(*mut c_void, u32, *const i32) = core::mem::transmute(method(k, mode, 22));
        f(l, 1, sc.as_ptr());
        let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = core::mem::transmute(method(k, mode, 34));
        f(l, k.rs as *mut c_void);
        let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = core::mem::transmute(method(k, mode, 25));
        f(l, k.pso as *mut c_void);
        let f: unsafe extern "system" fn(*mut c_void, u32) = core::mem::transmute(method(k, mode, 20));
        f(l, 4);
        let draw_add = |v: f32| {
            let c = [v, v, v, 1.0f32];
            vc!(l, PC_ROOT_CONSTANTS, fn(u32, u32, *const f32, u32) -> (), 0, 4, c.as_ptr(), 0);
            let draw: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = core::mem::transmute(method(k, mode, 12));
            draw(l, 3, 1, 0, 0);
        };
        draw_add(adds[0]);
        transition(k, mode, k.aux, ST_RT, ST_COPY_SOURCE);
        transition(k, mode, k.aux, ST_COPY_SOURCE, ST_RT);
        targets(l, 1, &k.rtv, 0, core::ptr::null());
        draw_add(adds[1]);
        transition(k, mode, k.aux, ST_RT, ST_COPY_SOURCE);
        transition(k, mode, k.aux, ST_COPY_SOURCE, ST_RT);
        draw_add(adds[2]);
        transition(k, mode, k.rt, ST_RT, ST_COPY_SOURCE);
        let mut dst = [0u8; 48];
        dst[0..8].copy_from_slice(&(k.rb as u64).to_le_bytes());
        dst[8..12].copy_from_slice(&1u32.to_le_bytes());
        for (o, v) in [(24usize, k.fmt), (28, W), (32, H), (36, 1), (40, W * bpp(k.fmt))] {
            dst[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut src = [0u8; 48];
        src[0..8].copy_from_slice(&(k.rt as u64).to_le_bytes());
        let copy: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void) = core::mem::transmute(method(k, mode, 16));
        copy(l, dst.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
        transition(k, mode, k.rt, ST_COPY_SOURCE, ST_RT);
        let close: unsafe extern "system" fn(*mut c_void) -> i32 = core::mem::transmute(method(k, mode, 9));
        assert!(close(l) >= 0, "Close");
        let lists = [k.list];
        vc!(k.queue, 10, fn(u32, *const usize) -> (), 1, lists.as_ptr());
        k.value += 1;
        assert!(vc!(k.queue, 14, fn(*mut c_void, u64) -> i32, k.fence as *mut c_void, k.value) >= 0);
        let mut n = 0;
        while vc!(k.fence, 8, fn() -> u64) < k.value {
            std::thread::sleep(std::time::Duration::from_millis(1));
            n += 1;
            assert!(n < 20000, "GPU never finished (removed reason {:#010x})", vc!(k.dev, 37, fn() -> i32));
        }
        let mut m: *mut u8 = core::ptr::null_mut();
        assert!(vc!(k.rb, 8, fn(u32, *const c_void, *mut *mut u8) -> i32, 0, core::ptr::null(), &mut m) >= 0);
        let bytes = core::slice::from_raw_parts(m, (W * H * bpp(k.fmt)) as usize);
        let out = bytes.chunks_exact(bpp(k.fmt) as usize).map(|px| channels(k.fmt, px)).collect();
        vc!(k.rb, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
        out
    }
}

fn channels(fmt: u32, px: &[u8]) -> [u32; 4] {
    if fmt == FMT_RGBA32F {
        let w = |i: usize| u32::from_le_bytes(px[4 * i..4 * i + 4].try_into().unwrap());
        [w(0), w(1), w(2), w(3)]
    } else if fmt == FMT_RGBA16F {
        let h = |i: usize| u16::from_le_bytes([px[2 * i], px[2 * i + 1]]) as u32;
        [h(0), h(1), h(2), h(3)]
    } else {
        let p = u32::from_le_bytes(px.try_into().unwrap());
        [p & 0x7ff, (p >> 11) & 0x7ff, p >> 22, 0]
    }
}

fn is_inf(fmt: u32, c: [u32; 4]) -> bool {
    if fmt == FMT_RGBA16F { c[0] & 0x7fff == 0x7c00 } else { c[0] == 0x7c0 }
}

fn largest(fmt: u32) -> [u32; 4] {
    if fmt == FMT_RGBA16F { [0x7bff, 0x7bff, 0x7bff, 0x4200] } else { [0x7bf, 0x7bf, 0x3df, 0] }
}

fn gpu_free() -> bool {
    let dir = std::path::Path::new(r"D:\TomBox\tools\dev");
    let busy = std::fs::read_dir(dir).map(|it| it.flatten().any(|e| {
        let n = e.file_name().to_string_lossy().to_ascii_uppercase();
        n.starts_with("GAME_LOCK") || n.starts_with("USER_PLAYING")
    })).unwrap_or(false);
    if busy {
        println!("GPU test skipped: a GAME_LOCK or USER_PLAYING file exists in {}", dir.display());
    }
    !busy
}

struct Outcome {
    raw_inf: usize,
    eager: Vec<[u32; 4]>,
    lazy: Vec<[u32; 4]>,
    eager_passes: u64,
    lazy_passes: u64,
    lazy_own: u64,
}

unsafe fn chain(fmt: u32) -> Outcome {
    unsafe {
        test_api::reset();
        let mut k = kit(fmt);
        test_api::wire(*(k.list as *const *const usize));
        let raw = run(&mut k, Mode::Raw);
        test_api::set_lazy(false);
        let p0 = test_api::passes();
        let eager = run(&mut k, Mode::Eager);
        let eager_passes = test_api::passes() - p0;
        test_api::set_lazy(true);
        crate::blendclamp::note_rtv(k.rtv, k.rt, core::ptr::null());
        let p0 = test_api::passes();
        let l0 = crate::blendclamp::lazy_passes();
        let lazy = run(&mut k, Mode::Lazy);
        let lazy_passes = test_api::passes() - p0;
        let lazy_own = crate::blendclamp::lazy_passes() - l0;
        test_api::reset();
        Outcome { raw_inf: raw.iter().filter(|&&c| is_inf(fmt, c)).count(), eager, lazy, eager_passes, lazy_passes, lazy_own }
    }
}

fn check(fmt: u32, o: &Outcome, per_flush: u64, bad: &mut Vec<String>) {
    for (i, (e, z)) in o.eager.iter().zip(o.lazy.iter()).enumerate() {
        if e != z && bad.len() < 8 {
            bad.push(format!("fmt {fmt} pixel {i}: eager {e:x?} lazy {z:x?}"));
        }
    }
    for (i, e) in o.eager.iter().enumerate() {
        if *e != largest(fmt) && bad.len() < 16 {
            bad.push(format!("fmt {fmt} pixel {i}: eager {e:x?} want {:x?}", largest(fmt)));
        }
    }
    if o.eager_passes != 3 * per_flush {
        bad.push(format!("fmt {fmt}: eager ran {} clamp passes, want {} (two unrelated barriers and the read barrier)", o.eager_passes, 3 * per_flush));
    }
    if o.lazy_passes != per_flush || o.lazy_own != per_flush {
        bad.push(format!("fmt {fmt}: lazy ran {} clamp passes ({} through its own view), want {per_flush} at the read barrier", o.lazy_passes, o.lazy_own));
    }
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_blendclamp -- --ignored"]
fn warp_blendclamp_lazy_matches_eager_for_an_additive_chain_that_overflows_to_inf() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: WARP blendclamp test not run");
        return;
    }
    let _g = test_api::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut bad = Vec::new();
    let half = unsafe { chain(FMT_RGBA16F) };
    if half.raw_inf == 0 {
        bad.push("fmt 10: the unclamped control never reached Inf, so the chain does not exercise the clamp".to_string());
    }
    check(FMT_RGBA16F, &half, 2, &mut bad);
    let small = unsafe { chain(FMT_R11G11B10) };
    if small.raw_inf == 0 {
        bad.push("fmt 26: the unclamped control never reached Inf, so the chain does not exercise the clamp".to_string());
    }
    check(FMT_R11G11B10, &small, 1, &mut bad);
    assert!(bad.is_empty(), "{}", bad.join("
"));
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_blendclamp -- --ignored"]
fn warp_blendclamp_leaves_fp32_targets_above_the_fp16_range() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: WARP blendclamp fp32 test not run");
        return;
    }
    let _g = test_api::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (raw, eager, passes) = unsafe {
        test_api::reset();
        let mut k = kit(FMT_RGBA32F);
        test_api::wire(*(k.list as *const *const usize));
        let raw = run_adds(&mut k, Mode::Raw, [40000.0, 40000.0, 1.0]);
        test_api::set_lazy(false);
        let p0 = test_api::passes();
        let eager = run_adds(&mut k, Mode::Eager, [40000.0, 40000.0, 1.0]);
        let passes = test_api::passes() - p0;
        test_api::reset();
        (raw, eager, passes)
    };
    let want = [80001.0f32.to_bits(), 80001.0f32.to_bits(), 80001.0f32.to_bits(), 3.0f32.to_bits()];
    let centre = (H / 2 * W + W / 2) as usize;
    assert!(raw.iter().all(|&p| p == want), "raw control: centre {:x?}, want {want:x?}", raw[centre]);
    assert!(eager.iter().all(|&p| p == want), "eager: centre {:x?} ({}), want {want:x?} (80001.0)", eager[centre], f32::from_bits(eager[centre][0]));
    assert_eq!(passes, 0, "eager ran clamp passes on an R32G32B32A32_FLOAT target");
}

fn verdict(fmt: u32, c: [u32; 4]) -> &'static str {
    let (exp_all, mant) = if fmt == FMT_RGBA16F { (c[0] & 0x7c00 == 0x7c00, c[0] & 0x3ff) } else { (c[0] >> 6 == 0x1f, c[0] & 0x3f) };
    if exp_all && mant != 0 {
        "NaN"
    } else if exp_all {
        "Inf"
    } else if c[..3] == largest(fmt)[..3] {
        "max-finite"
    } else if c[0] == 0 {
        "zero"
    } else {
        "other"
    }
}

unsafe fn matrix(adapter: usize, who: &str) -> Vec<String> {
    let cases: [(&str, bool, Mode, [f32; 3]); 7] = [
        ("blended 40000+40000", true, Mode::Raw, [40000.0, 40000.0, 0.0]),
        ("unblended write 100000", false, Mode::Raw, [100000.0; 3]),
        ("unblended shader Inf", false, Mode::Raw, [f32::INFINITY; 3]),
        ("blended 0+shader Inf", true, Mode::Raw, [f32::INFINITY, 0.0, 0.0]),
        ("blended 0+shader NaN", true, Mode::Raw, [f32::NAN, 0.0, 0.0]),
        ("blendclamp eager on shader Inf", true, Mode::Eager, [f32::INFINITY, 0.0, 0.0]),
        ("blendclamp eager on shader NaN", true, Mode::Eager, [f32::NAN, 0.0, 0.0]),
    ];
    let mut lines = Vec::new();
    for (fmt, name) in [(FMT_RGBA16F, "R16G16B16A16_FLOAT"), (FMT_R11G11B10, "R11G11B10_FLOAT")] {
        for (what, blend, mode, adds) in cases {
            unsafe {
                test_api::reset();
                let mut k = kit_on(adapter, fmt, blend);
                test_api::wire(*(k.list as *const *const usize));
                test_api::set_lazy(false);
                let img = run_adds(&mut k, mode, adds);
                test_api::reset();
                let c = img[(H / 2 * W + W / 2) as usize];
                let kinds: std::collections::BTreeMap<&str, usize> = img.iter().fold(Default::default(), |mut m, &p| {
                    *m.entry(verdict(fmt, p)).or_default() += 1;
                    m
                });
                lines.push(format!("{who} blend probe {name}: {what} -> {} (centre {c:x?}, all {kinds:?})", verdict(fmt, c)));
            }
        }
    }
    lines
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_blend_overflow_matrix -- --ignored --nocapture"]
fn warp_blend_overflow_matrix() {
    if !gpu_free() || !tombox_xshader::dxc::available() {
        return;
    }
    let _g = test_api::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for l in unsafe { matrix(warp_adapter(), "WARP") } {
        println!("{l}");
    }
}

#[test]
#[ignore = "real GPU: cargo test -p d3d12_xs --lib hw_blend_overflow_probe -- --ignored --nocapture"]
fn hw_blend_overflow_probe() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: HW blend overflow probe not run");
        return;
    }
    let _g = test_api::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for l in unsafe { matrix(0, "HW") } {
        println!("{l}");
    }
    let k = unsafe { kit_on(0, FMT_RGBA16F, true) };
    println!("HW blend_probe (the TOMBOX_BLEND_CLAMP=auto detector): {:?}", unsafe { crate::blend_probe::probe(k.dev as *mut c_void) });
}

fn ps_clamp_source(fmt: u32) -> String {
    let body = format!(
        "{}\ncbuffer C : register(b0) {{ float4 c; }};\nstruct O {{ float4 o0 : SV_Target0; }};\nO main() {{\n    O o;\n    o.o0 = c;\n{}    return o;\n}}\n",
        tombox_xshader::hlsl::ps_helpers(),
        tombox_xshader::hlsl::ps_clamp_epilogue(&[(0, 0, 4)])
    );
    let mut rtfmt = [0u8; 8];
    rtfmt[0] = crate::shader_xlat::export_kind(fmt, true);
    tombox_xshader::runtime_hlsl::finalize(&body, &rtfmt, true)
}

#[test]
fn ps_clamp_kinds_only_change_the_small_float_formats() {
    for (fmt, off, on) in [(10u32, 1u8, 3u8), (34, 1, 3), (54, 1, 3), (26, 2, 4), (2, 0, 0), (28, 0, 0)] {
        assert_eq!(crate::shader_xlat::export_kind(fmt, false), off, "fmt {fmt}");
        assert_eq!(crate::shader_xlat::export_kind(fmt, true), on, "fmt {fmt}");
    }
    let src = ps_clamp_source(26);
    assert!(src.contains("#define TB_RTFMT0 4u"), "{}", &src[..200.min(src.len())]);
    assert!(src.contains("o.o0.z = tb_psclamp(o.o0.z, TB_RTFMT0, 2u);"));
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_ps_clamp -- --ignored"]
fn warp_ps_clamp_matches_eager_blendclamp_for_shader_inf_and_nan() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: WARP ps-clamp test not run");
        return;
    }
    let _g = test_api::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let cases: [(&str, [f32; 3]); 5] = [
        ("+Inf", [f32::INFINITY, 0.0, 0.0]),
        ("NaN", [f32::NAN, 0.0, 0.0]),
        ("-Inf", [f32::NEG_INFINITY, 0.0, 0.0]),
        ("40000 then +Inf then 40000", [40000.0, f32::INFINITY, 40000.0]),
        ("NaN then 40000", [f32::NAN, 40000.0, 0.0]),
    ];
    let mut bad = Vec::new();
    for fmt in [FMT_RGBA16F, FMT_R11G11B10] {
        let src = ps_clamp_source(fmt);
        for (what, adds) in cases {
            unsafe {
                test_api::reset();
                test_api::set_lazy(false);
                test_api::set_ps(false);
                let mut k = kit_on(warp_adapter(), fmt, true);
                test_api::wire(*(k.list as *const *const usize));
                let p0 = test_api::passes();
                let eager = run_adds(&mut k, Mode::Eager, adds);
                let eager_passes = test_api::passes() - p0;
                test_api::reset();
                test_api::set_ps(true);
                let mut k = kit_with(warp_adapter(), fmt, true, &src);
                test_api::wire(*(k.list as *const *const usize));
                let p0 = test_api::passes();
                let ps = run_adds(&mut k, Mode::Eager, adds);
                let ps_passes = test_api::passes() - p0;
                test_api::reset();
                if eager_passes == 0 {
                    bad.push(format!("fmt {fmt} {what}: eager ran no clamp pass, so the comparison proves nothing"));
                }
                if ps_passes != 0 {
                    bad.push(format!("fmt {fmt} {what}: ps mode still ran {ps_passes} full-target clamp passes"));
                }
                let diff = eager.iter().zip(ps.iter()).filter(|(a, b)| a != b).count();
                if diff != 0 {
                    bad.push(format!("fmt {fmt} {what}: {diff} pixels differ, eager {:x?} ({}) vs ps {:x?} ({})", eager[0], verdict(fmt, eager[0]), ps[0], verdict(fmt, ps[0])));
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_blend_probe -- --ignored"]
fn warp_blend_probe_detects_saturating_overflow() {
    if !gpu_free() || !tombox_xshader::dxc::available() {
        return;
    }
    let _g = test_api::LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let k = unsafe { kit(FMT_RGBA16F) };
    let t0 = std::time::Instant::now();
    let r = unsafe { crate::blend_probe::probe(k.dev as *mut c_void) };
    println!("WARP blend_probe: {r:?} in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    assert_eq!(r, Ok(crate::blend_probe::Overflow::Saturates));
}
