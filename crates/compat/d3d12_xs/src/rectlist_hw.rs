use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

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
const IID_DEVICE2: Guid = Guid { d1: 0x30ba_a41e, d2: 0xb15b, d3: 0x475c, d4: [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28] };
const IID_ADAPTER: Guid = Guid { d1: 0x2411_e7e1, d2: 0x12ac, d3: 0x4ccf, d4: [0xbd, 0x14, 0x97, 0x98, 0xe8, 0x53, 0x4d, 0xc0] };

const W: u32 = 64;
const H: u32 = 64;
const FMT_RGBA32F: u32 = 2;
const FMT_R32_UINT: u32 = 42;
const PITCH: u32 = W * 16;
const ST_RT: u32 = 4;
const ST_COPY_SOURCE: u32 = 0x800;
const ST_COPY_DEST: u32 = 0x400;
const ST_GENERIC_READ: u32 = 0xac3;
const X_TOPOLOGY: usize = 20;
const X_VIEWPORTS: usize = 21;
const X_SCISSORS: usize = 22;
const X_PSO: usize = 25;
const X_GROOT: usize = 34;
const X_GN: usize = 40;
const X_IB: usize = 47;
const X_RT: usize = 50;
const X_DEPTH_BIAS: usize = 94;
const BIAS_SENTINEL: usize = 0x5eed;
const PC_CLOSE: usize = 9;
const PC_RESET: usize = 10;
const PC_DRAW: usize = 12;
const PC_DRAW_INDEXED: usize = 13;
const PC_COPY_TEXTURE: usize = 16;
const PC_BARRIER: usize = 26;
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

const RS: &str = "#define RS \"RootFlags(0), RootConstants(num32BitConstants=8, b0)\"\n";
const VS: &str = r#"cbuffer C : register(b0) { float x0; float y0; float x1; float y1; float w0; float w1; float w2; float pad; };
struct O { float4 p : SV_Position; float2 uv : TEXCOORD0; };
[RootSignature(RS)]
O main(uint v : SV_VertexID) {
    uint k = v % 3;
    float2 c = k == 0 ? float2(0, 0) : (k == 1 ? float2(0, 1) : float2(1, 0));
    float w = k == 0 ? w0 : (k == 1 ? w1 : w2);
    O o;
    o.p = float4(lerp(x0, x1, c.x) * w, lerp(y0, y1, c.y) * w, 0.5 * w, w);
    o.uv = c;
    return o;
}
"#;
const PS: &str = "float4 main(float4 p : SV_Position, float2 uv : TEXCOORD0) : SV_Target { return float4(uv, 0.25, 1); }\n";

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
    rb: usize,
    ib: usize,
    vt: Vec<AtomicUsize>,
    vs: Vec<u8>,
    ps: Vec<u8>,
}

