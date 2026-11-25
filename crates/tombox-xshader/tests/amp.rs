#![cfg(windows)]
#![allow(dead_code)]

#[path = "../../tools/xreplay/src/d3d.rs"]
mod d3d;

use core::ffi::c_void;
use d3d::{Gpu, Guid};
use tombox_xshader::hlsl::{translate, Translated};
use tombox_xshader::meta::{GsInfo, MeshInfo, RootParam, RootParamKind, RootSig, SigElement};
use tombox_xshader::{dxc, extract, rdna2, ShaderExtract, Stage};

extern "system" {
    fn LoadLibraryA(n: *const u8) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
}

const IID_DEVICE2: Guid = Guid(0x30baa41e, 0xb15b, 0x475c, [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28]);
const IID_LIST6: Guid = Guid(0xc3827890, 0xe548, 0x4cfa, [0x96, 0xcf, 0x56, 0x89, 0xa9, 0x37, 0x0f, 0x80]);
const PS: &str = "uint4 main(float4 p : SV_Position, nointerpolation uint4 t : TEXCOORD0) : SV_Target { return t; }";
const W: usize = 8;
const UAV_STATE: u32 = 8;

fn fixture(name: &str) -> Vec<u8> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../analysis/shaders/console_xsc");
    p.push(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("missing fixture {}: {e}", p.display()))
}

