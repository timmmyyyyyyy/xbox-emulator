#![cfg(windows)]
#![allow(dead_code)]

#[path = "../../tools/xreplay/src/d3d.rs"]
mod d3d;

use core::ffi::c_void;
use d3d::{Gpu, Guid};
use tombox_xshader::hlsl::{passthrough_hull, translate, Partitioning};
use tombox_xshader::tess_abi::{TfLayout, TB_TESS_CB_DWORDS, TB_TESS_CB_REGISTER, TB_TESS_GFX6_CONTROL_WORD, TB_TESS_SPACE, TB_TESS_TF_REGISTER};
use tombox_xshader::meta::{GsInfo, SigElement, TessInfo};
use tombox_xshader::{dxc, rdna2, ShaderExtract, Stage};

const S_ENDPGM: u32 = 0xbf81_0000;
const F_ONE: u32 = 242;
const F_NEG1: u32 = 243;
const W: usize = 16;
const H: usize = 16;
const CLEAR: u32 = 7;

extern "system" {
    fn LoadLibraryA(n: *const u8) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
}

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

fn exp(target: u32, en: u32, done: bool, s: [u32; 4]) -> [u32; 2] {
    [0xf800_0000 | ((done as u32) << 11) | (target << 4) | en, s[0] | (s[1] << 8) | (s[2] << 16) | (s[3] << 24)]
}

fn sig(name: &str, index: u32, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index, sysval, comp_type, register, mask, rw_mask: 0 }
}

fn quad_patch_sig() -> Vec<SigElement> {
    let mut s: Vec<SigElement> = (0..4).map(|k| sig("SV_TessFactor", k, 11, 3, k, 1)).collect();
    s.extend((0..2).map(|k| sig("SV_InsideTessFactor", k, 12, 3, 4 + k, 1)));
    s
}

fn tri_patch_sig() -> Vec<SigElement> {
    let mut s: Vec<SigElement> = (0..3).map(|k| sig("SV_TessFactor", k, 11, 3, k, 1)).collect();
    s.push(sig("SV_InsideTessFactor", 0, 12, 3, 3, 1));
    s.push(sig("PATCHDATA", 0, 0, 3, 4, 0xf));
    s
}

fn tess_info(stage: u8, input_control_points: u32, domain: u32) -> TessInfo {
    TessInfo { stage, input_control_points, output_control_points: 0, domain, output_primitive: 0, output_position: true, patch_const_vectors: 6 }
}

fn ds(words: &[u32], tess: Option<TessInfo>, inputs: Vec<SigElement>, patch: Vec<SigElement>, outs: Vec<SigElement>) -> ShaderExtract {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderExtract {
        stage: Stage::Domain,
        shader_model: (6, 0),
        bitcode_stripped: true,
        fssm: Vec::new(),
        entry: Some("main".into()),
        isa_data: isa.clone(),
        isa,
        isa_offset_in_fssm: 0,
        isa_preamble: [0; 4],
        psv_numthreads: None,
        has_input_sig: !inputs.is_empty(),
        has_output_sig: !outs.is_empty(),
        has_psv0: true,
        has_root_sig: false,
        input_sig: inputs,
        output_sig: outs,
        prim_sig: patch,
        resources: Vec::new(),
        gs_info: Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: None, tess }),
        root_sig: None,
    }
}

fn console_ds() -> Vec<u32> {
    let mut w = vec![
        vop1(6, 9, v(3)),
        vop2(3, 4, v(0), 9),
        vop2(3, 4, F_NEG1, 4),
        vop2(3, 5, v(1), 1),
        vop2(3, 5, F_NEG1, 5),
        vop1(1, 10, ic(0)),
        vop1(1, 11, F_ONE),
    ];
    w.extend(exp(12, 0xf, true, [4, 5, 10, 11]));
    w.extend(exp(32, 0x7, false, [0, 1, 3, 0]));
    w.push(S_ENDPGM);
    w
}