unsafe fn kit() -> Kit {
    unsafe {
        let p = crate::host_proc(b"D3D12CreateDevice\0");
        let create: extern "system" fn(usize, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
        let mut dev: *mut c_void = core::ptr::null_mut();
        let hr = create(warp_adapter(), 0xb000, &IID_DEVICE, &mut dev);
        assert!(hr >= 0 && !dev.is_null(), "D3D12CreateDevice (WARP) hr={hr:#010x}");
        let dev = dev as usize;
        let qd = [0u32; 4];
        let mut queue: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 8, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &IID_QUEUE, &mut queue) >= 0);
        let mut alloc: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut alloc) >= 0);
        let mut list: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, alloc, core::ptr::null_mut(), &IID_LIST, &mut list) >= 0);
        let mut fence: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut fence) >= 0);
        let vs = tombox_xshader::dxc::compile(&format!("{RS}{VS}"), "vs_6_0").expect("vs");
        let ps = tombox_xshader::dxc::compile(PS, "ps_6_0").expect("ps");
        let mut rs: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, vs.as_ptr(), vs.len(), &IID_ROOT_SIGNATURE, &mut rs) >= 0);
        let rt = committed(dev, 1, 3, W as u64, H, FMT_RGBA32F, 1, ST_RT);
        let hd = [2u32, 1, 0, 0];
        let mut heap: *mut c_void = core::ptr::null_mut();
        assert!(vc!(dev, 14, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, hd.as_ptr(), &IID_DESC_HEAP, &mut heap) >= 0);
        let mut rtv = 0usize;
        vc!(heap, 9, fn(*mut usize) -> *mut usize, &mut rtv);
        vc!(dev, 20, fn(*mut c_void, *const c_void, usize) -> (), rt as *mut c_void, core::ptr::null(), rtv);
        let rb = committed(dev, 3, 1, (PITCH * H) as u64, 1, 0, 0, ST_COPY_DEST);
        let ib = committed(dev, 2, 1, 256, 1, 0, 0, ST_GENERIC_READ);
        let mut m: *mut u32 = core::ptr::null_mut();
        assert!(vc!(ib, 8, fn(u32, *const c_void, *mut *mut u32) -> i32, 0, core::ptr::null(), &mut m) >= 0);
        core::ptr::copy_nonoverlapping([0u32, 1, 2].as_ptr(), m, 3);
        vc!(ib, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
        let lvt = *(list as *const *const usize);
        let vt: Vec<AtomicUsize> = (0..128).map(|_| AtomicUsize::new(0)).collect();
        for (x, pc) in [(X_TOPOLOGY, 20usize), (X_VIEWPORTS, 21), (X_SCISSORS, 22), (X_PSO, 25), (X_GROOT, 30), (X_GN, 36), (X_IB, 43), (X_RT, 46)] {
            vt[x].store(*lvt.add(pc), Ordering::Relaxed);
        }
        vt[X_DEPTH_BIAS].store(BIAS_SENTINEL, Ordering::Relaxed);
        Kit { dev, queue: queue as usize, alloc: alloc as usize, list: list as usize, fence: fence as usize, value: 0, rs: rs as usize, rt, rtv, rb, ib, vt, vs, ps }
    }
}

unsafe fn x<F: Copy>(k: &Kit, s: usize) -> F {
    let p = k.vt[s].load(Ordering::Relaxed);
    assert!(p != 0 && p != BIAS_SENTINEL, "slot {s} not wired");
    unsafe { core::mem::transmute_copy(&p) }
}