fn hlsl_of(name: &str) -> tombox_xshader::hlsl::Translated {
    let ex = extract(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
    translate(&ex).unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
#[cfg_attr(not(tombox_fixtures_console_xsc), ignore = "needs analysis/shaders/console_xsc fixtures extracted from local title dumps")]
fn console_as_fixture_uses_the_decoded_ring_abi() {
    let ex = extract(&fixture("as3.dxbc")).unwrap();
    assert_eq!(ex.stage, Stage::Amplification);
    let m = ex.gs_info.and_then(|g| g.mesh).expect("PSV0 AS info");
    assert_eq!((m.stage, m.payload_bytes, m.num_threads), (14, 16, [32, 1, 1]));
    let insns = rdna2::disassemble(&ex.isa);
    let stores: Vec<_> = insns.iter().filter(|i| i.enc == rdna2::Enc::Flat && i.op & 0x7f == 30).collect();
    assert_eq!(stores.len(), 2);
    assert!(insns.iter().any(|i| i.mnemonic == "s_lshl_b64" && i.src.first() == Some(&rdna2::Opnd::Sgpr(2))));
    assert!(insns.iter().any(|i| i.mnemonic == "s_lshl_b64" && i.src.first() == Some(&rdna2::Opnd::Sgpr(6))));
    let ex2 = extract(&fixture("as2.dxbc")).unwrap();
    let i2 = rdna2::disassemble(&ex2.isa);
    assert!(i2.iter().any(|i| i.mnemonic == "s_mul_i32" && i.src.get(1) == Some(&rdna2::Opnd::ConstInt(48))), "40-byte payload uses a 48-byte ring stride");
}

#[test]
#[cfg_attr(not(tombox_fixtures_console_xsc), ignore = "needs analysis/shaders/console_xsc fixtures extracted from local title dumps")]
fn amp_hlsl_dispatches_from_the_args_entry() {
    let t = hlsl_of("as3.dxbc");
    assert_eq!(t.profile, "as_6_5");
    assert!(t.hlsl.lines().any(|l| l == "#define TB_AMP_GMEM 1"), "{}", t.hlsl);
    assert!(t.hlsl.contains("struct TbPayload { uint w[4]; };"), "{}", t.hlsl);
    assert!(t.hlsl.contains("DispatchMesh(tb_args[0], max(tb_args[1], 1u), 1u, tb_pl);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("s2 = 0x10000000u; s3 = 0x20000000u; s4 = gid.x;"), "{}", t.hlsl);
    let t2 = hlsl_of("as2.dxbc");
    assert!(t2.hlsl.contains("struct TbPayload { uint w[12]; };"), "{}", t2.hlsl);
    for t in [&t, &t2] {
        dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
    }
}

#[test]
#[cfg_attr(not(tombox_fixtures_console_xsc), ignore = "needs analysis/shaders/console_xsc fixtures extracted from local title dumps")]
fn paired_mesh_shader_reads_the_payload_window() {
    let t = hlsl_of("ms3.dxbc");
    assert!(t.hlsl.contains("in payload TbPayload tb_pl"), "{}", t.hlsl);
    assert!(t.hlsl.contains("s10 = 0x3u; s11 = 0u;"), "{}", t.hlsl);
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
}

unsafe fn warp_device() -> *mut c_void {
    const IID_FACTORY4: Guid = Guid(0x1bc6ea02, 0xef36, 0x464f, [0xbf, 0x0c, 0x21, 0xca, 0x39, 0xe5, 0x16, 0x8a]);
    const IID_ADAPTER: Guid = Guid(0x2411e7e1, 0x12ac, 0x4ccf, [0xbd, 0x14, 0x97, 0x98, 0xe8, 0x53, 0x4d, 0xc0]);
    let dxgi = LoadLibraryA(b"dxgi.dll\0".as_ptr());
    let cf: unsafe extern "system" fn(*const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(GetProcAddress(dxgi, b"CreateDXGIFactory1\0".as_ptr()));
    let mut f: *mut c_void = core::ptr::null_mut();
    assert!(cf(&IID_FACTORY4, &mut f) >= 0, "CreateDXGIFactory1");
    let mut ad: *mut c_void = core::ptr::null_mut();
    assert!(vcall!(f, 27, fn(*const Guid, *mut *mut c_void) -> i32, &IID_ADAPTER, &mut ad) >= 0, "EnumWarpAdapter");
    let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let create: unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(GetProcAddress(d3d, b"D3D12CreateDevice\0".as_ptr()));
    let mut device: *mut c_void = core::ptr::null_mut();
    assert!(create(ad, 0xc100, &d3d::IID_DEVICE, &mut device) >= 0, "D3D12CreateDevice(WARP)");
    device
}

unsafe fn gpu_on(device: *mut c_void) -> Gpu {
    let qd: [u32; 4] = [0, 0, 0, 0];
    let mut queue: *mut c_void = core::ptr::null_mut();
    vcall!(device, 8, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &d3d::IID_QUEUE, &mut queue);
    let mut alloc: *mut c_void = core::ptr::null_mut();
    vcall!(device, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &d3d::IID_ALLOCATOR, &mut alloc);
    let mut list: *mut c_void = core::ptr::null_mut();
    vcall!(device, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, alloc, core::ptr::null_mut(), &d3d::IID_GFX_LIST, &mut list);
    vcall!(list, 9, fn() -> i32);
    let mut fence: *mut c_void = core::ptr::null_mut();
    vcall!(device, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &d3d::IID_FENCE, &mut fence);
    Gpu { device, queue, alloc, list, fence, value: 0, info: core::ptr::null_mut(), open: false }
}

unsafe fn mesh_tier(device: *mut c_void) -> u32 {
    let mut o7 = [0u32; 2];
    let hr = vcall!(device, 13, fn(u32, *mut c_void, u32) -> i32, 32, o7.as_mut_ptr() as *mut c_void, 8);
    if hr < 0 { 0 } else { o7[0] }
}

unsafe fn mesh_gpu() -> (Gpu, &'static str) {
    if std::env::var_os("TOMBOX_EXEC_HW").is_none() {
        let d = warp_device();
        assert!(mesh_tier(d) != 0, "WARP reports MeshShaderTier 0: rerun with TOMBOX_EXEC_HW=1 to use the hardware adapter");
        return (gpu_on(d), "WARP");
    }
    let d = hw_mesh_device().expect("no hardware adapter supports mesh shaders");
    (gpu_on(d), "hardware")
}

unsafe fn hw_mesh_device() -> Option<*mut c_void> {
    const IID_FACTORY6: Guid = Guid(0xc1b6694f, 0xff09, 0x44a9, [0xb0, 0x3c, 0x77, 0x90, 0x0a, 0x0a, 0x1d, 0x17]);
    const IID_ADAPTER1: Guid = Guid(0x29038f61, 0x3839, 0x4626, [0x91, 0xfd, 0x08, 0x68, 0x79, 0x01, 0x1a, 0x05]);
    const DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE: u32 = 2;
    const DXGI_ADAPTER_FLAG_SOFTWARE: u32 = 2;
    let dxgi = LoadLibraryA(b"dxgi.dll\0".as_ptr());
    let cf: unsafe extern "system" fn(*const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(GetProcAddress(dxgi, b"CreateDXGIFactory1\0".as_ptr()));
    let mut f: *mut c_void = core::ptr::null_mut();
    assert!(cf(&IID_FACTORY6, &mut f) >= 0, "CreateDXGIFactory1(IDXGIFactory6)");
    let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let create: unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(GetProcAddress(d3d, b"D3D12CreateDevice\0".as_ptr()));
    for i in 0.. {
        let mut ad: *mut c_void = core::ptr::null_mut();
        if vcall!(f, 29, fn(u32, u32, *const Guid, *mut *mut c_void) -> i32, i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, &IID_ADAPTER1, &mut ad) < 0 {
            return None;
        }
        let mut desc = [0u32; 78];
        vcall!(ad, 10, fn(*mut c_void) -> i32, desc.as_mut_ptr() as *mut c_void);
        if desc[76] & DXGI_ADAPTER_FLAG_SOFTWARE != 0 {
            continue;
        }
        let mut device: *mut c_void = core::ptr::null_mut();
        if create(ad, 0xc100, &d3d::IID_DEVICE, &mut device) >= 0 {
            if mesh_tier(device) != 0 {
                return Some(device);
            }
            vcall!(device, 2, fn() -> u32);
        }
    }
    None
}

fn sub(stream: &mut Vec<u8>, ty: u32, body: &[u8]) {
    stream.extend_from_slice(&ty.to_le_bytes());
    let align = if matches!(ty, 0..=6 | 19 | 24 | 25) { 8 } else { 4 };
    while stream.len() % align != 0 {
        stream.push(0);
    }
    stream.extend_from_slice(body);
    while stream.len() % 8 != 0 {
        stream.push(0);
    }
}

fn bytecode(b: &[u8]) -> Vec<u8> {
    let mut v = (b.as_ptr() as u64).to_le_bytes().to_vec();
    v.extend_from_slice(&(b.len() as u64).to_le_bytes());
    v
}

unsafe fn root_signature(gpu: &Gpu, consts: u32) -> *mut c_void {
    const SPACE: u32 = 4_000_003;
    let mut params = [0u8; 32 * 5];
    for (i, (ty, reg)) in [(2u32, 0u32), (4, 0), (4, 1), (4, 2)].iter().enumerate() {
        let p = &mut params[i * 32..i * 32 + 32];
        p[0..4].copy_from_slice(&ty.to_le_bytes());
        p[8..12].copy_from_slice(&reg.to_le_bytes());
        p[12..16].copy_from_slice(&SPACE.to_le_bytes());
    }
    params[128..132].copy_from_slice(&1u32.to_le_bytes());
    params[144..148].copy_from_slice(&consts.to_le_bytes());
    let mut desc = [0u8; 40];
    desc[0..4].copy_from_slice(&(4 + (consts != 0) as u32).to_le_bytes());
    desc[8..16].copy_from_slice(&(params.as_ptr() as u64).to_le_bytes());
    let d3d12 = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
        core::mem::transmute(GetProcAddress(d3d12, b"D3D12SerializeRootSignature\0".as_ptr()));
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    assert!(ser(desc.as_ptr(), 1, &mut blob, &mut err) >= 0, "serialize root signature");
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(gpu.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, vcall!(blob, 3, fn() -> *const u8), vcall!(blob, 4, fn() -> usize), &d3d::IID_ROOTSIG, &mut rs);
    assert!(hr >= 0, "CreateRootSignature {hr:#x}");
    rs
}

static WARP_RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

unsafe fn render(as_name: &str, ms_name: &str, groups: u32) -> Result<([[u32; 4]; W], [u32; 3], u64), String> {
    render_pair(&hlsl_of(as_name), &hlsl_of(ms_name), groups, &[])
}

unsafe fn render_pair(ta: &Translated, tm: &Translated, groups: u32, consts: &[u32]) -> Result<([[u32; 4]; W], [u32; 3], u64), String> {
    let asb = dxc::compile(&ta.hlsl, &dxc::profile_for(ta.profile)).map_err(|e| format!("{e}\n{}", ta.hlsl))?;
    let msb = dxc::compile(&tm.hlsl, &dxc::profile_for(tm.profile)).map_err(|e| format!("{e}\n{}", tm.hlsl))?;
    let psb = dxc::compile(PS, "ps_6_5")?;
    let _warp = std::env::var_os("TOMBOX_EXEC_HW").is_none().then(|| WARP_RENDER.lock().unwrap_or_else(|e| e.into_inner()));
    let (mut gpu, kind) = mesh_gpu();
    eprintln!("amplification render on {kind}");
    let rs = root_signature(&gpu, consts.len() as u32);
    let mut s = Vec::new();
    sub(&mut s, 0, &(rs as u64).to_le_bytes());
    sub(&mut s, 24, &bytecode(&asb));
    sub(&mut s, 25, &bytecode(&msb));
    sub(&mut s, 2, &bytecode(&psb));
    let mut rast = [0u32; 11];
    rast[0] = 3;
    rast[1] = 1;
    rast[6] = 1;
    sub(&mut s, 10, &rast.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>());
    let mut ds = [0u32; 13];
    ds[2] = 8;
    ds[4] = 0xffff;
    for k in [5usize, 9] {
        ds[k] = 1;
        ds[k + 1] = 1;
        ds[k + 2] = 1;
        ds[k + 3] = 8;
    }
    sub(&mut s, 11, &ds.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>());
    let mut rtf = [0u32; 9];
    rtf[0] = 3;
    rtf[8] = 1;
    sub(&mut s, 15, &rtf.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>());
    sub(&mut s, 17, &[1, 0, 0, 0, 0, 0, 0, 0]);
    sub(&mut s, 9, &0xffff_ffffu32.to_le_bytes());
    let sd: [u64; 2] = [s.len() as u64, s.as_ptr() as u64];
    let mut dev2: *mut c_void = core::ptr::null_mut();
    assert!(vcall!(gpu.device, 0, fn(*const Guid, *mut *mut c_void) -> i32, &IID_DEVICE2, &mut dev2) >= 0, "ID3D12Device2");
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(dev2, 47, fn(*const u64, *const Guid, *mut *mut c_void) -> i32, sd.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        return Err(format!("CreatePipelineState(AS+MS) hr={:#010x}{}\n{}\n{}", hr as u32, gpu.messages(), ta.hlsl, tm.hlsl));
    }
    let mut td = [0u8; 0x38];
    td[0..4].copy_from_slice(&3u32.to_le_bytes());
    td[0x10..0x18].copy_from_slice(&(W as u64).to_le_bytes());
    td[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    td[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    td[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    td[0x20..0x24].copy_from_slice(&3u32.to_le_bytes());
    td[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    td[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
    let rt = gpu.committed(&td, d3d::default_heap(), 4)?;
    let rb = gpu.buffer(512, 3, d3d::COPY_DEST, false)?;
    let cb = gpu.buffer(256, 2, d3d::GENERIC_READ, false)?;
    let win = gpu.buffer(256, 1, UAV_STATE, true)?;
    let fault = gpu.buffer(256, 1, UAV_STATE, true)?;
    let (_h, rtv, _, _) = gpu.heap(2, 1, false)?;
    vcall!(gpu.device, 20, fn(*mut c_void, *const c_void, usize) -> (), rt, core::ptr::null(), rtv);
    gpu.begin();
    let l = gpu.list;
    let clear = [5.0f32, 6.0, 7.0, 8.0];
    vcall!(l, 48, fn(usize, *const f32, u32, *const c_void) -> (), rtv, clear.as_ptr(), 0, core::ptr::null());
    vcall!(l, 46, fn(u32, *const usize, i32, *const usize) -> (), 1, &rtv, 0, core::ptr::null());
    vcall!(l, 30, fn(*mut c_void) -> (), rs);
    vcall!(l, 38, fn(u32, u64) -> (), 0, Gpu::gpu_va(cb));
    vcall!(l, 42, fn(u32, u64) -> (), 1, Gpu::gpu_va(win));
    vcall!(l, 42, fn(u32, u64) -> (), 2, Gpu::gpu_va(win));
    vcall!(l, 42, fn(u32, u64) -> (), 3, Gpu::gpu_va(fault));
    if !consts.is_empty() {
        vcall!(l, 36, fn(u32, u32, *const u32, u32) -> (), 4, consts.len() as u32, consts.as_ptr(), 0);
    }
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    let vp = [0f32, 0., W as f32, 1., 0., 1.];
    vcall!(l, 21, fn(u32, *const f32) -> (), 1, vp.as_ptr());
    let sc = [0i32, 0, W as i32, 1];
    vcall!(l, 22, fn(u32, *const i32) -> (), 1, sc.as_ptr());
    let qd = [0u32, 1, 0];
    let mut qh: *mut c_void = core::ptr::null_mut();
    assert!(vcall!(gpu.device, 39, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &d3d::IID_QUERYHEAP, &mut qh) >= 0, "CreateQueryHeap(occlusion)");
    let mut l6: *mut c_void = core::ptr::null_mut();
    assert!(vcall!(l, 0, fn(*const Guid, *mut *mut c_void) -> i32, &IID_LIST6, &mut l6) >= 0, "ID3D12GraphicsCommandList6");
    vcall!(l, 52, fn(*mut c_void, u32, u32) -> (), qh, 0, 0);
    vcall!(l6, 79, fn(u32, u32, u32) -> (), groups, 1, 1);
    vcall!(l, 53, fn(*mut c_void, u32, u32) -> (), qh, 0, 0);
    d3d::release(l6);
    gpu.barrier(rt, 4, d3d::COPY_SOURCE);
    gpu.barrier(fault, UAV_STATE, d3d::COPY_SOURCE);
    let mut dst2 = [0u8; 48];
    dst2[0..8].copy_from_slice(&(rb as u64).to_le_bytes());
    dst2[8..12].copy_from_slice(&1u32.to_le_bytes());
    dst2[24..28].copy_from_slice(&3u32.to_le_bytes());
    dst2[28..32].copy_from_slice(&(W as u32).to_le_bytes());
    dst2[32..36].copy_from_slice(&1u32.to_le_bytes());
    dst2[36..40].copy_from_slice(&1u32.to_le_bytes());
    dst2[40..44].copy_from_slice(&256u32.to_le_bytes());
    let mut src = [0u8; 48];
    src[0..8].copy_from_slice(&(rt as u64).to_le_bytes());
    vcall!(l, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), dst2.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 256, fault, 0, 16);
    vcall!(l, 54, fn(*mut c_void, u32, u32, u32, *mut c_void, u64) -> (), qh, 0, 0, 1, rb, 272);
    gpu.submit()?;
    let b = Gpu::map_read(rb, 512).ok_or("map readback")?;
    d3d::release(qh);
    d3d::release(pso);
    d3d::release(dev2);
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let mut px = [[0u32; 4]; W];
    for (i, p) in px.iter_mut().enumerate() {
        for c in 0..4 {
            p[c] = u(i * 16 + c * 4);
        }
    }
    Ok((px, [u(256), u(260), u(264)], u64::from(u(272)) | (u64::from(u(276)) << 32)))
}

#[test]
#[cfg_attr(not(tombox_fixtures_console_xsc), ignore = "needs analysis/shaders/console_xsc fixtures extracted from local title dumps")]
fn gpu_console_amplification_feeds_payload_and_group_counts_to_the_mesh_stage() {
    let (px, fault, _) = unsafe { render("as3.dxbc", "ms3.dxbc", 2) }.unwrap_or_else(|e| panic!("{e}"));
    let clear = [5, 6, 7, 8];
    let mut want = [clear; W];
    want[0] = [0x100, 0, 0, 0x5a5a];
    want[4] = [0x101, 0, 1, 0x5a5a];
    want[5] = [0x101, 1, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
}

const S_ENDPGM: u32 = 0xbf81_0000;
const LIT: u32 = 0xff;
const EXEC: u32 = 126;
const M0: u32 = 124;
const F_NEG1: u32 = 243;
const F_ONE: u32 = 242;
const SNULL: u32 = 0x7d;
const TRI: u32 = 64 | (65 << 10) | (66 << 20);

fn v(n: u32) -> u32 {
    256 + n
}

fn ic(n: i32) -> u32 {
    if n >= 0 { 128 + n as u32 } else { 192 + (-n) as u32 }
}

fn vop1(op: u32, vdst: u32, src0: u32) -> u32 {
    0x7e00_0000 | (vdst << 17) | (op << 9) | src0
}

fn vop2(op: u32, vdst: u32, src0: u32, vsrc1: u32) -> u32 {
    (op << 25) | (vdst << 17) | (vsrc1 << 9) | src0
}

fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0x7c00_0000 | (op << 17) | (vsrc1 << 9) | src0
}

fn vop3(op: u32, vdst: u32, srcs: [u32; 3]) -> [u32; 2] {
    [0xd400_0000 | (op << 16) | vdst, srcs[0] | (srcs[1] << 9) | (srcs[2] << 18)]
}

fn sop1(op: u32, sdst: u32, s0: u32) -> u32 {
    0xbe80_0000 | (sdst << 16) | (op << 8) | s0
}

fn sop2(op: u32, sdst: u32, s0: u32, s1: u32) -> u32 {
    0x8000_0000 | (op << 23) | (sdst << 16) | (s1 << 8) | s0
}

fn sopp(op: u32, imm: u16) -> u32 {
    0xbf80_0000 | (op << 16) | imm as u32
}

fn exp(target: u32, en: u32, done: bool, s: [u32; 4]) -> [u32; 2] {
    [0xf800_0000 | ((done as u32) << 11) | (target << 4) | en, s[0] | (s[1] << 8) | (s[2] << 16) | (s[3] << 24)]
}

fn smem(op: u32, sdata: u32, sbase: u32, imm: u32) -> [u32; 2] {
    [0xf400_0000 | (op << 18) | (sdata << 6) | (sbase >> 1), (SNULL << 25) | imm]
}

fn global(op: u32, vaddr: u32, vdata: u32, saddr: u32, vdst: u32) -> [u32; 2] {
    [0xdc00_0000 | (op << 18) | (2 << 14), vaddr | (vdata << 8) | (saddr << 16) | (vdst << 24)]
}

fn sig(name: &str, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index: 0, sysval, comp_type, register, mask, rw_mask: 0 }
}

fn synth(stage: Stage, words: &[u32], mesh: MeshInfo, outs: Vec<SigElement>, root: Option<RootSig>) -> ShaderExtract {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderExtract {
        stage,
        shader_model: (6, 5),
        bitcode_stripped: true,
        fssm: Vec::new(),
        entry: Some("main".into()),
        isa_data: isa.clone(),
        isa,
        isa_offset_in_fssm: 0,
        isa_preamble: [0; 4],
        psv_numthreads: None,
        has_input_sig: false,
        has_output_sig: !outs.is_empty(),
        has_psv0: true,
        has_root_sig: root.is_some(),
        input_sig: vec![],
        output_sig: outs,
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: Some(mesh), tess: None }),
        root_sig: root,
    }
}

fn dword_args() -> Vec<u32> {
    global(28, 10, 8, 2, 0).to_vec()
}

fn console_args() -> Vec<u32> {
    let mut w = vec![sop2(33, 14, 6, ic(4)), vop1(1, 12, 11), vop1(1, 13, ic(0)), vop1(1, 14, 15), vop1(1, 15, 14)];
    w.extend(global(30, 10, 12, 2, 0));
    w
}

fn synth_as(head: &[u32], tag_and_count: &[u32], args: &[u32]) -> Vec<u32> {
    synth_as_with(head, tag_and_count, args, &[], 16)
}

fn synth_as_with(head: &[u32], tag_and_count: &[u32], args: &[u32], tail: &[u32], stride: u32) -> Vec<u32> {
    let mut w = head.to_vec();
    w.extend([sop1(3, 6, 3), sop1(3, 7, ic(0)), sop1(3, 3, ic(0))]);
    w.extend([sop2(31, 2, 2, ic(8)), sop2(31, 6, 6, ic(8))]);
    let pay_step = if stride == 16 {
        8
    } else {
        w.extend([sop2(0x26, 14, 4, LIT), stride]);
        14
    };
    w.extend([sop2(30, 8, 4, ic(4)), sop2(0, 2, 2, 8), sop2(4, 3, 3, ic(0)), sop2(0, 6, 6, pay_step), sop2(4, 7, 7, ic(0))]);
    w.push(sop2(30, 9, 4, ic(2)));
    w.extend(tag_and_count);
    w.extend([vop1(1, 10, ic(0)), vop1(1, 4, 9), vop1(1, 5, 10), vop1(1, 6, 4), vop1(1, 7, LIT), 0x5a5a, vop1(1, 8, 11)]);
    w.extend(global(30, 10, 4, 6, 0));
    w.extend(args);
    w.extend(tail);
    w.push(S_ENDPGM);
    w
}

fn synth_ms() -> Vec<u32> {
    synth_ms_with(&[])
}

fn synth_ms_with(tail: &[u32]) -> Vec<u32> {
    let mut w = vec![sop1(4, EXEC, ic(-1))];
    w.extend(vop3(0x365, 0, [ic(-1), ic(0), 0]));
    w.extend(vop3(0x366, 0, [ic(-1), v(0), 0]));
    w.extend([sop2(0x27, 3, 3, LIT), 0x40018]);
    w.extend(vop3(0x346, 0, [3, ic(6), v(0)]));
    w.extend([vop1(2, 2, v(5)), sop2(32, 14, 2, ic(7))]);
    w.extend([sop2(31, 12, 10, ic(36)), vop1(1, 20, ic(0))]);
    w.extend(global(14, 20, 0, 12, 24));
    w.push(0xbf8c_3f70);
    w.extend([sop1(4, 20, EXEC), vopc(0xd2, ic(0), 0), sopp(8, 3), sop1(3, M0, LIT), (1 << 12) | 67, sopp(0x10, 9), sop1(4, EXEC, 20)]);
    w.extend([vop2(0x25, 28, 14, 24), vop1(6, 10, v(28))]);
    w.extend([vop2(8, 10, LIT, 10), 0.25f32.to_bits(), vop2(3, 10, LIT, 10), (-0.875f32).to_bits()]);
    w.extend([vop1(1, 11, ic(0)), vop1(1, 12, LIT), (-0.125f32).to_bits(), vop1(1, 13, LIT), 0.125f32.to_bits()]);
    w.extend([vopc(0xc2, LIT, 0), 64, vop2(1, 11, v(11), 12), vopc(0xc2, LIT, 0), 65, vop2(1, 11, v(11), 13)]);
    w.extend([vop2(3, 1, v(10), 11), vop1(1, 2, LIT), 4.0f32.to_bits(), vopc(0xc2, LIT, 0), 66, vop2(1, 3, F_NEG1, 2)]);
    w.extend([vop1(1, 4, ic(0)), vop1(1, 6, F_ONE), vop1(1, 7, v(25)), vop1(1, 8, 14), vop1(1, 9, LIT), TRI]);
    w.extend(tail);
    w.extend(exp(12, 0xf, false, [1, 3, 4, 6]));
    w.extend(exp(20, 0x1, false, [9, 0, 0, 0]));
    w.extend(exp(32, 0xf, true, [7, 8, 26, 27]));
    w.push(S_ENDPGM);
    w
}

fn synth_pair(head: &[u32], tag_and_count: &[u32], args: &[u32], root: Option<RootSig>) -> (Translated, Translated) {
    synth_pair_with(&synth_as(head, tag_and_count, args), 16, root)
}

fn synth_pair_with(as_words: &[u32], payload_bytes: u32, root: Option<RootSig>) -> (Translated, Translated) {
    synth_pair_ms(as_words, &synth_ms(), payload_bytes, root)
}

fn synth_pair_ms(as_words: &[u32], ms_words: &[u32], payload_bytes: u32, root: Option<RootSig>) -> (Translated, Translated) {
    let a = MeshInfo { stage: 14, groupshared_bytes: 0, payload_bytes, max_vertices: 0, max_primitives: 0, prim_vectors: 0, topology: 0, num_threads: [32, 1, 1] };
    let m = MeshInfo { stage: 13, groupshared_bytes: 0, payload_bytes, max_vertices: 128, max_primitives: 128, prim_vectors: 0, topology: 2, num_threads: [128, 1, 1] };
    let outs = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)];
    let ta = translate(&synth(Stage::Amplification, as_words, a, vec![], root)).unwrap_or_else(|e| panic!("synthetic AS: {e}"));
    let tm = translate(&synth(Stage::Mesh, ms_words, m, outs, None)).unwrap_or_else(|e| panic!("synthetic MS: {e}"));
    (ta, tm)
}

#[test]
fn synthetic_pair_disassembles_to_the_ring_abi_shape() {
    let dis = |w: &[u32]| rdna2::disassemble(&w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>());
    let a = dis(&synth_as(&smem(1, 12, 0, 0), &[], &dword_args()));
    let m = dis(&synth_ms());
    for (insns, want) in [
        (&a, &["s_load_dwordx2", "s_lshl_b64", "s_addc_u32", "flat_store_dwordx4", "flat_store_dword", "s_endpgm"][..]),
        (&m, &["v_readfirstlane_b32", "s_lshl_b64", "flat_load_dwordx4", "s_sendmsg", "v_cvt_f32_u32", "v_cndmask_b32", "exp"][..]),
    ] {
        let names: Vec<&str> = insns.iter().map(|i| i.mnemonic.as_str()).collect();
        for x in want {
            assert!(names.iter().any(|y| y.starts_with(x)), "{x} missing from {names:?}");
        }
    }
    let flat: Vec<_> = a.iter().chain(&m).filter(|i| i.enc == rdna2::Enc::Flat).collect();
    assert_eq!(flat.len(), 3);
    assert!(flat.iter().all(|i| i.aux.mask == 2 && matches!(i.src.get(2), Some(rdna2::Opnd::Sgpr(_)))), "{flat:?}");
}

#[test]
fn gpu_synthetic_amplification_feeds_payload_and_group_counts_to_the_mesh_stage() {
    let (ta, tm) = synth_pair(&[], &[sop2(0, 10, 4, LIT), 0x100, sop2(0, 11, 4, ic(1))], &dword_args(), None);
    assert!(ta.hlsl.contains("struct TbPayload { uint w[4]; };"), "{}", ta.hlsl);
    assert!(tm.hlsl.contains("in payload TbPayload tb_pl"), "{}", tm.hlsl);
    let (px, fault, samples) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [[5, 6, 7, 8]; W];
    want[0] = [0x100, 0, 0, 0x5a5a];
    want[4] = [0x101, 0, 1, 0x5a5a];
    want[5] = [0x101, 1, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
    assert_eq!(samples, 3, "mesh groups launched");
}

#[test]
fn gpu_synthetic_amplification_reads_root_constants_through_s0_s1() {
    let root = RootSig { version: 2, flags: 0, params: vec![RootParam { visibility: 0, kind: RootParamKind::Constants { register: 0, space: 0, num32: 2 } }], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut head = smem(1, 12, 0, 0).to_vec();
    head.push(0xbf8c_c07f);
    let (ta, tm) = synth_pair(&head, &[sop2(0, 10, 4, 13), sop1(3, 11, 12)], &dword_args(), Some(root));
    assert!(ta.hlsl.contains("cbuffer TB_RC0 : register(b0, space0)"), "{}", ta.hlsl);
    let (px, fault, samples) = unsafe { render_pair(&ta, &tm, 2, &[2, 0x200]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [[5, 6, 7, 8]; W];
    want[0] = [0x200, 0, 0, 0x5a5a];
    want[1] = [0x200, 1, 0, 0x5a5a];
    want[4] = [0x201, 0, 1, 0x5a5a];
    want[5] = [0x201, 1, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
    assert_eq!(samples, 4, "mesh groups launched");
}

#[test]
fn gpu_synthetic_amplification_args_entry_with_the_payload_pointer_launches_each_mesh_group_once() {
    let (ta, tm) = synth_pair(&[], &[sop2(0, 10, 4, LIT), 0x100, sop2(0, 11, 4, ic(1))], &console_args(), None);
    let (px, fault, samples) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [[5, 6, 7, 8]; W];
    want[0] = [0x100, 0, 0, 0x5a5a];
    want[4] = [0x101, 0, 1, 0x5a5a];
    want[5] = [0x101, 1, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
    assert_eq!(samples, 3, "mesh groups launched");
}

#[test]
fn gpu_synthetic_amplification_root_load_after_a_branch_still_uses_s0_s1() {
    let root = RootSig { version: 2, flags: 0, params: vec![RootParam { visibility: 0, kind: RootParamKind::Constants { register: 0, space: 0, num32: 2 } }], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut head = vec![0xbf00_0000 | (6 << 16) | (4 << 8) | 4, sopp(5, 0)];
    head.extend(smem(1, 12, 0, 0));
    head.push(0xbf8c_c07f);
    let (ta, tm) = synth_pair(&head, &[sop2(0, 10, 4, 13), sop1(3, 11, 12)], &dword_args(), Some(root));
    assert!(ta.hlsl.contains("s12 = tb_rc0_0;") && !ta.hlsl.contains("s4 = tb_rc0_0;"), "{}", ta.hlsl);
    let (px, fault, samples) = unsafe { render_pair(&ta, &tm, 2, &[2, 0x200]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [[5, 6, 7, 8]; W];
    want[0] = [0x200, 0, 0, 0x5a5a];
    want[1] = [0x200, 1, 0, 0x5a5a];
    want[4] = [0x201, 0, 1, 0x5a5a];
    want[5] = [0x201, 1, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
    assert_eq!(samples, 4, "mesh groups launched");
}

const CLEAR: [u32; 4] = [5, 6, 7, 8];

#[test]
fn gpu_synthetic_amplification_group_count_zero_launches_nothing() {
    let (ta, tm) = synth_pair(&[], &[sop2(0, 10, 4, LIT), 0x100, sop1(3, 11, 4)], &dword_args(), None);
    let (px, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [CLEAR; W];
    want[4] = [0x101, 0, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
    let (ta, tm) = synth_pair(&[], &[sop2(0, 10, 4, LIT), 0x100, sop1(3, 11, ic(0))], &dword_args(), None);
    let (px, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, [CLEAR; W]);
}

#[test]
fn gpu_synthetic_amplification_four_mesh_groups_each_see_their_own_payload() {
    let (ta, tm) = synth_pair(&[], &[sop2(0, 10, 4, LIT), 0x100, sop1(3, 11, ic(4))], &dword_args(), None);
    let (px, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [CLEAR; W];
    for (i, p) in want.iter_mut().enumerate() {
        let (g, m) = (i as u32 / 4, i as u32 % 4);
        *p = [0x100 + g, m, g, 0x5a5a];
    }
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
}

#[test]
fn gpu_synthetic_amplification_payload_stride_rounds_up_to_sixteen() {
    let words = synth_as_with(&[], &[sop2(0, 10, 4, LIT), 0x100, sop2(0, 11, 4, ic(1))], &dword_args(), &[], 48);
    let (ta, tm) = synth_pair_with(&words, 40, None);
    let (px, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [CLEAR; W];
    want[0] = [0x100, 0, 0, 0x5a5a];
    want[4] = [0x101, 0, 1, 0x5a5a];
    want[5] = [0x101, 1, 1, 0x5a5a];
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
}

#[test]
fn gpu_synthetic_amplification_group_count_from_a_byte_store() {
    let st = global(24, 10, 12, 2, 0);
    let tail = [vop1(1, 12, LIT), 2, st[0], st[1]];
    let words = synth_as_with(&[], &[sop2(0, 10, 4, LIT), 0x100, sop1(3, 11, ic(0))], &dword_args(), &tail, 16);
    let (ta, tm) = synth_pair_with(&words, 16, None);
    let (px, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let mut want = [CLEAR; W];
    for (i, g, m) in [(0, 0, 0), (1, 0, 1), (4, 1, 0), (5, 1, 1)] {
        want[i] = [0x100 + g, m, g, 0x5a5a];
    }
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
}

#[test]
fn gpu_synthetic_amplification_sub_dword_payload_stores_merge() {
    let mut tail = vec![];
    for (op, off, data) in [(24, 5, 0xab), (26, 10, 0xbeef), (25, 12, 0x00cd_0000), (27, 14, 0x7e57_0000)] {
        let st = global(op, 10, 12, 6, 0);
        tail.extend([vop1(1, 12, LIT), data, st[0] | off, st[1]]);
    }
    let words = synth_as_with(&[], &[sop2(0, 10, 4, LIT), 0x100, sop2(0, 11, 4, ic(1))], &dword_args(), &tail, 16);
    let (ta, tm) = synth_pair_with(&words, 16, None);
    let (px, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));
    let px_for = |g: u32, m: u32| [0xab00 | g, m, 0xbeef_0000 | g, 0x7e57_5acd];
    let mut want = [CLEAR; W];
    want[0] = px_for(0, 0);
    want[4] = px_for(1, 0);
    want[5] = px_for(1, 1);
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(px, want);
}

#[test]
fn gpu_synthetic_amplification_merges_bytes_written_by_different_lanes() {
    let store = global(24, 10, 12, 6, 0);
    let tail = [vop2(0x1b, 10, ic(3), 0), vop2(0x25, 12, ic(0x11), 10), store[0] | 12, store[1]];
    let words = synth_as_with(&[], &[sop2(0, 10, 4, LIT), 0x100, sop2(0, 11, 4, ic(1))], &dword_args(), &tail, 16);
    let (ta, tm) = synth_pair_with(&words, 16, None);
    let (pixels, fault, _) = unsafe { render_pair(&ta, &tm, 2, &[]) }.unwrap_or_else(|e| panic!("{e}"));

    let mut expected = [CLEAR; W];
    for (pixel, group, mesh) in [(0, 0, 0), (4, 1, 0), (5, 1, 1)] {
        expected[pixel] = [0x100 + group, mesh, group, 0x1413_1211];
    }
    assert_eq!(fault, [0, 0, 0], "global-memory misses");
    assert_eq!(pixels, expected);
}

#[test]
fn gpu_synthetic_mesh_sub_dword_payload_loads_extend_correctly() {
    let st = global(28, 10, 12, 6, 0);
    let as_tail = [vop1(1, 12, LIT), 0x8081_82f3, st[0] | 12, st[1]];
    let words = synth_as_with(&[], &[sop2(0, 10, 4, LIT), 0x100, sop2(0, 11, 4, ic(1))], &dword_args(), &as_tail, 16);
    for (ops, want) in [
        ([(8, 13), (9, 13)], [0x82, 0xffff_ff82]),
        ([(10, 14), (11, 14)], [0x8081, 0xffff_8081]),
        ([(10, 13), (11, 13)], [0x8182, 0xffff_8182]),
        ([(8, 15), (9, 12)], [0x80, 0xffff_fff3]),
    ] {
        let mut tail = vec![];
        for (k, (op, off)) in ops.into_iter().enumerate() {
            let ld = global(op, 20, 0, 12, 26 + k as u32);
            tail.extend([ld[0] | off, ld[1]]);
        }
        let (ta, tm) = synth_pair_ms(&words, &synth_ms_with(&tail), 16, None);
        let (px, fault, _) = unsafe { render_pair(&ta, &tm, 1, &[]) }.unwrap_or_else(|e| panic!("{e}"));
        let mut expect = [CLEAR; W];
        expect[0] = [0x100, 0, want[0], want[1]];
        assert_eq!(fault, [0, 0, 0], "global-memory misses for {ops:?}");
        assert_eq!(px, expect, "loads {ops:?}");
    }
}