fn console_quad_ds() -> ShaderExtract {
    ds(
        &console_ds(),
        Some(tess_info(4, 1, 3)),
        vec![sig("CPOS", 0, 0, 3, 0, 0xf)],
        quad_patch_sig(),
        vec![sig("SV_Position", 0, 1, 3, 0, 0xf), sig("TEXCOORD", 0, 0, 1, 1, 0x7)],
    )
}

const VS: &str = "struct VO { float4 p : CPOS; };
VO main(uint id : SV_VertexID) { VO o; o.p = float4((float)id, 0.0, 0.0, 0.0); return o; }";

const HS: &str = "struct VO { float4 p : CPOS; };
struct PC { float e[4] : SV_TessFactor; float i[2] : SV_InsideTessFactor; };
PC pcf(InputPatch<VO, 1> ip, uint pid : SV_PrimitiveID) {
    PC o;
    [unroll] for (int k = 0; k < 4; k++) o.e[k] = 4.0;
    o.i[0] = 4.0;
    o.i[1] = 4.0;
    return o;
}
[domain(\"quad\")]
[partitioning(\"integer\")]
[outputtopology(\"triangle_cw\")]
[outputcontrolpoints(1)]
[patchconstantfunc(\"pcf\")]
VO main(InputPatch<VO, 1> ip, uint i : SV_OutputControlPointID) { return ip[i]; }";

const DS_REFERENCE: &str = "struct CP { float4 p : CPOS; };
struct PC { float e[4] : SV_TessFactor; float i[2] : SV_InsideTessFactor; };
struct O { precise float4 p : SV_Position; uint3 t : TEXCOORD0; };
[domain(\"quad\")]
O main(float2 loc : SV_DomainLocation, uint pid : SV_PrimitiveID, const OutputPatch<CP, 1> cp, PC pc) {
    O o;
    o.p = float4((loc.x + (float)pid) - 1.0, (loc.y + loc.y) - 1.0, 0.0, 1.0);
    o.t = uint3(asuint(loc.x), asuint(loc.y), pid);
    return o;
}";

const PS: &str = "uint4 main(float4 p : SV_Position, nointerpolation uint3 t : TEXCOORD0, uint prim : SV_PrimitiveID) : SV_Target { return uint4(t, prim + 1u); }";

static WARP_RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    assert!(create(ad, 0xc000, &d3d::IID_DEVICE, &mut device) >= 0, "D3D12CreateDevice(WARP)");
    d3d::release(ad);
    d3d::release(f);
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

unsafe fn tess_root_signature(gpu: &Gpu) -> *mut c_void {
    let d3d12 = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
        core::mem::transmute(GetProcAddress(d3d12, b"D3D12SerializeRootSignature\0".as_ptr()));
    let mut params = [0u8; 96];
    let put = |p: &mut [u8; 96], o: usize, v: u32| p[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut params, 0, 3);
    put(&mut params, 8, TB_TESS_TF_REGISTER);
    put(&mut params, 12, TB_TESS_SPACE);
    put(&mut params, 32, 1);
    put(&mut params, 40, TB_TESS_CB_REGISTER);
    put(&mut params, 44, TB_TESS_SPACE);
    put(&mut params, 48, TB_TESS_CB_DWORDS);
    put(&mut params, 64, 4);
    let mut desc = [0u8; 40];
    desc[0..4].copy_from_slice(&3u32.to_le_bytes());
    desc[8..16].copy_from_slice(&(params.as_ptr() as u64).to_le_bytes());
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    assert!(ser(desc.as_ptr(), 1, &mut blob, &mut err) >= 0, "D3D12SerializeRootSignature");
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(gpu.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, vcall!(blob, 3, fn() -> *const u8), vcall!(blob, 4, fn() -> usize), &d3d::IID_ROOTSIG, &mut rs);
    assert!(hr >= 0, "CreateRootSignature");
    d3d::release(blob);
    rs
}

struct Pipe<'a> {
    vs: &'a [u8],
    hs: &'a [u8],
    ds: &'a [u8],
    patches: u32,
    control_points: u32,
    ring: Vec<u32>,
    consts: [u32; 2],
}

struct Rendered {
    pixels: Vec<[u32; 4]>,
    log: Vec<u32>,
}

const LOG_DWORDS: usize = 256;
const UAV_STATE: u32 = 8;

unsafe fn render_patches(ds_code: &[u8], patches: u32) -> Result<Vec<[u32; 4]>, String> {
    let vs = dxc::compile(VS, "vs_6_2")?;
    let hs = dxc::compile(HS, "hs_6_2")?;
    render(&Pipe { vs: &vs, hs: &hs, ds: ds_code, patches, control_points: 1, ring: vec![0], consts: [0, 0] }).map(|r| r.pixels)
}

unsafe fn render(p: &Pipe) -> Result<Rendered, String> {
    let ps = dxc::compile(PS, "ps_6_2")?;
    let _warp = WARP_RENDER.lock().unwrap_or_else(|e| e.into_inner());
    let mut gpu = gpu_on(warp_device());
    let rs = tess_root_signature(&gpu);
    let mut d = vec![0u8; 656];
    let put32 = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    put64(&mut d, 0, rs as u64);
    for (o, code) in [(8usize, p.vs), (24, &ps[..]), (40, p.ds), (56, p.hs)] {
        put64(&mut d, o, code.as_ptr() as u64);
        put64(&mut d, o + 8, code.len() as u64);
    }
    for i in 0..8 {
        let o = 128 + i * 40;
        put32(&mut d, o + 8, 2);
        put32(&mut d, o + 12, 1);
        put32(&mut d, o + 16, 1);
        put32(&mut d, o + 20, 2);
        put32(&mut d, o + 24, 1);
        put32(&mut d, o + 28, 1);
        put32(&mut d, o + 32, 4);
        d[o + 36] = 0xf;
    }
    put32(&mut d, 448, 0xffff_ffff);
    put32(&mut d, 452, 3);
    put32(&mut d, 456, 1);
    put32(&mut d, 476, 1);
    put32(&mut d, 504, 8);
    for f in [516usize, 532] {
        put32(&mut d, f, 1);
        put32(&mut d, f + 4, 1);
        put32(&mut d, f + 8, 1);
        put32(&mut d, f + 12, 8);
    }
    put32(&mut d, 572, 4);
    put32(&mut d, 576, 1);
    put32(&mut d, 580, 3);
    put32(&mut d, 616, 1);
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(gpu.device, 10, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        return Err(format!("CreateGraphicsPipelineState(tessellation) hr={:#010x}{}", hr as u32, gpu.messages()));
    }
    let mut td = [0u8; 0x38];
    td[0..4].copy_from_slice(&3u32.to_le_bytes());
    td[0x10..0x18].copy_from_slice(&(W as u64).to_le_bytes());
    td[0x18..0x1c].copy_from_slice(&(H as u32).to_le_bytes());
    td[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    td[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    td[0x20..0x24].copy_from_slice(&3u32.to_le_bytes());
    td[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    td[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
    let rt = gpu.committed(&td, d3d::default_heap(), 4)?;
    let pitch = W * 16;
    let rb = gpu.buffer((pitch * H) as u64, 3, d3d::COPY_DEST, false)?;
    let ring_bytes: Vec<u8> = p.ring.iter().flat_map(|w| w.to_le_bytes()).collect();
    let ring = gpu.buffer(ring_bytes.len() as u64, 2, d3d::GENERIC_READ, false)?;
    assert!(Gpu::map_write(ring, &ring_bytes), "map ring");
    let log_bytes = (LOG_DWORDS * 4) as u64;
    let zeros = gpu.buffer(log_bytes, 2, d3d::GENERIC_READ, false)?;
    assert!(Gpu::map_write(zeros, &vec![0u8; LOG_DWORDS * 4]), "map zeros");
    let log = gpu.buffer(log_bytes, 1, d3d::COPY_DEST, true)?;
    let log_rb = gpu.buffer(log_bytes, 3, d3d::COPY_DEST, false)?;
    let (rtv_heap, rtv, _, _) = gpu.heap(2, 1, false)?;
    vcall!(gpu.device, 20, fn(*mut c_void, *const c_void, usize) -> (), rt, core::ptr::null(), rtv);
    gpu.begin();
    let l = gpu.list;
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), log, 0, zeros, 0, log_bytes);
    gpu.barrier(log, d3d::COPY_DEST, UAV_STATE);
    let clear = [CLEAR as f32; 4];
    vcall!(l, 48, fn(usize, *const f32, u32, *const c_void) -> (), rtv, clear.as_ptr(), 0, core::ptr::null());
    vcall!(l, 46, fn(u32, *const usize, i32, *const usize) -> (), 1, &rtv, 0, core::ptr::null());
    vcall!(l, 30, fn(*mut c_void) -> (), rs);
    vcall!(l, 40, fn(u32, u64) -> (), 0, Gpu::gpu_va(ring));
    vcall!(l, 36, fn(u32, u32, *const u32, u32) -> (), 1, 2, p.consts.as_ptr(), 0);
    vcall!(l, 42, fn(u32, u64) -> (), 2, Gpu::gpu_va(log));
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    let vp = [0f32, 0., W as f32, H as f32, 0., 1.];
    vcall!(l, 21, fn(u32, *const f32) -> (), 1, vp.as_ptr());
    let sc = [0i32, 0, W as i32, H as i32];
    vcall!(l, 22, fn(u32, *const i32) -> (), 1, sc.as_ptr());
    vcall!(l, 20, fn(u32) -> (), 32 + p.control_points);
    vcall!(l, 12, fn(u32, u32, u32, u32) -> (), p.patches * p.control_points, 1, 0, 0);
    gpu.barrier(rt, 4, d3d::COPY_SOURCE);
    gpu.barrier(log, UAV_STATE, d3d::COPY_SOURCE);
    let mut dst = [0u8; 48];
    dst[0..8].copy_from_slice(&(rb as u64).to_le_bytes());
    dst[8..12].copy_from_slice(&1u32.to_le_bytes());
    dst[24..28].copy_from_slice(&3u32.to_le_bytes());
    dst[28..32].copy_from_slice(&(W as u32).to_le_bytes());
    dst[32..36].copy_from_slice(&(H as u32).to_le_bytes());
    dst[36..40].copy_from_slice(&1u32.to_le_bytes());
    dst[40..44].copy_from_slice(&(pitch as u32).to_le_bytes());
    let mut src = [0u8; 48];
    src[0..8].copy_from_slice(&(rt as u64).to_le_bytes());
    vcall!(l, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), dst.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), log_rb, 0, log, 0, log_bytes);
    let submitted = gpu.submit();
    let bytes = Gpu::map_read(rb, pitch * H);
    let log_read = Gpu::map_read(log_rb, LOG_DWORDS * 4);
    for obj in [pso, rt, rb, ring, zeros, log, log_rb, rtv_heap, rs, gpu.list, gpu.alloc, gpu.fence, gpu.queue, gpu.device] {
        d3d::release(obj);
    }
    submitted?;
    let b = bytes.ok_or("map readback")?;
    let pixels = b.chunks_exact(16).map(|px| core::array::from_fn(|k| u32::from_le_bytes(px[k * 4..k * 4 + 4].try_into().unwrap()))).collect();
    let log = log_read.ok_or("map log readback")?.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect();
    Ok(Rendered { pixels, log })
}

#[test]
fn console_domain_shader_disassembles_as_written() {
    let isa: Vec<u8> = console_ds().iter().flat_map(|w| w.to_le_bytes()).collect();
    let names: Vec<String> = rdna2::disassemble(&isa).into_iter().map(|i| i.mnemonic).collect();
    assert_eq!(
        names,
        ["v_cvt_f32_u32", "v_add_f32", "v_add_f32", "v_add_f32", "v_add_f32", "v_mov_b32", "v_mov_b32", "exp", "exp", "s_endpgm"]
    );
}

#[test]
fn domain_shader_entry_follows_the_psv0_domain_and_signatures() {
    let t = translate(&console_quad_ds()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(t.profile, "ds_5_0");
    for needle in [
        "struct SPatch {",
        "float tb_tf0[4] : SV_TessFactor;",
        "float tb_tf1[2] : SV_InsideTessFactor;",
        "[domain(\"quad\")]",
        "float2 tb_loc : SV_DomainLocation",
        "uint tb_pid : SV_PrimitiveID",
        "const OutputPatch<SIn, 1> i",
        "SPatch tb_pc",
        "v0 = asuint(tb_loc.x);",
        "v1 = asuint(tb_loc.y);",
        "v3 = tb_pid;",
    ] {
        assert!(t.hlsl.contains(needle), "missing {needle:?}\n{}", t.hlsl);
    }
    assert!(!t.hlsl.contains("v2 = tb_pid;"), "v2 is not read, so it is not seeded\n{}", t.hlsl);
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
}

#[test]
fn triangle_and_isoline_domains_get_their_location_type_and_patch_data() {
    let outs = || vec![sig("SV_Position", 0, 1, 3, 0, 0xf), sig("TEXCOORD", 0, 0, 1, 1, 0x7)];
    let tri = ds(&console_ds(), Some(tess_info(4, 3, 2)), vec![sig("CPOS", 0, 0, 3, 0, 0xf)], tri_patch_sig(), outs());
    let t = translate(&tri).unwrap_or_else(|e| panic!("{e}"));
    for needle in ["[domain(\"tri\")]", "float3 tb_loc : SV_DomainLocation", "const OutputPatch<SIn, 3> i", "float tb_tf0[3] : SV_TessFactor;", "float tb_tf1 : SV_InsideTessFactor;", "float4 p4 : PATCHDATA0;"] {
        assert!(t.hlsl.contains(needle), "missing {needle:?}\n{}", t.hlsl);
    }
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
    let lines = ds(&console_ds(), Some(tess_info(4, 2, 1)), vec![], vec![sig("SV_TessFactor", 0, 16, 3, 0, 1), sig("SV_TessFactor", 1, 15, 3, 1, 1)], outs());
    let t = translate(&lines).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("[domain(\"isoline\")]") && t.hlsl.contains("float2 tb_loc") && t.hlsl.contains("float tb_tf0[2] : SV_TessFactor;"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("OutputPatch"), "no control-point inputs, no OutputPatch\n{}", t.hlsl);
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
}

#[test]
fn domain_shader_refusals() {
    let mut no_tess = console_quad_ds();
    no_tess.gs_info = None;
    assert_eq!(translate(&no_tess).err().as_deref(), Some("Domain shader without PSV0 tessellator info"));
    let mut hull_info = console_quad_ds();
    hull_info.gs_info = Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: None, tess: Some(tess_info(3, 1, 3)) });
    assert_eq!(translate(&hull_info).err().as_deref(), Some("Domain shader without PSV0 tessellator info"));
    let mut no_outputs = console_quad_ds();
    no_outputs.output_sig.clear();
    assert_eq!(translate(&no_outputs).err().as_deref(), Some("domain shader without outputs unsupported"));
    let mut hull = console_quad_ds();
    hull.stage = Stage::Hull;
    assert_eq!(translate(&hull).err().as_deref(), Some("stage Hull not supported yet"));
}

#[test]
fn gpu_translated_domain_shader_matches_native_hlsl_on_a_tessellated_quad() {
    let t = translate(&console_quad_ds()).unwrap_or_else(|e| panic!("{e}"));
    let translated = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
    let reference = dxc::compile(DS_REFERENCE, "ds_6_2").unwrap_or_else(|e| panic!("{e}"));
    let got = unsafe { render_patches(&translated, 2) }.unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
    let want = unsafe { render_patches(&reference, 2) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, want, "translated DS differs from the native HLSL DS\n{}", t.hlsl);
    let grid: Vec<u32> = [0.0f32, 0.25, 0.5, 0.75, 1.0].iter().map(|f| f.to_bits()).collect();
    for (k, px) in got.iter().enumerate() {
        let (x, y) = (k % W, k / W);
        assert_ne!(px[3], CLEAR, "pixel ({x}, {y}) not covered: {px:?}");
        assert_eq!(px[2] + 1, px[3], "pixel ({x}, {y}): DS patch id differs from the tessellator's SV_PrimitiveID: {px:?}");
        assert_eq!(px[2], (x >= W / 2) as u32, "pixel ({x}, {y}) has the wrong patch id: {px:?}");
        assert!(grid.contains(&px[0]) && grid.contains(&px[1]), "pixel ({x}, {y}) domain location off the factor-4 grid: {px:?}");
    }
    let distinct_u: std::collections::BTreeSet<u32> = got.iter().map(|p| p[0]).collect();
    assert!(distinct_u.len() >= 4, "tessellation produced too few distinct u values: {distinct_u:?}");
}

fn hull_info(domain: u32, output_primitive: u32, output_control_points: u32) -> TessInfo {
    TessInfo { stage: 3, input_control_points: 1, output_control_points, domain, output_primitive, output_position: false, patch_const_vectors: 0 }
}

fn quad_ring(layout: TfLayout, tf_offset_dwords: usize, slots: &[f32]) -> Vec<u32> {
    let mut ring = vec![0u32; tf_offset_dwords];
    if layout == TfLayout::Gfx6To8 {
        ring.push(TB_TESS_GFX6_CONTROL_WORD);
    }
    for &f in slots {
        ring.extend([f.to_bits(); 6]);
    }
    ring
}

unsafe fn render_quad_with_passthrough(layout: TfLayout, ring: Vec<u32>, consts: [u32; 2]) -> Rendered {
    let ds_ex = console_quad_ds();
    let t = translate(&ds_ex).unwrap_or_else(|e| panic!("{e}"));
    let ds_code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{}", t.hlsl));
    let pt = passthrough_hull(&hull_info(3, 3, 1), &ds_ex, Partitioning::Integer, layout).unwrap_or_else(|e| panic!("{e}"));
    let vs = dxc::compile(&pt.vs, "vs_6_2").unwrap_or_else(|e| panic!("{e}\n{}", pt.vs));
    let hs = dxc::compile(&pt.hs, "hs_6_2").unwrap_or_else(|e| panic!("{e}\n{}", pt.hs));
    render(&Pipe { vs: &vs, hs: &hs, ds: &ds_code, patches: 2, control_points: 1, ring, consts }).unwrap_or_else(|e| panic!("{e}\n{}", pt.hs))
}

fn on_grid(bits: u32, n: u32) -> bool {
    (0..=n).any(|k| (k as f32 / n as f32).to_bits() == bits)
}

#[test]
fn passthrough_hull_reads_the_hardware_factor_layout() {
    let pt = passthrough_hull(&hull_info(3, 3, 1), &console_quad_ds(), Partitioning::FractionalOdd, TfLayout::Gfx6To8).unwrap_or_else(|e| panic!("{e}"));
    for needle in [
        "ByteAddressBuffer tb_tf : register(t0, space4000006);",
        "cbuffer TbTess : register(b0, space4000006) { uint tb_tf_offset; uint tb_patch_offset; };",
        "uint b = tb_tf_offset + 4u + (tb_patch_offset + pid) * 24u;",
        "o.tb_tf0[3] = asfloat(tb_tf.Load(b + 12u));",
        "o.tb_tf1[1] = asfloat(tb_tf.Load(b + 20u));",
        "[partitioning(\"fractional_odd\")]",
        "[outputtopology(\"triangle_cw\")]",
        "[outputcontrolpoints(1)]",
        "float4 i0 : CPOS0;",
    ] {
        assert!(pt.hs.contains(needle), "missing {needle:?}\n{}", pt.hs);
    }
    let tri = ds(&console_ds(), Some(tess_info(4, 3, 2)), vec![sig("CPOS", 0, 0, 3, 0, 0xf)], tri_patch_sig(), vec![sig("SV_Position", 0, 1, 3, 0, 0xf)]);
    let pt = passthrough_hull(&hull_info(2, 4, 3), &tri, Partitioning::Integer, TfLayout::Gfx9Plus).unwrap_or_else(|e| panic!("{e}"));
    for needle in ["uint b = tb_tf_offset + 0u + (tb_patch_offset + pid) * 16u;", "o.tb_tf1 = asfloat(tb_tf.Load(b + 12u));", "float4 p4 : PATCHDATA0;", "[outputtopology(\"triangle_ccw\")]"] {
        assert!(pt.hs.contains(needle), "missing {needle:?}\n{}", pt.hs);
    }
}

#[test]
fn passthrough_hulls_compile_for_every_domain_and_partitioning() {
    let outs = || vec![sig("SV_Position", 0, 1, 3, 0, 0xf)];
    let cases = [
        (ds(&console_ds(), Some(tess_info(4, 1, 1)), vec![], vec![], outs()), hull_info(1, 2, 1)),
        (ds(&console_ds(), Some(tess_info(4, 3, 2)), vec![sig("CPOS", 0, 0, 3, 0, 0xf)], tri_patch_sig(), outs()), hull_info(2, 3, 3)),
        (console_quad_ds(), hull_info(3, 1, 1)),
    ];
    for (dse, hi) in &cases {
        for part in [Partitioning::Integer, Partitioning::Pow2, Partitioning::FractionalOdd, Partitioning::FractionalEven] {
            for layout in [TfLayout::Gfx6To8, TfLayout::Gfx9Plus] {
                let pt = passthrough_hull(hi, dse, part, layout).unwrap_or_else(|e| panic!("{e}"));
                dxc::compile(&pt.hs, "hs_6_2").unwrap_or_else(|e| panic!("{e}\n{}", pt.hs));
            }
        }
    }
    dxc::compile(tombox_xshader::hlsl::PASSTHROUGH_VS, "vs_6_2").unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn passthrough_hull_refusals() {
    let quad = console_quad_ds();
    let refuse = |hi: TessInfo, d: &ShaderExtract| passthrough_hull(&hi, d, Partitioning::Integer, TfLayout::Gfx9Plus).err().unwrap_or_default();
    assert_eq!(refuse(tess_info(4, 1, 3), &quad), "pass-through hull needs hull-shader PSV0 tessellator info");
    assert_eq!(refuse(hull_info(2, 3, 1), &quad), "hull domain 2 differs from domain shader domain 3");
    assert_eq!(refuse(hull_info(3, 3, 4), &quad), "hull outputs 4 control points, domain shader reads 1");
    assert_eq!(refuse(hull_info(3, 2, 1), &quad), "tessellator output primitive 2 is not valid for domain 3");
    let lines = ds(&console_ds(), Some(tess_info(4, 1, 1)), vec![], vec![], vec![]);
    assert_eq!(refuse(hull_info(1, 3, 1), &lines), "tessellator output primitive 3 is not valid for domain 1");
    let mut no_info = quad.clone();
    no_info.gs_info = None;
    assert_eq!(refuse(hull_info(3, 3, 1), &no_info), "Domain shader without PSV0 tessellator info");
    let mut vertex = quad.clone();
    vertex.stage = Stage::Vertex;
    assert_eq!(refuse(hull_info(3, 3, 1), &vertex), "pass-through hull feeds a domain shader, not Vertex");
}

#[test]
fn gpu_passthrough_hull_takes_each_patch_factors_from_the_ring() {
    let r = unsafe { render_quad_with_passthrough(TfLayout::Gfx9Plus, quad_ring(TfLayout::Gfx9Plus, 0, &[2.0, 4.0]), [0, 0]) };
    let mut off_half_grid = 0;
    for (k, px) in r.pixels.iter().enumerate() {
        let (x, y) = (k % W, k / W);
        assert_ne!(px[3], CLEAR, "pixel ({x}, {y}) not covered: {px:?}");
        assert_eq!(px[2] + 1, px[3], "pixel ({x}, {y}): DS patch id differs from SV_PrimitiveID: {px:?}");
        let n = if x < W / 2 { 2 } else { 4 };
        assert_eq!(px[2], (n == 4) as u32, "pixel ({x}, {y}) in the wrong patch: {px:?}");
        assert!(on_grid(px[0], n) && on_grid(px[1], n), "pixel ({x}, {y}) off the factor-{n} grid: {px:?}");
        off_half_grid += (n == 4 && !(on_grid(px[0], 2) && on_grid(px[1], 2))) as u32;
    }
    assert!(off_half_grid > 0, "patch 1 shows no factor-4 vertices, so its factors were not read");
}

#[test]
fn gpu_passthrough_hull_honours_the_gfx6_control_word_and_both_offsets() {
    let base = unsafe { render_quad_with_passthrough(TfLayout::Gfx9Plus, quad_ring(TfLayout::Gfx9Plus, 0, &[2.0, 4.0]), [0, 0]) }.pixels;
    let gcn_ring = quad_ring(TfLayout::Gfx6To8, 5, &[16.0, 2.0, 4.0]);
    let gcn = unsafe { render_quad_with_passthrough(TfLayout::Gfx6To8, gcn_ring.clone(), [20, 1]) }.pixels;
    assert_eq!(gcn, base, "control word at the TF offset, factors 4 bytes later, patch offset 1");
    let misread = unsafe { render_quad_with_passthrough(TfLayout::Gfx9Plus, gcn_ring, [20, 1]) }.pixels;
    assert_ne!(misread, base, "reading a GFX6-8 ring without the control-word shift must not match");
}

const DS_ISOLINE_LOG: &str = "RWByteAddressBuffer tb_log : register(u0);
struct SCp { uint tb_pad : TBPAD; };
struct SPatch { float tb_tf0[2] : SV_TessFactor; };
struct O { float4 p : SV_Position; uint3 t : TEXCOORD0; };
[domain(\"isoline\")]
O main(float2 loc : SV_DomainLocation, uint pid : SV_PrimitiveID, const OutputPatch<SCp, 1> cp, SPatch pc) {
    uint k;
    tb_log.InterlockedAdd(0, 1u, k);
    if (k < 120u) tb_log.Store2(8u + 8u * k, uint2(asuint(loc.x), asuint(loc.y)));
    O o;
    o.p = float4(loc.x, loc.y, 0.0, 1.0);
    o.t = uint3(0u, 0u, pid);
    return o;
}";

#[test]
fn gpu_isoline_ring_dword0_is_line_detail_and_dword1_is_line_density() {
    let lines = ds(&console_ds(), Some(tess_info(4, 1, 1)), vec![], vec![sig("SV_TessFactor", 0, 15, 3, 0, 1), sig("SV_TessFactor", 1, 16, 3, 1, 1)], vec![sig("SV_Position", 0, 1, 3, 0, 0xf)]);
    let pt = passthrough_hull(&hull_info(1, 2, 1), &lines, Partitioning::Integer, TfLayout::Gfx9Plus).unwrap_or_else(|e| panic!("{e}"));
    let vs = dxc::compile(&pt.vs, "vs_6_2").unwrap_or_else(|e| panic!("{e}"));
    let hs = dxc::compile(&pt.hs, "hs_6_2").unwrap_or_else(|e| panic!("{e}\n{}", pt.hs));
    let dsc = dxc::compile(DS_ISOLINE_LOG, "ds_6_2").unwrap_or_else(|e| panic!("{e}"));
    let r = unsafe { render(&Pipe { vs: &vs, hs: &hs, ds: &dsc, patches: 1, control_points: 1, ring: vec![3.0f32.to_bits(), 2.0f32.to_bits()], consts: [0, 0] }) }.unwrap_or_else(|e| panic!("{e}\n{}", pt.hs));
    let n = (r.log[0] as usize).min(120);
    assert!(n > 0, "the domain shader never ran");
    let points: Vec<(u32, u32)> = (0..n).map(|k| (r.log[2 + 2 * k], r.log[3 + 2 * k])).collect();
    let us: std::collections::BTreeSet<u32> = points.iter().map(|p| p.0).collect();
    let vs_: std::collections::BTreeSet<u32> = points.iter().map(|p| p.1).collect();
    assert_eq!(vs_, [0.0f32, 0.5].iter().map(|f| f.to_bits()).collect(), "line density 2 gives lines at v = 0 and 0.5: {points:?}");
    assert_eq!(us.len(), 4, "line detail 3 gives 4 points per line: {points:?}");
}