fn put(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

unsafe fn pso(k: &Kit) -> usize {
    let mut d = vec![0u8; 0x290];
    d[0..8].copy_from_slice(&(k.rs as u64).to_le_bytes());
    d[0x08..0x10].copy_from_slice(&(k.vs.as_ptr() as u64).to_le_bytes());
    d[0x10..0x18].copy_from_slice(&(k.vs.len() as u64).to_le_bytes());
    d[0x18..0x20].copy_from_slice(&(k.ps.as_ptr() as u64).to_le_bytes());
    d[0x20..0x28].copy_from_slice(&(k.ps.len() as u64).to_le_bytes());
    put(&mut d, 0x80, 1);
    for (o, v) in [(0x88, 2u32), (0x8c, 2), (0x90, 1), (0x94, 2), (0x98, 2), (0x9c, 1), (0xa0, 0)] {
        put(&mut d, o, v);
    }
    d[0xa4] = 0xf;
    put(&mut d, 0x1c0, u32::MAX);
    put(&mut d, 0x1c4, 3);
    put(&mut d, 0x1c8, 1);
    put(&mut d, 0x1dc, 1);
    put(&mut d, 0x23c, 3);
    put(&mut d, 0x240, 1);
    put(&mut d, 0x244, FMT_RGBA32F);
    put(&mut d, 0x268, 1);
    unsafe { crate::cl2_state::patch_gfx_pso_desc(k.dev as *mut c_void, &mut d) };
    let mut p: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { vc!(k.dev, 10, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &IID_PSO, &mut p) };
    assert!(hr >= 0 && !p.is_null(), "CreateGraphicsPipelineState hr={hr:#010x}");
    let shaders = vec![std::sync::Arc::new(k.vs.clone()), std::sync::Arc::new(k.ps.clone())];
    d[0x08..0x10].copy_from_slice(&(shaders[0].as_ptr() as u64).to_le_bytes());
    d[0x18..0x20].copy_from_slice(&(shaders[1].as_ptr() as u64).to_le_bytes());
    unsafe { crate::device_wrap::register_gfx_pso(p, d, shaders) };
    p as usize
}

unsafe fn barrier(k: &Kit, a: u32, b: u32) {
    let mut d = [0u8; 32];
    d[8..16].copy_from_slice(&(k.rt as u64).to_le_bytes());
    d[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    d[20..24].copy_from_slice(&a.to_le_bytes());
    d[24..28].copy_from_slice(&b.to_le_bytes());
    unsafe { vc!(k.list, PC_BARRIER, fn(u32, *const u8) -> (), 1, d.as_ptr()) };
}

#[derive(Default)]
struct Draw {
    topology: u32,
    indexed: bool,
    pso_after_topology: bool,
    depth_bias: bool,
    c: [f32; 8],
}

unsafe fn run(k: &mut Kit, p: usize, draws: &[Draw]) -> Vec<[f32; 4]> {
    let l = k.list as *mut c_void;
    unsafe {
        crate::cl2_state::on_new_list(l);
        vc!(l, PC_CLEAR_RTV, fn(usize, *const f32, u32, *const c_void) -> (), k.rtv, [0f32; 4].as_ptr(), 0, core::ptr::null());
        let rt: unsafe extern "system" fn(*mut c_void, u32, *const usize, i32, *const usize) = x(k, X_RT);
        rt(l, 1, &k.rtv, 0, core::ptr::null());
        let vp = [0f32, 0.0, W as f32, H as f32, 0.0, 1.0];
        let f: unsafe extern "system" fn(*mut c_void, u32, *const f32) = x(k, X_VIEWPORTS);
        f(l, 1, vp.as_ptr());
        let sc = [0i32, 0, W as i32, H as i32];
        let f: unsafe extern "system" fn(*mut c_void, u32, *const i32) = x(k, X_SCISSORS);
        f(l, 1, sc.as_ptr());
        let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = x(k, X_GROOT);
        f(l, k.rs as *mut c_void);
        let set_pso: unsafe extern "system" fn(*mut c_void, *mut c_void) = x(k, X_PSO);
        set_pso(l, p as *mut c_void);
        let mut ibv = [0u8; 16];
        ibv[0..8].copy_from_slice(&vc!(k.ib, 11, fn() -> u64).to_le_bytes());
        ibv[8..12].copy_from_slice(&12u32.to_le_bytes());
        ibv[12..16].copy_from_slice(&FMT_R32_UINT.to_le_bytes());
        let f: unsafe extern "system" fn(*mut c_void, *const u8) = x(k, X_IB);
        f(l, ibv.as_ptr());
        for d in draws {
            let f: unsafe extern "system" fn(*mut c_void, u32) = x(k, X_TOPOLOGY);
            f(l, d.topology);
            if d.pso_after_topology {
                set_pso(l, p as *mut c_void);
            }
            if d.depth_bias {
                let f: unsafe extern "system" fn(*mut c_void, i32, f32, f32, u32) = x(k, X_DEPTH_BIAS);
                f(l, 1, 0.0, 0.0, 0);
            }
            let f: unsafe extern "system" fn(*mut c_void, u32, u32, *const f32, u32) = x(k, X_GN);
            f(l, 0, 8, d.c.as_ptr(), 0);
            if d.indexed {
                vc!(l, PC_DRAW_INDEXED, fn(u32, u32, u32, i32, u32) -> (), 3, 1, 0, 0, 0);
            } else {
                vc!(l, PC_DRAW, fn(u32, u32, u32, u32) -> (), 3, 1, 0, 0);
            }
        }
        barrier(k, ST_RT, ST_COPY_SOURCE);
        let mut dst = [0u8; 48];
        dst[0..8].copy_from_slice(&(k.rb as u64).to_le_bytes());
        dst[8..12].copy_from_slice(&1u32.to_le_bytes());
        for (o, v) in [(24usize, FMT_RGBA32F), (28, W), (32, H), (36, 1), (40, PITCH)] {
            put(&mut dst, o, v);
        }
        let mut src = [0u8; 48];
        src[0..8].copy_from_slice(&(k.rt as u64).to_le_bytes());
        vc!(l, PC_COPY_TEXTURE, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), dst.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
        barrier(k, ST_COPY_SOURCE, ST_RT);
        assert!(vc!(l, PC_CLOSE, fn() -> i32) >= 0, "Close");
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
        assert!(vc!(k.alloc, 8, fn() -> i32) >= 0);
        assert!(vc!(l, PC_RESET, fn(*mut c_void, *mut c_void) -> i32, k.alloc as *mut c_void, core::ptr::null_mut()) >= 0);
        let mut m: *mut f32 = core::ptr::null_mut();
        assert!(vc!(k.rb, 8, fn(u32, *const c_void, *mut *mut f32) -> i32, 0, core::ptr::null(), &mut m) >= 0);
        let mut out = Vec::with_capacity((W * H) as usize);
        for y in 0..H as usize {
            for xx in 0..W as usize {
                let p = m.add(y * (PITCH as usize / 4) + xx * 4);
                out.push([*p, *p.add(1), *p.add(2), *p.add(3)]);
            }
        }
        vc!(k.rb, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
        out
    }
}

fn expected(c: &[f32; 8], px: u32, py: u32) -> Option<[f64; 2]> {
    let (x0, y0, x1, y1) = (c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64);
    let nx = (px as f64 + 0.5) / (W as f64 / 2.0) - 1.0;
    let ny = 1.0 - (py as f64 + 0.5) / (H as f64 / 2.0);
    let l2 = (nx - x0) / (x1 - x0);
    let l1 = (ny - y0) / (y1 - y0);
    if !(0.0..=1.0).contains(&l1) || !(0.0..=1.0).contains(&l2) {
        return None;
    }
    let l = [1.0 - l1 - l2, l1, l2];
    let q = [1.0 / c[4] as f64, 1.0 / c[5] as f64, 1.0 / c[6] as f64];
    let uv = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0]];
    let den: f64 = (0..3).map(|i| l[i] * q[i]).sum();
    let u: f64 = (0..3).map(|i| l[i] * q[i] * uv[i][0]).sum::<f64>() / den;
    let v: f64 = (0..3).map(|i| l[i] * q[i] * uv[i][1]).sum::<f64>() / den;
    Some([u, v])
}

fn check_rect(img: &[[f32; 4]], c: &[f32; 8], what: &str, bad: &mut Vec<String>) {
    let mut covered = 0;
    for py in 0..H {
        for px in 0..W {
            let got = img[(py * W + px) as usize];
            match expected(c, px, py) {
                Some([u, v]) => {
                    covered += 1;
                    if (got[2] != 0.25 || got[3] != 1.0 || (got[0] as f64 - u).abs() > 2e-4 || (got[1] as f64 - v).abs() > 2e-4) && bad.len() < 24 {
                        bad.push(format!("{what}: pixel ({px},{py}) = {got:?}, want uv ({u:.5},{v:.5}) b=0.25 a=1"));
                    }
                }
                None => {
                    if got != [0.0; 4] && bad.len() < 24 {
                        bad.push(format!("{what}: pixel ({px},{py}) outside the rect = {got:?}"));
                    }
                }
            }
        }
    }
    if covered != 32 * 32 {
        bad.push(format!("{what}: {covered} pixels inside, want 1024"));
    }
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

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_rectlist -- --ignored"]
fn warp_rectlist_topology_17_covers_all_four_corners() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: WARP rect-list test not run");
        return;
    }
    let _g = crate::cl2_state::TEST_PREV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut k = kit();
        let saved = crate::cl2_state::test_install(&k.vt);
        let mut defaults = vec![0usize; 128];
        defaults[X_DEPTH_BIAS] = BIAS_SENTINEL;
        crate::cl2_state::install(&k.vt, &defaults);
        let p = pso(&k);
        let bad = exercise(&mut k, p);
        crate::cl2_state::test_restore(saved);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_rectlist -- --ignored"]
fn warp_rectlist_topology_17_on_a_stream_built_pso_covers_all_four_corners() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: WARP rect-list stream test not run");
        return;
    }
    let _g = crate::cl2_state::TEST_PREV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut k = kit();
        let saved = crate::cl2_state::test_install(&k.vt);
        let mut defaults = vec![0usize; 128];
        defaults[X_DEPTH_BIAS] = BIAS_SENTINEL;
        crate::cl2_state::install(&k.vt, &defaults);
        let p = stream_pso(&k);
        let bad = exercise(&mut k, p);
        crate::cl2_state::test_restore(saved);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[test]
#[ignore = "needs D3D12 WARP: cargo test -p d3d12_xs --lib warp_rectlist -- --ignored"]
fn warp_rectlist_compute_pso_bound_under_topology_17_is_skipped_quietly() {
    if !gpu_free() {
        return;
    }
    if !tombox_xshader::dxc::available() {
        println!("DXC not available: WARP rect-list compute test not run");
        return;
    }
    let _g = crate::cl2_state::TEST_PREV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut k = kit();
        let saved = crate::cl2_state::test_install(&k.vt);
        let mut defaults = vec![0usize; 128];
        defaults[X_DEPTH_BIAS] = BIAS_SENTINEL;
        crate::cl2_state::install(&k.vt, &defaults);
        let known = compute_pso(&k, true);
        let unknown = compute_pso(&k, false);
        let l = k.list as *mut c_void;
        crate::cl2_state::on_new_list(l);
        let topo: unsafe extern "system" fn(*mut c_void, u32) = x(&k, X_TOPOLOGY);
        topo(l, 17);
        let set_pso: unsafe extern "system" fn(*mut c_void, *mut c_void) = x(&k, X_PSO);
        set_pso(l, known as *mut c_void);
        set_pso(l, unknown as *mut c_void);
        let mut bad = Vec::new();
        if crate::rectlist::was_refused(known as u64) {
            bad.push(format!("registered compute PSO {known:#x} was reported as an unknown rect-list pipeline"));
        }
        if !crate::rectlist::was_refused(unknown as u64) {
            bad.push(format!("unregistered PSO {unknown:#x} was skipped instead of refused"));
        }
        let p = pso(&k);
        let flat = [-0.5f32, 0.5, 0.5, -0.5, 1.0, 1.0, 1.0, 0.0];
        let img = run(&mut k, p, &[Draw { topology: 17, pso_after_topology: true, c: flat, ..Draw::default() }]);
        check_rect(&img, &flat, "graphics rect after a compute PSO under topology 17", &mut bad);
        crate::cl2_state::test_restore(saved);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

unsafe fn compute_pso(k: &Kit, register: bool) -> usize {
    let cs = tombox_xshader::dxc::compile("[RootSignature(\"RootFlags(0)\")]\n[numthreads(1, 1, 1)]\nvoid main() {}\n", "cs_6_0").expect("cs");
    let mut rs: *mut c_void = core::ptr::null_mut();
    assert!(unsafe { vc!(k.dev, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, cs.as_ptr(), cs.len(), &IID_ROOT_SIGNATURE, &mut rs) } >= 0);
    let mut d = vec![0u8; crate::pso_serial::COMPUTE_DESC_SIZE];
    d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
    d[0x08..0x10].copy_from_slice(&(cs.as_ptr() as u64).to_le_bytes());
    d[0x10..0x18].copy_from_slice(&(cs.len() as u64).to_le_bytes());
    let mut p: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { vc!(k.dev, 11, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &IID_PSO, &mut p) };
    assert!(hr >= 0 && !p.is_null(), "CreateComputePipelineState hr={hr:#010x}");
    if register {
        crate::device_x::register_compute(p, &d, std::sync::Arc::new(cs));
    }
    p as usize
}

unsafe fn stream_pso(k: &Kit) -> usize {
    use crate::dbcount::append_sub;
    let shaders = vec![std::sync::Arc::new(k.vs.clone()), std::sync::Arc::new(k.ps.clone())];
    let code = |c: &[u8]| [(c.as_ptr() as u64).to_le_bytes(), (c.len() as u64).to_le_bytes()].concat();
    let mut rast = [0u8; 44];
    put(&mut rast, 0, 3);
    put(&mut rast, 4, 1);
    put(&mut rast, 24, 1);
    let mut rtf = [0u8; 36];
    put(&mut rtf, 0, FMT_RGBA32F);
    put(&mut rtf, 32, 1);
    let mut s = Vec::new();
    let mut subs = Vec::new();
    append_sub(&mut s, &mut subs, 0, &(k.rs as u64).to_le_bytes());
    append_sub(&mut s, &mut subs, 1, &code(&shaders[0]));
    append_sub(&mut s, &mut subs, 2, &code(&shaders[1]));
    append_sub(&mut s, &mut subs, 10, &rast);
    append_sub(&mut s, &mut subs, 11, &[0u8; 52]);
    append_sub(&mut s, &mut subs, 14, &3u32.to_le_bytes());
    append_sub(&mut s, &mut subs, 15, &rtf);
    let mut d2: *mut c_void = core::ptr::null_mut();
    assert!(unsafe { vc!(k.dev, 0, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE2, &mut d2) } >= 0 && !d2.is_null());
    let desc = [s.len() as u64, s.as_ptr() as u64];
    let mut p: *mut c_void = core::ptr::null_mut();
    let hr = unsafe { vc!(d2, 47, fn(*const [u64; 2], *const Guid, *mut *mut c_void) -> i32, &desc, &IID_PSO, &mut p) };
    unsafe { vc!(d2, 2, fn() -> u32) };
    assert!(hr >= 0 && !p.is_null(), "CreatePipelineState hr={hr:#010x}");
    crate::dbcount::note_stream_pso(p as usize, &s, &shaders);
    assert!(crate::device_wrap::gfx_pso_desc(p as usize).is_none());
    p as usize
}

unsafe fn exercise(k: &mut Kit, p: usize) -> Vec<String> {
    unsafe {
        let flat = [-0.5f32, 0.5, 0.5, -0.5, 1.0, 1.0, 1.0, 0.0];
        let persp = [-0.5f32, 0.5, 0.5, -0.5, 2.0, 1.5, 1.5, 0.0];
        let mut bad = Vec::new();
        let img = run(k, p, &[Draw { topology: 17, c: flat, ..Draw::default() }]);
        check_rect(&img, &flat, "Draw w=1", &mut bad);
        for (px, py, want) in [(16u32, 16u32, [0.0f32, 0.0]), (16, 47, [0.0, 1.0]), (47, 16, [1.0, 0.0]), (47, 47, [1.0, 1.0])] {
            let g = img[(py * W + px) as usize];
            if g[3] != 1.0 || (g[0] - want[0]).abs() > 0.02 || (g[1] - want[1]).abs() > 0.02 {
                bad.push(format!("corner ({px},{py}) = {g:?}, want uv near {want:?}"));
            }
        }
        let img = run(k, p, &[Draw { topology: 17, indexed: true, c: persp, ..Draw::default() }]);
        check_rect(&img, &persp, "DrawIndexed perspective w=(2,1.5,1.5)", &mut bad);
        let img = run(k, p, &[Draw { topology: 17, pso_after_topology: true, c: flat, ..Draw::default() }]);
        check_rect(&img, &flat, "PSO set after topology 17", &mut bad);
        let img = run(k, p, &[Draw { topology: 17, depth_bias: true, c: persp, ..Draw::default() }]);
        check_rect(&img, &persp, "RSSetDepthBiasX while topology 17", &mut bad);
        let img = run(k, p, &[Draw { topology: 17, c: [-1.0, 1.0, -0.5, 0.5, 1.0, 1.0, 1.0, 0.0], ..Draw::default() }, Draw { topology: 4, c: flat, ..Draw::default() }]);
        let at = |px: u32, py: u32| img[(py * W + px) as usize];
        if at(0, 0)[3] != 1.0 || at(15, 15)[3] != 1.0 {
            bad.push(format!("first rect lost corners: {:?} {:?}", at(0, 0), at(15, 15)));
        }
        if at(17, 17)[3] != 1.0 {
            bad.push(format!("topology-4 draw missing its first triangle: {:?}", at(17, 17)));
        }
        if at(46, 46) != [0.0; 4] {
            bad.push(format!("topology-4 draw after a rect still drew a fourth corner: {:?}", at(46, 46)));
        }
        bad
    }
}
