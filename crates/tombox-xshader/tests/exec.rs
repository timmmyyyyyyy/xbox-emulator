#![cfg(windows)]
#![allow(dead_code)]

#[path = "../../tools/xreplay/src/d3d.rs"]
mod d3d;
#[path = "exec_vop/mod.rs"]
mod vop;

use core::ffi::c_void;
use d3d::{Gpu, Guid};
use std::sync::{Mutex, OnceLock};
use tombox_xshader::hlsl::translate;
use tombox_xshader::meta::SigElement;
use tombox_xshader::{dxc, ShaderExtract, Stage};

const S_ENDPGM: u32 = 0xbf81_0000;
const LIT: u32 = 0xff;
const VCC: u32 = 106;
const SCC_SRC: u32 = 253;

extern "system" {
    fn LoadLibraryA(n: *const u8) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
}

pub fn v(n: u32) -> u32 {
    256 + n
}

pub fn ic(n: i32) -> u32 {
    if n >= 0 { 128 + n as u32 } else { 192 + (-n) as u32 }
}

pub fn vop1(op: u32, vdst: u32, src0: u32) -> u32 {
    0x7e00_0000 | (vdst << 17) | (op << 9) | src0
}

pub fn vop2(op: u32, vdst: u32, src0: u32, vsrc1: u32) -> u32 {
    (op << 25) | (vdst << 17) | (vsrc1 << 9) | src0
}

pub fn vopc(op: u32, src0: u32, vsrc1: u32) -> u32 {
    0x7c00_0000 | (op << 17) | (vsrc1 << 9) | src0
}

pub fn vop3(op: u32, vdst: u32, srcs: [u32; 3]) -> [u32; 2] {
    [0xd400_0000 | (op << 16) | vdst, srcs[0] | (srcs[1] << 9) | (srcs[2] << 18)]
}

pub fn vop3m(op: u32, vdst: u32, srcs: [u32; 3], abs: u32, neg: u32, opsel: u32, clamp: bool) -> [u32; 2] {
    [0xd400_0000 | (op << 16) | ((clamp as u32) << 15) | (opsel << 11) | (abs << 8) | vdst, srcs[0] | (srcs[1] << 9) | (srcs[2] << 18) | (neg << 29)]
}

pub fn vop3b(op: u32, vdst: u32, sdst: u32, srcs: [u32; 3]) -> [u32; 2] {
    [0xd400_0000 | (op << 16) | (sdst << 8) | vdst, srcs[0] | (srcs[1] << 9) | (srcs[2] << 18)]
}

pub fn vop3p(op: u32, vdst: u32, srcs: [u32; 3], opsel: u32, opsel_hi: u32, neg_lo: u32, neg_hi: u32) -> [u32; 2] {
    [
        0xcc00_0000 | (op << 16) | (((opsel_hi >> 2) & 1) << 14) | (opsel << 11) | (neg_hi << 8) | vdst,
        srcs[0] | (srcs[1] << 9) | (srcs[2] << 18) | ((opsel_hi & 3) << 27) | (neg_lo << 29),
    ]
}

pub fn sop2(op: u32, sdst: u32, s0: u32, s1: u32) -> u32 {
    0x8000_0000 | (op << 23) | (sdst << 16) | (s1 << 8) | s0
}

pub fn sop1(op: u32, sdst: u32, s0: u32) -> u32 {
    0xbe80_0000 | (sdst << 16) | (op << 8) | s0
}

pub fn sopk(op: u32, sdst: u32, imm: u16) -> u32 {
    0xb000_0000 | (op << 23) | (sdst << 16) | imm as u32
}

pub fn sopc(op: u32, s0: u32, s1: u32) -> u32 {
    0xbf00_0000 | (op << 16) | (s1 << 8) | s0
}

pub fn sopp(op: u32, imm: u16) -> u32 {
    0xbf80_0000 | (op << 16) | imm as u32
}

pub struct Prog {
    pub w: Vec<u32>,
}

impl Prog {
    pub fn new() -> Prog {
        Prog { w: Vec::new() }
    }
    pub fn raw(&mut self, w: &[u32]) -> &mut Self {
        self.w.extend_from_slice(w);
        self
    }
    pub fn sv(&mut self, s: u32, val: u32) -> &mut Self {
        self.w.extend_from_slice(&[sop1(3, s, LIT), val]);
        self
    }
    pub fn vv(&mut self, r: u32, val: u32) -> &mut Self {
        self.w.extend_from_slice(&[vop1(1, r, LIT), val]);
        self
    }
    pub fn s2v(&mut self, r: u32, s: u32) -> &mut Self {
        self.w.push(vop1(1, r, s));
        self
    }
    pub fn scc_to_v(&mut self, r: u32) -> &mut Self {
        self.w.push(sop2(0x0a, 100, ic(1), ic(0)));
        self.w.push(vop1(1, r, 100));
        self
    }
    pub fn vcc_to_v(&mut self, r: u32) -> &mut Self {
        self.w.extend_from_slice(&[vop1(1, 200, ic(1))]);
        self.w.push(vop2(0x01, r, ic(0), 200));
        self
    }
    pub fn words(&self) -> Vec<u32> {
        let mut w = self.w.clone();
        w.extend_from_slice(&[0xf800_180f, 0x0302_0100, S_ENDPGM]);
        w
    }
}

fn sig(name: &str, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index: 0, sysval, comp_type, register, mask, rw_mask: 0 }
}

pub fn shader(words: &[u32]) -> ShaderExtract {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderExtract {
        stage: Stage::Pixel,
        shader_model: (6, 0),
        bitcode_stripped: true,
        fssm: Vec::new(),
        entry: Some("main".into()),
        isa_data: isa.clone(),
        isa,
        isa_offset_in_fssm: 0,
        isa_preamble: [0; 4],
        psv_numthreads: None,
        has_input_sig: true,
        has_output_sig: true,
        has_psv0: false,
        has_root_sig: false,
        input_sig: vec![],
        output_sig: vec![sig("SV_Target", 64, 1, 0, 0xf)],
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: None,
        root_sig: None,
    }
}

struct Ctx {
    gpu: Gpu,
    rs: *mut c_void,
    vs: Vec<u8>,
    rt: *mut c_void,
    rb: *mut c_void,
    rtv: usize,
}

unsafe impl Send for Ctx {}

static CTX: OnceLock<Mutex<Ctx>> = OnceLock::new();

const VS: &str = "float4 main(uint id : SV_VertexID) : SV_Position { float2 p = float2((id << 1) & 2, id & 2); return float4(p * float2(2, -2) + float2(-1, 1), 0, 1); }";

unsafe fn warp_gpu() -> Gpu {
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

unsafe fn ctx_new() -> Ctx {
    let mut gpu = if std::env::var_os("TOMBOX_EXEC_HW").is_some() { Gpu::new(std::env::var_os("TOMBOX_EXEC_DEBUG").is_some()).expect("d3d12 device") } else { warp_gpu() };
    gpu.open = false;
    let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
        core::mem::transmute(GetProcAddress(d3d, b"D3D12SerializeRootSignature\0".as_ptr()));
    let mut desc = [0u8; 40];
    desc[32] = 1;
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    assert!(ser(desc.as_ptr(), 1, &mut blob, &mut err) >= 0, "serialize root signature");
    let p = vcall!(blob, 3, fn() -> *const u8);
    let n = vcall!(blob, 4, fn() -> usize);
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(gpu.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, p, n, &d3d::IID_ROOTSIG, &mut rs);
    assert!(hr >= 0, "CreateRootSignature {hr:#x}");
    let vs = dxc::compile(VS, "vs_6_0").expect("vs compile");
    let mut td = [0u8; 0x38];
    td[0..4].copy_from_slice(&3u32.to_le_bytes());
    td[0x10..0x18].copy_from_slice(&1u64.to_le_bytes());
    td[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    td[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    td[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    td[0x20..0x24].copy_from_slice(&3u32.to_le_bytes());
    td[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    td[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
    let rt = gpu.committed(&td, d3d::default_heap(), 4).expect("rt");
    let rb = gpu.buffer(256, 3, d3d::COPY_DEST, false).expect("readback");
    let (_h, rtv, _, _) = gpu.heap(2, 1, false).expect("rtv heap");
    vcall!(gpu.device, 20, fn(*mut c_void, *const c_void, usize) -> (), rt, core::ptr::null(), rtv);
    Ctx { gpu, rs, vs, rt, rb, rtv }
}

unsafe fn draw(c: &mut Ctx, ps: &[u8]) -> Result<[u32; 4], String> {
    let rs = c.rs;
    draw_with(c, ps, rs, &|_| {}, &|_| {})
}

unsafe fn draw_with(c: &mut Ctx, ps: &[u8], rs: *mut c_void, pre: &dyn Fn(*mut c_void), post: &dyn Fn(*mut c_void)) -> Result<[u32; 4], String> {
    draw_graphics(c, ps, rs, pre, post, None)
}

#[repr(C)]
struct VertexElement {
    semantic: *const u8,
    semantic_index: u32,
    format: u32,
    slot: u32,
    offset: u32,
    input_class: u32,
    step_rate: u32,
}

unsafe fn draw_graphics(
    c: &mut Ctx,
    ps: &[u8],
    rs: *mut c_void,
    pre: &dyn Fn(*mut c_void),
    post: &dyn Fn(*mut c_void),
    vertex: Option<(&[u8], &[VertexElement])>,
) -> Result<[u32; 4], String> {
    let mut d = vec![0u8; 656];
    let put32 = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |d: &mut Vec<u8>, o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    let (vs, layout) = vertex.unwrap_or((&c.vs, &[]));
    put64(&mut d, 0, rs as u64);
    put64(&mut d, 8, vs.as_ptr() as u64);
    put64(&mut d, 16, vs.len() as u64);
    put64(&mut d, 24, ps.as_ptr() as u64);
    put64(&mut d, 32, ps.len() as u64);
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
    if !layout.is_empty() {
        put64(&mut d, 552, layout.as_ptr() as u64);
        put32(&mut d, 560, layout.len() as u32);
    }
    put32(&mut d, 572, 3);
    put32(&mut d, 576, 1);
    put32(&mut d, 580, 3);
    put32(&mut d, 616, 1);
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(c.gpu.device, 10, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        return Err(format!("CreateGraphicsPipelineState hr={:#010x}{}", hr as u32, c.gpu.messages()));
    }
    c.gpu.begin();
    let l = c.gpu.list;
    pre(l);
    let clear = [0xdead_beefu32 as f32; 4];
    vcall!(l, 48, fn(usize, *const f32, u32, *const c_void) -> (), c.rtv, clear.as_ptr(), 0, core::ptr::null());
    vcall!(l, 46, fn(u32, *const usize, i32, *const usize) -> (), 1, &c.rtv, 0, core::ptr::null());
    if rs == c.rs {
        vcall!(l, 30, fn(*mut c_void) -> (), c.rs);
    }
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    let vp = [0f32, 0., 1., 1., 0., 1.];
    vcall!(l, 21, fn(u32, *const f32) -> (), 1, vp.as_ptr());
    let sc = [0i32, 0, 1, 1];
    vcall!(l, 22, fn(u32, *const i32) -> (), 1, sc.as_ptr());
    vcall!(l, 20, fn(u32) -> (), 4);
    vcall!(l, 12, fn(u32, u32, u32, u32) -> (), 3, 1, 0, 0);
    c.gpu.barrier(c.rt, 4, d3d::COPY_SOURCE);
    let mut dst = [0u8; 40];
    dst[0..8].copy_from_slice(&(c.rb as u64).to_le_bytes());
    dst[8..12].copy_from_slice(&1u32.to_le_bytes());
    dst[24..28].copy_from_slice(&3u32.to_le_bytes());
    dst[28..32].copy_from_slice(&1u32.to_le_bytes());
    dst[32..36].copy_from_slice(&1u32.to_le_bytes());
    dst[36..40].copy_from_slice(&1u32.to_le_bytes());
    let mut dst2 = [0u8; 48];
    dst2[..40].copy_from_slice(&dst);
    dst2[40..44].copy_from_slice(&256u32.to_le_bytes());
    let mut src = [0u8; 48];
    src[0..8].copy_from_slice(&(c.rt as u64).to_le_bytes());
    vcall!(l, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), dst2.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
    c.gpu.barrier(c.rt, d3d::COPY_SOURCE, 4);
    post(l);
    let r = c.gpu.submit();
    d3d::release(pso);
    r?;
    let b = Gpu::map_read(c.rb, 16).ok_or("map readback")?;
    let mut o = [0u32; 4];
    for i in 0..4 {
        o[i] = u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    }
    Ok(o)
}

#[test]
fn vertex_fetch_reads_sparse_attributes_from_a_vertex_buffer() {
    let input = |name, format, offset| VertexElement {
        semantic: name, semantic_index: 0, format, slot: 0, offset, input_class: 0, step_rate: 0,
    };
    let layout = [
        input(b"POSITION\0".as_ptr(), 16, 0),
        input(b"NORMAL\0".as_ptr(), 2, 8),
        input(b"TEXCOORD\0".as_ptr(), 42, 24),
    ];
    let mut vs = shader(&[0xbe84_2104, 0xf800_00cf, 0x0706_0504, 0xf800_0a0f, 0x0b0a_0908, S_ENDPGM]);
    vs.stage = Stage::Vertex;
    vs.input_sig = vec![sig("POSITION", 0, 3, 0, 3), sig("NORMAL", 0, 3, 1, 0xf), sig("TEXCOORD", 0, 1, 3, 4)];
    vs.input_sig[0].rw_mask = 3;
    vs.input_sig[2].rw_mask = 4;
    vs.output_sig = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)];
    let vs = translate(&vs).unwrap();
    let vertex_code = dxc::compile(&vs.hlsl, &dxc::profile_for(vs.profile)).unwrap();
    let pixel_code = dxc::compile(
        "uint4 main(float4 p : SV_Position, nointerpolation uint4 v : TEXCOORD0) : SV_Target { return v ^ uint4(uint2(p.xy), 0, 0); }",
        "ps_6_0",
    ).unwrap();
    let vertices: Vec<u8> = [(-1.0f32, 1.0f32), (3.0, 1.0), (-1.0, -3.0)]
        .into_iter()
        .flat_map(|(x, y)| [x.to_bits(), y.to_bits(), 0x41a8_0000, 0, 0, 0, 0x1234])
        .flat_map(u32::to_le_bytes)
        .collect();
    let mut c = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() })).lock().unwrap_or_else(|e| e.into_inner());
    let result = unsafe {
        let vb = c.gpu.buffer(vertices.len() as u64, 2, 0xac3, false).unwrap();
        assert!(Gpu::map_write(vb, &vertices));
        let view = [Gpu::gpu_va(vb), ((28u64) << 32) | vertices.len() as u64];
        let rs = c.rs;
        let result = draw_graphics(&mut c, &pixel_code, rs, &|list| {
            vcall!(list, 44, fn(u32, u32, *const u64) -> (), 0, 1, view.as_ptr());
        }, &|_| {}, Some((&vertex_code, &layout)));
        d3d::release(vb);
        result
    };
    assert_eq!(result.unwrap_or_else(|e| panic!("{e}\n{}", vs.hlsl)), [0, 0, 0x1234, 0x3f80_0000]);
}

#[test]
fn d3d12_takes_a_changed_container_only_with_the_recomputed_dxil_hash() {
    use tombox_xshader::dxil_hash::{stamp, stored, Stored};
    let expect = [0x11, 0x22, 0x33, 0x44];
    let code = dxc::compile("uint4 main() : SV_Target { return uint4(0x11, 0x22, 0x33, 0x44); }", "ps_6_0").unwrap();
    assert_eq!(stored(&code), Ok(Stored::Retail));
    let parts = tombox_xshader::Dxbc::parse(&code).unwrap().chunks;
    let names: Vec<String> = parts.iter().map(|p| String::from_utf8_lossy(&p.fourcc).into_owned()).collect();
    let part = [b"HASH", b"ILDN", b"STAT"]
        .into_iter()
        .find_map(|tag| parts.iter().find(|p| p.fourcc == *tag && p.size != 0))
        .unwrap_or_else(|| panic!("no part to change without changing the program, container has {names:?}"));
    let mut stale = code.clone();
    stale[part.offset + part.size - 1] ^= 1;
    assert_eq!(stored(&stale), Ok(Stored::Mismatch));
    let mut stamped = stale.clone();
    stamp(&mut stamped).unwrap();
    assert_eq!(stored(&stamped), Ok(Stored::Retail));
    assert_ne!(stamped[4..20], code[4..20]);
    let mut zero = code.clone();
    zero[4..20].fill(0);
    let mut c = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() })).lock().unwrap_or_else(|e| e.into_inner());
    let changed = String::from_utf8_lossy(&part.fourcc).into_owned();
    unsafe {
        assert_eq!(draw(&mut c, &code), Ok(expect), "container as DXC wrote it");
        assert_eq!(draw(&mut c, &stamped), Ok(expect), "{changed} part changed, digest recomputed");
        let refused = draw(&mut c, &stale).expect_err("a stale digest was accepted");
        assert!(refused.contains("CreateGraphicsPipelineState"), "{changed} part changed, digest left: {refused}");
        let refused = draw(&mut c, &zero).expect_err("a zero digest was accepted");
        assert!(refused.contains("CreateGraphicsPipelineState"), "zero digest: {refused}");
    }
}

#[test]
fn merged_vertex_shader_seeds_wave_info_for_the_program_wave_size() {
    let build = |bfm: u32, cselect: u32| {
        let mut words = vec![sop2(0x28, 32, 3, LIT), 0x80000, sop2(bfm, 126, 32, ic(0)), sopc(0x06, 32, ic(if bfm == 0x24 { 32 } else { 64 })), sop2(cselect, 126, ic(-1), 126)];
        words.extend_from_slice(&[0xbe84_2104, 0xf800_00cf, 0x0c0b_0a09, 0xf800_0a0f, 0x0909_0805, S_ENDPGM]);
        let mut vs = shader(&words);
        vs.stage = Stage::Vertex;
        vs.input_sig = vec![sig("POSITION", 0, 3, 0, 0xf), sig("SV_VertexID", 6, 1, 1, 1), sig("SV_InstanceID", 8, 1, 2, 1)];
        vs.input_sig[0].rw_mask = 0xf;
        vs.output_sig = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)];
        translate(&vs).unwrap().hlsl
    };
    let wave32 = build(0x24, 0x0a);
    assert!(wave32.contains("s3 = 0x20u;") && !wave32.contains("s3 = 0x40u;"), "{wave32}");
    assert!(wave32.contains("v5 = (uint)(i.i1);") && wave32.contains("v8 = (uint)(i.i2);"), "{wave32}");
    let wave64 = build(0x25, 0x0b);
    assert!(wave64.contains("s3 = 0x40u;") && !wave64.contains("s3 = 0x20u;"), "{wave64}");
    if dxc::available() {
        dxc::compile(&wave32, "vs_6_0").expect("dxc compiles the wave32 merged vertex shader");
        dxc::compile(&wave64, "vs_6_0").expect("dxc compiles the wave64 merged vertex shader");
    }
}

#[test]
fn v_interp_mov_integer_reads_all_three_vertices() {
    let mut program = Prog::new();
    program.raw(&[0xc802_0002, 0xc806_0100, 0xc80a_0201, 0xc80e_0301]);
    let mut ps = shader(&program.words());
    ps.input_sig = vec![
        sig("SV_Position", 1, 3, 0, 0xf),
        sig("TEXCOORD", 0, 1, 1, 0xf),
    ];
    ps.input_sig[1].rw_mask = 0xf;
    let ps = translate(&ps).unwrap();
    let pixel_code = dxc::compile(&ps.hlsl, &dxc::profile_for(ps.profile))
        .unwrap_or_else(|e| panic!("{e}\n{}", ps.hlsl));
    let vertex_code = dxc::compile(
        "struct O { float4 pos : SV_Position; nointerpolation uint4 value : TEXCOORD0; };
         O main(uint id : SV_VertexID) {
             float2 p = float2((id << 1) & 2, id & 2);
             O o;
             o.pos = float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
             o.value = uint4(0x11u, 0x44u, 0x80u, 0x80000000u) + id * 0x100u;
             return o;
         }",
        "vs_6_0",
    ).unwrap();
    let mut c = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() })).lock().unwrap_or_else(|e| e.into_inner());
    let rs = c.rs;
    let result = unsafe { draw_graphics(&mut c, &pixel_code, rs, &|_| {}, &|_| {}, Some((&vertex_code, &[]))) };
    assert_eq!(result.unwrap_or_else(|e| panic!("{e}\n{}", ps.hlsl)), [0x11, 0x144, 0x280, 0x80000200]);
}

pub fn hlsl_of(words: &[u32]) -> Result<String, String> {
    translate(&shader(words)).map(|t| t.hlsl)
}

pub fn run(words: &[u32]) -> Result<[u32; 4], String> {
    let t = translate(&shader(words))?;
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { draw(&mut c, &code) }.map_err(|e| format!("{e}\n{}", t.hlsl))
}

pub struct Tex {
    pub w: u32,
    pub h: u32,
    pub mips: u32,
    pub texel: fn(u32, u32, u32) -> [f32; 4],
}

pub struct ResOut {
    pub rt: [u32; 4],
    pub buf: Vec<u32>,
}

unsafe fn ser_root(params: &[(u32, u32)], samplers: &[[u8; 52]]) -> Result<Vec<u8>, String> {
    let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
        core::mem::transmute(GetProcAddress(d3d, b"D3D12SerializeRootSignature\0".as_ptr()));
    let ranges: Vec<[u32; 5]> = params.iter().map(|&(ty, n)| [ty, n, 0, 0, 0xffff_ffff]).collect();
    let mut ps = vec![0u8; 32 * params.len().max(1)];
    for (i, _) in params.iter().enumerate() {
        let o = i * 32;
        ps[o..o + 4].copy_from_slice(&0u32.to_le_bytes());
        ps[o + 8..o + 12].copy_from_slice(&1u32.to_le_bytes());
        ps[o + 16..o + 24].copy_from_slice(&(ranges[i].as_ptr() as u64).to_le_bytes());
        ps[o + 24..o + 28].copy_from_slice(&0u32.to_le_bytes());
    }
    let mut ss: Vec<u8> = Vec::new();
    for (i, s) in samplers.iter().enumerate() {
        let mut x = [0u8; 52];
        x[..36].copy_from_slice(&s[..36]);
        x[36..40].copy_from_slice(&s[44..48]);
        x[40..44].copy_from_slice(&s[48..52]);
        x[44..48].copy_from_slice(&(i as u32).to_le_bytes());
        ss.extend_from_slice(&x);
    }
    let mut desc = [0u8; 40];
    desc[0..4].copy_from_slice(&(params.len() as u32).to_le_bytes());
    desc[8..16].copy_from_slice(&(ps.as_ptr() as u64).to_le_bytes());
    desc[16..20].copy_from_slice(&(samplers.len() as u32).to_le_bytes());
    desc[24..32].copy_from_slice(&(if ss.is_empty() { 0 } else { ss.as_ptr() as u64 }).to_le_bytes());
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    if ser(desc.as_ptr(), 1, &mut blob, &mut err) < 0 {
        let msg = if err.is_null() { String::new() } else { std::ffi::CStr::from_ptr(vcall!(err, 3, fn() -> *const i8)).to_string_lossy().into_owned() };
        return Err(format!("serialize root signature: {msg}"));
    }
    let p = vcall!(blob, 3, fn() -> *const u8);
    let n = vcall!(blob, 4, fn() -> usize);
    Ok(core::slice::from_raw_parts(p, n).to_vec())
}

pub fn sampler(filter: u32, addr: u32, cmp: u32) -> [u8; 52] {
    let mut s = [0u8; 52];
    let w = |s: &mut [u8; 52], o: usize, v: u32| s[o..o + 4].copy_from_slice(&v.to_le_bytes());
    w(&mut s, 0, filter);
    w(&mut s, 4, addr);
    w(&mut s, 8, addr);
    w(&mut s, 12, addr);
    w(&mut s, 20, 1);
    w(&mut s, 24, cmp);
    w(&mut s, 48, 1000f32.to_bits());
    s
}

pub fn run_res(words: &[u32], root: tombox_xshader::meta::RootSig, buf: Option<&[u32]>, tex: Option<&Tex>, samp: [u8; 52]) -> Result<ResOut, String> {
    run_res_ex(words, root, buf, tex, samp, false)
}

pub fn run_res_ex(words: &[u32], root: tombox_xshader::meta::RootSig, buf: Option<&[u32]>, tex: Option<&Tex>, samp: [u8; 52], pos: bool) -> Result<ResOut, String> {
    run_res_psv(words, root, buf, tex, samp, pos, &[])
}

fn shader_psv(words: &[u32], root: tombox_xshader::meta::RootSig, psv: &[tombox_xshader::meta::PsvResource]) -> ShaderExtract {
    let mut ex = shader_root(words, root);
    if !psv.is_empty() {
        ex.has_psv0 = true;
        ex.resources = psv.to_vec();
    }
    ex
}

pub fn run_res_psv(words: &[u32], root: tombox_xshader::meta::RootSig, buf: Option<&[u32]>, tex: Option<&Tex>, samp: [u8; 52], pos: bool, psv: &[tombox_xshader::meta::PsvResource]) -> Result<ResOut, String> {
    use tombox_xshader::meta::RootParamKind;
    let mut ex = shader_psv(words, root.clone(), psv);
    if pos {
        ex.input_sig = vec![sig("SV_Position", 1, 3, 0, 0xf)];
    }
    let t = translate(&ex)?;
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let params: Vec<(u32, u32)> = root
        .params
        .iter()
        .map(|p| match &p.kind {
            RootParamKind::Table(r) => (r[0].range_type, r[0].num),
            _ => (0, 1),
        })
        .collect();
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let g = &c.gpu;
        let blob = ser_root(&params, &[])?;
        let mut rs: *mut c_void = core::ptr::null_mut();
        let hr = vcall!(g.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, blob.as_ptr(), blob.len(), &d3d::IID_ROOTSIG, &mut rs);
        if hr < 0 {
            return Err(format!("CreateRootSignature {hr:#x}"));
        }
        let (vh, vcpu, vgpu, vinc) = g.heap(0, 256, true)?;
        let (sh, scpu, sgpu, sinc) = g.heap(1, 64, true)?;
        let mut kill: Vec<*mut c_void> = vec![rs, vh, sh];
        let nbytes = 1024u64;
        let mut ubuf: *mut c_void = core::ptr::null_mut();
        let mut up: *mut c_void = core::ptr::null_mut();
        if let Some(data) = buf {
            ubuf = g.buffer(nbytes, 1, d3d::COPY_DEST, true)?;
            up = g.buffer(nbytes, 2, d3d::GENERIC_READ, false)?;
            let mut bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
            bytes.resize(nbytes as usize, 0);
            Gpu::map_write(up, &bytes);
            kill.push(ubuf);
            kill.push(up);
        }
        let mut texr: *mut c_void = core::ptr::null_mut();
        let mut tup: *mut c_void = core::ptr::null_mut();
        let mut fps: Vec<[u8; 32]> = Vec::new();
        if let Some(tx) = tex {
            let mut td = [0u8; 0x38];
            td[0..4].copy_from_slice(&3u32.to_le_bytes());
            td[0x10..0x18].copy_from_slice(&(tx.w as u64).to_le_bytes());
            td[0x18..0x1c].copy_from_slice(&tx.h.to_le_bytes());
            td[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
            td[0x1e..0x20].copy_from_slice(&(tx.mips as u16).to_le_bytes());
            td[0x20..0x24].copy_from_slice(&2u32.to_le_bytes());
            td[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
            texr = g.committed(&td, d3d::default_heap(), d3d::COPY_DEST)?;
            let (f, total) = g.footprints(&td, tx.mips);
            fps = f;
            tup = g.buffer(total, 2, d3d::GENERIC_READ, false)?;
            let mut bytes = vec![0u8; total as usize];
            for (mip, fp) in fps.iter().enumerate() {
                let off = u64::from_le_bytes(fp[0..8].try_into().unwrap()) as usize;
                let pitch = u32::from_le_bytes(fp[24..28].try_into().unwrap()) as usize;
                let (mw, mh) = ((tx.w >> mip).max(1), (tx.h >> mip).max(1));
                for y in 0..mh {
                    for x in 0..mw {
                        let v = (tx.texel)(mip as u32, x, y);
                        for k in 0..4 {
                            let o = off + y as usize * pitch + x as usize * 16 + k * 4;
                            bytes[o..o + 4].copy_from_slice(&v[k].to_bits().to_le_bytes());
                        }
                    }
                }
            }
            Gpu::map_write(tup, &bytes);
            kill.push(texr);
            kill.push(tup);
        }
        let hlsl = &t.hlsl;
        let mut stride = 0u32;
        let mut fmt = 42u32;
        let mut raw = false;
        if hlsl.contains("RWByteAddressBuffer ubuf0") {
            fmt = 39;
            raw = true;
        } else if let Some(n) = hlsl.split("struct TbElem_ubuf").nth(1).and_then(|s| s.split("{ uint w[").nth(1)).and_then(|s| s.split(']').next()).and_then(|s| s.parse::<u32>().ok()) {
            stride = n * 4;
            fmt = 0;
        } else if hlsl.contains("RWStructuredBuffer<uint64_t> ubuf0") {
            stride = 8;
            fmt = 0;
        } else if hlsl.contains("RWStructuredBuffer<uint> ubuf0") {
            stride = 4;
            fmt = 0;
        } else if hlsl.contains("RWBuffer<uint64_t>") {
            fmt = 17;
        } else if hlsl.contains("RWBuffer<float4>") {
            fmt = 41;
        } else if hlsl.contains("RWBuffer<uint4>") {
            fmt = 3;
        } else if hlsl.contains("RWBuffer<float>") {
            fmt = 41;
        }
        let mut tbl_gpu = Vec::new();
        for (i, &(ty, _)) in params.iter().enumerate() {
            if ty == 3 {
                let cpu = scpu + i * 16 * sinc;
                for k in 0..16 {
                    vcall!(g.device, 22, fn(*const u8, usize) -> (), samp.as_ptr(), cpu + k * sinc);
                }
                tbl_gpu.push(sgpu + (i * 16 * sinc) as u64);
                continue;
            }
            let base = vcpu + i * 64 * vinc;
            tbl_gpu.push(vgpu + (i * 64 * vinc) as u64);
            if ty == 1 && !ubuf.is_null() {
                let esz = if stride != 0 { stride } else if fmt == 2 || fmt == 3 { 16 } else if fmt == 17 { 8 } else { 4 };
                for slot in 0..8u32 {
                    let first = (slot * 128 / esz) as u64;
                    let mut ud = [0u8; 40];
                    ud[0..4].copy_from_slice(&fmt.to_le_bytes());
                    ud[4..8].copy_from_slice(&1u32.to_le_bytes());
                    ud[8..16].copy_from_slice(&first.to_le_bytes());
                    let elems = nbytes as u32 / esz - first as u32;
                    ud[16..20].copy_from_slice(&elems.to_le_bytes());
                    ud[20..24].copy_from_slice(&stride.to_le_bytes());
                    ud[32..36].copy_from_slice(&(raw as u32).to_le_bytes());
                    vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), ubuf, core::ptr::null_mut(), ud.as_ptr(), base + slot as usize * vinc);
                }
            }
            if ty == 0 && !texr.is_null() {
                let mut sd = [0u8; 40];
                sd[0..4].copy_from_slice(&2u32.to_le_bytes());
                sd[4..8].copy_from_slice(&4u32.to_le_bytes());
                sd[8..12].copy_from_slice(&0x1688u32.to_le_bytes());
                sd[20..24].copy_from_slice(&tex.unwrap().mips.to_le_bytes());
                vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), texr, sd.as_ptr(), base);
            }
        }
        let (rb2, ubuf2, up2, texr2, tup2) = (c.rb, ubuf, up, texr, tup);
        let rb_buf = if !ubuf.is_null() { c.gpu.buffer(nbytes, 3, d3d::COPY_DEST, false)? } else { core::ptr::null_mut() };
        let _ = rb2;
        let gp = &c.gpu as *const Gpu;
        let fps2 = fps.clone();
        let heaps = [vh, sh];
        let pre = |l: *mut c_void| {
            let g = &*gp;
            if !ubuf2.is_null() {
                vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), ubuf2, 0, up2, 0, nbytes);
                g.barrier(ubuf2, d3d::COPY_DEST, 8);
            }
            if !texr2.is_null() {
                for (mip, fp) in fps2.iter().enumerate() {
                    let mut dst = [0u8; 48];
                    dst[0..8].copy_from_slice(&(texr2 as u64).to_le_bytes());
                    dst[16..20].copy_from_slice(&(mip as u32).to_le_bytes());
                    let mut src = [0u8; 48];
                    src[0..8].copy_from_slice(&(tup2 as u64).to_le_bytes());
                    src[8..12].copy_from_slice(&1u32.to_le_bytes());
                    src[16..48].copy_from_slice(fp);
                    vcall!(l, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), dst.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
                }
                g.barrier(texr2, d3d::COPY_DEST, 0x40 | 0x80);
            }
            vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 2, heaps.as_ptr());
            vcall!(l, 30, fn(*mut c_void) -> (), rs);
            for (i, &h) in tbl_gpu.iter().enumerate() {
                vcall!(l, 32, fn(u32, u64) -> (), i as u32, h);
            }
        };
        let post = |l: *mut c_void| {
            let g = &*gp;
            if !ubuf2.is_null() {
                g.barrier(ubuf2, 8, d3d::COPY_SOURCE);
                vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb_buf, 0, ubuf2, 0, nbytes);
            }
        };
        let r = draw_with(&mut c, &code, rs, &pre, &post);
        let out = r.map_err(|e| format!("{e}\n{}", t.hlsl)).map(|rt| {
            let buf = if rb_buf.is_null() { Vec::new() } else { Gpu::map_read(rb_buf, nbytes as usize).unwrap_or_default().chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect() };
            ResOut { rt, buf }
        });
        if !rb_buf.is_null() {
            kill.push(rb_buf);
        }
        for k in kill {
            d3d::release(k);
        }
        out
    }
}

pub struct Case {
    pub m: &'static str,
    pub pre: Vec<u32>,
    pub op: Vec<u32>,
    pub post: Vec<u32>,
    pub exp: Vec<u32>,
}

pub struct Cases {
    pub v: Vec<Case>,
}

pub const SCC1: u32 = 0xbf06_8080;
pub const SCC0: u32 = 0xbf06_8180;

impl Cases {
    pub fn new() -> Cases {
        Cases { v: Vec::new() }
    }
    pub fn add(&mut self, m: &'static str, pre: &Prog, op: &[u32], post: &Prog, exp: &[u32]) {
        self.v.push(Case { m, pre: pre.w.clone(), op: op.to_vec(), post: post.w.clone(), exp: exp.to_vec() });
    }
    pub fn run(&self, group: &str) {
        let mut fails = Vec::new();
        let mut bad: std::collections::BTreeSet<&str> = Default::default();
        let mut all: std::collections::BTreeSet<&str> = Default::default();
        for (i, c) in self.v.iter().enumerate() {
            all.insert(c.m);
            let isa: Vec<u8> = c.op.iter().flat_map(|w| w.to_le_bytes()).collect();
            let got_m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
            if got_m != c.m {
                fails.push(format!("#{i} {}: encoding decodes as {got_m}", c.m));
                bad.insert(c.m);
                continue;
            }
            let mut p = Prog::new();
            p.raw(&c.pre).raw(&c.op).raw(&c.post);
            match run(&p.words()) {
                Ok(r) => {
                    if r[..c.exp.len()] != c.exp[..] {
                        let h = hlsl_of(&p.words()).unwrap_or_default();
                        let body: String = h.lines().filter(|l| l.starts_with("    ") && !l.contains("TB_RTFMT")).collect::<Vec<_>>().join("\n");
                        fails.push(format!("#{i} {}: got {:08x?} want {:08x?}\n{body}", c.m, &r[..c.exp.len()], c.exp));
                        bad.insert(c.m);
                    }
                }
                Err(e) => {
                    let e: String = e.lines().take(3).collect::<Vec<_>>().join(" | ");
                    fails.push(format!("#{i} {}: {e}", c.m));
                    bad.insert(c.m);
                }
            }
        }
        let ok: Vec<&str> = all.iter().filter(|m| !bad.contains(*m)).copied().collect();
        if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{group}.txt")), ok.join("\n") + "\n");
        }
        println!("{group}: {} mnemonics verified, {} failing, {} cases", ok.len(), bad.len(), self.v.len());
        assert!(fails.is_empty(), "{group}: {} failing cases\n{}", fails.len(), fails.join("\n\n"));
    }
}

fn pr() -> Prog {
    Prog::new()
}

#[test]
fn scalar_null_reads_zero_after_discarded_writes() {
    let mut cases = Cases::new();
    for value in [1, 0x8000_0000, 0xffff_ffff] {
        let mut pre = pr();
        pre.sv(125, value);
        let mut post = pr();
        post.s2v(0, 125).s2v(1, 20);
        cases.add("s_mov_b32", &pre, &[sop1(3, 20, 125)], &post, &[0, 0]);
    }
    for (a, b, carry) in [(1, 2, 0), (0xffff_ffff, 1, 1)] {
        let mut pre = pr();
        pre.sv(10, a).sv(12, b);
        let mut post = pr();
        post.scc_to_v(0).s2v(1, 125);
        cases.add("s_add_u32", &pre, &[sop2(0, 125, 10, 12)], &post, &[carry, 0]);
    }
    let mut post = pr();
    post.scc_to_v(0).s2v(1, 125);
    cases.add("s_not_b32", &pr(), &[sop1(7, 125, 125)], &post, &[1, 0]);
    cases.run("scalar_null_reads_zero_after_discarded_writes");
}

#[test]
fn scalar_null_sopk_reads_do_not_reuse_discarded_values() {
    let mut cases = Cases::new();
    for value in [0x7fff_ffff, 0x8000_0000, 1, 0xffff_ffff] {
        for imm in [1, 0xffff, 0x7fff, 0x8000] {
            let mut pre = pr();
            pre.sv(125, value).raw(&[SCC1]);
            let mut post = pr();
            post.scc_to_v(0).s2v(1, 125);
            cases.add("s_addk_i32", &pre, &[sopk(0x0f, 125, imm)], &post, &[0, 0]);
        }
    }
    for (value, imm) in [(0x7fff_ffffu32, 1u16), (0x8000_0000, 0xffff)] {
        let mut pre = pr();
        pre.sv(20, value);
        let mut post = pr();
        post.s2v(0, 20).scc_to_v(1);
        let sum = value.wrapping_add(imm as i16 as i32 as u32);
        cases.add("s_addk_i32", &pre, &[sopk(0x0f, 20, imm)], &post, &[sum, 1]);
    }
    let mode_low8 = 1 | (7 << 11);
    for value in [1, 0xff, 0x55] {
        for src in [125, 20] {
            let mut pre = pr();
            pre.sv(125, value).sv(20, value);
            let mut post = pr();
            post.raw(&[sopk(0x12, 21, mode_low8)]).s2v(0, 21).s2v(1, 125);
            cases.add("s_setreg_b32", &pre, &[sopk(0x13, src, mode_low8)], &post, &[if src == 125 { 0 } else { value }, 0]);
        }
    }
    cases.run("scalar_null_sopk_reads_do_not_reuse_discarded_values");
}

fn s2(c: &mut Cases, m: &'static str, op: u32, a: u32, b: u32, cin: u32, res: u32, scc: u32) {
    let mut pre = pr();
    pre.sv(10, a).sv(12, b).raw(&[if cin != 0 { SCC1 } else { SCC0 }]);
    let mut post = pr();
    post.s2v(0, 20).scc_to_v(1);
    c.add(m, &pre, &[sop2(op, 20, 10, 12)], &post, &[res, scc]);
}

fn s2w(c: &mut Cases, m: &'static str, op: u32, a: u64, b: u64, cin: u32, res: u64, scc: u32) {
    let mut pre = pr();
    pre.sv(10, a as u32).sv(11, (a >> 32) as u32).sv(12, b as u32).sv(13, (b >> 32) as u32).raw(&[if cin != 0 { SCC1 } else { SCC0 }]);
    let mut post = pr();
    post.s2v(0, 20).s2v(1, 21).scc_to_v(2);
    c.add(m, &pre, &[sop2(op, 20, 10, 12)], &post, &[res as u32, (res >> 32) as u32, scc]);
}

fn s1(c: &mut Cases, m: &'static str, op: u32, a: u32, d0: u32, cin: u32, res: u32, scc: u32) {
    let mut pre = pr();
    pre.sv(10, a).sv(20, d0).raw(&[if cin != 0 { SCC1 } else { SCC0 }]);
    let mut post = pr();
    post.s2v(0, 20).scc_to_v(1);
    c.add(m, &pre, &[sop1(op, 20, 10)], &post, &[res, scc]);
}

fn s1w(c: &mut Cases, m: &'static str, op: u32, a: u64, d0: u64, cin: u32, res: u64, scc: u32) {
    let mut pre = pr();
    pre.sv(10, a as u32).sv(11, (a >> 32) as u32).sv(20, d0 as u32).sv(21, (d0 >> 32) as u32).raw(&[if cin != 0 { SCC1 } else { SCC0 }]);
    let mut post = pr();
    post.s2v(0, 20).s2v(1, 21).scc_to_v(2);
    c.add(m, &pre, &[sop1(op, 20, 10)], &post, &[res as u32, (res >> 32) as u32, scc]);
}

fn s1n(c: &mut Cases, m: &'static str, op: u32, a: u64, cin: u32, res: u32, scc: u32) {
    let mut pre = pr();
    pre.sv(10, a as u32).sv(11, (a >> 32) as u32).sv(20, 0x5555_5555).raw(&[if cin != 0 { SCC1 } else { SCC0 }]);
    let mut post = pr();
    post.s2v(0, 20).scc_to_v(1);
    c.add(m, &pre, &[sop1(op, 20, 10)], &post, &[res, scc]);
}

fn sk(c: &mut Cases, m: &'static str, op: u32, d0: u32, imm: u16, cin: u32, res: u32, scc: u32) {
    let mut pre = pr();
    pre.sv(20, d0).raw(&[if cin != 0 { SCC1 } else { SCC0 }]);
    let mut post = pr();
    post.s2v(0, 20).scc_to_v(1);
    c.add(m, &pre, &[sopk(op, 20, imm)], &post, &[res, scc]);
}

fn scmp(c: &mut Cases, m: &'static str, op: u32, a: u64, b: u64, scc: u32) {
    let mut pre = pr();
    pre.sv(10, a as u32).sv(11, (a >> 32) as u32).sv(12, b as u32).sv(13, (b >> 32) as u32).raw(&[if scc != 0 { SCC0 } else { SCC1 }]);
    let mut post = pr();
    post.scc_to_v(0);
    c.add(m, &pre, &[sopc(op, 10, 12)], &post, &[scc]);
}

fn nz(x: u64) -> u32 {
    (x != 0) as u32
}

fn bfe64(a: u64, b: u32, signed: bool) -> u64 {
    let o = b & 63;
    let w = (b >> 16) & 127;
    if w == 0 {
        return 0;
    }
    let x = if signed { ((a as i64) >> o) as u64 } else { a >> o };
    if w >= 64 {
        return x;
    }
    let v = x & ((1u64 << w) - 1);
    if signed && (v >> (w - 1)) & 1 != 0 { v | !((1u64 << w) - 1) } else { v }
}

fn wqm(x: u32) -> u32 {
    (0..8).fold(0, |acc, i| if (x >> (i * 4)) & 0xf != 0 { acc | (0xf << (i * 4)) } else { acc })
}

fn flbit_i32(x: u32) -> u32 {
    if x == 0 || x == u32::MAX {
        return u32::MAX;
    }
    let y = if (x as i32) < 0 { !x } else { x };
    y.leading_zeros()
}

fn flbit_i64(x: u64) -> u32 {
    if x == 0 || x == u64::MAX {
        return u32::MAX;
    }
    let y = if (x as i64) < 0 { !x } else { x };
    y.leading_zeros()
}

fn ff(x: u64, bits: u32) -> u32 {
    if bits == 32 && x as u32 == 0 || x == 0 { u32::MAX } else { x.trailing_zeros() }
}

#[test]
fn scalar_sop2() {
    let mut c = Cases::new();
    let pairs32: [(u32, u32); 5] = [(5, 3), (3, 5), (0, 0), (0x8000_0000, 1), (0xffff_ffff, 0xffff_ffff)];
    for &(a, b) in &pairs32 {
        s2(&mut c, "s_sub_u32", 0x01, a, b, 1, a.wrapping_sub(b), (b > a) as u32);
        for cin in 0..2u32 {
            let full = (a as u64).wrapping_sub(b as u64).wrapping_sub(cin as u64);
            s2(&mut c, "s_subb_u32", 0x05, a, b, cin, full as u32, ((b as u64 + cin as u64) > a as u64) as u32);
        }
        s2(&mut c, "s_max_u32", 0x09, a, b, 0, a.max(b), (a > b) as u32);
        s2(&mut c, "s_min_u32", 0x07, a, b, 0, a.min(b), (a < b) as u32);
        s2(&mut c, "s_min_i32", 0x06, a, b, 0, (a as i32).min(b as i32) as u32, ((a as i32) < (b as i32)) as u32);
        s2(&mut c, "s_max_i32", 0x08, a, b, 0, (a as i32).max(b as i32) as u32, ((a as i32) > (b as i32)) as u32);
        s2(&mut c, "s_add_i32", 0x02, a, b, 0, a.wrapping_add(b), (a as i32).checked_add(b as i32).is_none() as u32);
        s2(&mut c, "s_sub_i32", 0x03, a, b, 0, a.wrapping_sub(b), (a as i32).checked_sub(b as i32).is_none() as u32);
        s2(&mut c, "s_andn2_b32", 0x14, a, b, 0, a & !b, nz((a & !b) as u64));
        s2(&mut c, "s_orn2_b32", 0x16, a, b, 0, a | !b, nz((a | !b) as u64));
        s2(&mut c, "s_nand_b32", 0x18, a, b, 0, !(a & b), nz(!(a & b) as u64));
        s2(&mut c, "s_nor_b32", 0x1a, a, b, 0, !(a | b), nz(!(a | b) as u64));
        s2(&mut c, "s_xnor_b32", 0x1c, a, b, 0, !(a ^ b), nz(!(a ^ b) as u64));
        let ad = (a as i32).wrapping_sub(b as i32).wrapping_abs() as u32;
        s2(&mut c, "s_absdiff_i32", 0x2c, a, b, 0, ad, nz(ad as u64));
        for (n, op, m) in [(1u32, 0x2e, "s_lshl1_add_u32"), (2, 0x2f, "s_lshl2_add_u32"), (3, 0x30, "s_lshl3_add_u32"), (4, 0x31, "s_lshl4_add_u32")] {
            let full = ((a as u64) << n) + b as u64;
            s2(&mut c, m, op, a, b, 0, full as u32, (full >= 1 << 32) as u32);
        }
        s2(&mut c, "s_pack_ll_b32_b16", 0x32, a ^ 0x1234_5678, b ^ 0x9abc_def0, 1, ((a ^ 0x1234_5678) & 0xffff) | ((b ^ 0x9abc_def0) << 16), 1);
        s2(&mut c, "s_pack_lh_b32_b16", 0x33, a ^ 0x1234_5678, b ^ 0x9abc_def0, 1, ((a ^ 0x1234_5678) & 0xffff) | ((b ^ 0x9abc_def0) & 0xffff_0000), 1);
        s2(&mut c, "s_pack_hh_b32_b16", 0x34, a ^ 0x1234_5678, b ^ 0x9abc_def0, 1, ((a ^ 0x1234_5678) >> 16) | ((b ^ 0x9abc_def0) & 0xffff_0000), 1);
        s2(&mut c, "s_mul_hi_u32", 0x35, a ^ 0x7654_3210, b ^ 0x0fed_cba9, 1, (((a ^ 0x7654_3210) as u64 * (b ^ 0x0fed_cba9) as u64) >> 32) as u32, 1);
        let (x, y) = ((a ^ 0x7654_3210) as i32 as i64, (b ^ 0x8fed_cba9) as i32 as i64);
        s2(&mut c, "s_mul_hi_i32", 0x36, x as u32, y as u32, 0, ((x * y) >> 32) as u32, 0);
    }
    for &(a, b) in &[(3u32, 4u32), (0, 5), (31, 1), (5, 30), (37, 33)] {
        let w = a & 31;
        let r = if w == 0 { 0 } else { (((1u64 << w) - 1) as u32) << (b & 31) };
        s2(&mut c, "s_bfm_b32", 0x24, a, b, 1, r, 1);
    }
    let pairs64: [(u64, u64); 4] = [(0x1234_5678_9abc_def0, 0x0f0f_0f0f_f0f0_f0f0), (0, 0), (u64::MAX, 0), (0x8000_0000_0000_0001, 0xffff_ffff_0000_0000)];
    for &(a, b) in &pairs64 {
        for (m, op, f) in [
            ("s_orn2_b64", 0x17u32, (|a: u64, b: u64| a | !b) as fn(u64, u64) -> u64),
            ("s_nand_b64", 0x19, |a, b| !(a & b)),
            ("s_nor_b64", 0x1b, |a, b| !(a | b)),
            ("s_xnor_b64", 0x1d, |a, b| !(a ^ b)),
        ] {
            let r = f(a, b);
            s2w(&mut c, m, op, a, b, 0, r, nz(r));
        }
    }
    for &(a, s) in &[(0x8123_4567_89ab_cdefu64, 0u64), (0x8123_4567_89ab_cdef, 4), (0x8123_4567_89ab_cdef, 32), (0x8123_4567_89ab_cdef, 63), (0x8123_4567_89ab_cdef, 0x44), (0x0000_0001_0000_0000, 33)] {
        let r = a >> (s & 63);
        s2w(&mut c, "s_lshr_b64", 0x21, a, s, 1, r, nz(r));
        let r = ((a as i64) >> (s & 63)) as u64;
        s2w(&mut c, "s_ashr_i64", 0x23, a, s, 1, r, nz(r));
    }
    for &(a, b) in &[(0x8123_4567_89ab_cdefu64, 0x0008_0004u32), (0x8123_4567_89ab_cdef, 0x0020_0010), (0x8123_4567_89ab_cdef, 0x0028_001c), (0x8123_4567_89ab_cdef, 0x0004_003c), (0x8123_4567_89ab_cdef, 0x0000_0008), (0x8123_4567_89ab_cdef, 0x003f_0001), (0x0000_0000_f000_0000, 0x0004_001c)] {
        let r = bfe64(a, b, false);
        s2w(&mut c, "s_bfe_u64", 0x29, a, b as u64, 0, r, nz(r));
        let r = bfe64(a, b, true);
        s2w(&mut c, "s_bfe_i64", 0x2a, a, b as u64, 0, r, nz(r));
    }
    c.run_both("sop2");
}

#[test]
fn scalar_bfe32_result_and_scc() {
    let mut cases = Cases::new();
    for (offset, width, unsigned, signed) in [
        (16, 0, 0, 0),
        (31, 1, 1, u32::MAX),
        (28, 8, 8, 0xffff_fff8),
        (16, 32, 0x8123, 0xffff_8123),
        (0, 32, 0x8123_4567, 0x8123_4567),
    ] {
        let control = (width << 16) | offset;
        s2(&mut cases, "s_bfe_u32", 0x27, 0x8123_4567, control, 1, unsigned, nz(u64::from(unsigned)));
        s2(&mut cases, "s_bfe_i32", 0x28, 0x8123_4567, control, 1, signed, nz(u64::from(signed)));
    }
    cases.run_both("s_bfe32_result_and_scc");
}

#[test]
fn scalar_sop1() {
    let mut c = Cases::new();
    let vals32: [u32; 7] = [0, 1, 0x8000_0000, 0xffff_ffff, 0x0001_0000, 0x7fff_fff0, 0x1234_0080];
    for &a in &vals32 {
        for cin in 0..2u32 {
            s1(&mut c, "s_cmov_b32", 0x05, a, 0xabcd_ef01, cin, if cin != 0 { a } else { 0xabcd_ef01 }, cin);
        }
        s1(&mut c, "s_not_b32", 0x07, a, 0, 1, !a, nz(!a as u64));
        s1(&mut c, "s_wqm_b32", 0x09, a, 0, 1, wqm(a), nz(wqm(a) as u64));
        s1(&mut c, "s_bcnt0_i32_b32", 0x0d, a, 0, 1, a.count_zeros(), nz(a.count_zeros() as u64));
        s1(&mut c, "s_bcnt1_i32_b32", 0x0f, a, 0, 1, a.count_ones(), nz(a.count_ones() as u64));
        s1(&mut c, "s_ff0_i32_b32", 0x11, a, 0, 1, ff(!a as u64, 32), 1);
        s1(&mut c, "s_ff1_i32_b32", 0x13, a, 0, 0, ff(a as u64, 32), 0);
        s1(&mut c, "s_flbit_i32_b32", 0x15, a, 0, 1, if a == 0 { u32::MAX } else { a.leading_zeros() }, 1);
        s1(&mut c, "s_flbit_i32", 0x17, a, 0, 0, flbit_i32(a), 0);
        s1(&mut c, "s_sext_i32_i8", 0x19, a | 0x80, 0, 1, (a as u8 | 0x80) as i8 as i32 as u32, 1);
        s1(&mut c, "s_sext_i32_i8", 0x19, a & !0x80, 0, 1, (a as u8 & 0x7f) as i8 as i32 as u32, 1);
        s1(&mut c, "s_sext_i32_i16", 0x1a, a, 0, 1, a as u16 as i16 as i32 as u32, 1);
        let ab = (a as i32).wrapping_abs() as u32;
        s1(&mut c, "s_abs_i32", 0x34, a, 0, 0, ab, nz(ab as u64));
    }
    for &(b, d0) in &[(0u32, 0u32), (5, 0x10), (31, 0), (37, 0xffff_ffff)] {
        s1(&mut c, "s_bitset1_b32", 0x1d, b, d0, 1, d0 | (1 << (b & 31)), 1);
    }
    let vals64: [u64; 6] = [0, 1, 0x8000_0000_0000_0000, u64::MAX, 0x0000_0001_0000_0000, 0xffff_ffff_0000_0000];
    for &a in &vals64 {
        for cin in 0..2u32 {
            s1w(&mut c, "s_cmov_b64", 0x06, a, 0x1111_2222_3333_4444, cin, if cin != 0 { a } else { 0x1111_2222_3333_4444 }, cin);
        }
        s1w(&mut c, "s_not_b64", 0x08, a, 0, 1, !a, nz(!a));
        s1w(&mut c, "s_brev_b64", 0x0c, a ^ 0x0123_4567_89ab_cdef, 0, 1, (a ^ 0x0123_4567_89ab_cdef).reverse_bits(), 1);
        s1n(&mut c, "s_bcnt0_i32_b64", 0x0e, a, 1, a.count_zeros(), nz(a.count_zeros() as u64));
        s1n(&mut c, "s_ff0_i32_b64", 0x12, a, 1, ff(!a, 64), 1);
        s1n(&mut c, "s_ff1_i32_b64", 0x14, a, 0, ff(a, 64), 0);
        s1n(&mut c, "s_flbit_i32_b64", 0x16, a, 1, if a == 0 { u32::MAX } else { a.leading_zeros() }, 1);
        s1n(&mut c, "s_flbit_i32_i64", 0x18, a, 1, flbit_i64(a), 1);
        s1n(&mut c, "s_flbit_i32_i64", 0x18, a ^ 0x0000_0000_8000_0000, 0, flbit_i64(a ^ 0x0000_0000_8000_0000), 0);
    }
    for &(b, d0) in &[(0u32, 0u64), (5, 0x10), (31, 0), (32, 0), (63, u64::MAX), (0x45, 0)] {
        s1w(&mut c, "s_bitset0_b64", 0x1c, b as u64, d0 | (1u64 << (b & 63)), 1, d0 & !(1u64 << (b & 63)), 1);
        s1w(&mut c, "s_bitset1_b64", 0x1e, b as u64, d0, 0, d0 | (1u64 << (b & 63)), 0);
    }
    c.run_both("sop1");
}

#[test]
fn scalar_quadmask() {
    let mut cases = Cases::new();
    for (mask, expected) in [(0, 0), (8, 1), (0x8000_0000, 0x80), (0x1020_3040, 0xaa), (u32::MAX, 0xff)] {
        let scc = nz(u64::from(expected));
        s1(&mut cases, "s_quadmask_b32", 0x2c, mask, u32::MAX, scc ^ 1, expected, scc);
    }

    for (mask, expected) in [
        (0, 0),
        (8, 1),
        (0x0000_0008_0000_0000, 0x0100),
        (0x8000_0000_0000_0000, 0x8000),
        (0x1000_0001_0000_0100, 0x8104),
        (u64::MAX, 0xffff),
    ] {
        let scc = nz(expected);
        s1w(&mut cases, "s_quadmask_b64", 0x2d, mask, u64::MAX, scc ^ 1, expected, scc);
    }

    let mut pre = pr();
    pre.sv(10, 0x0000_0100).sv(11, 0x1000_0001);
    let mut post = pr();
    post.s2v(0, 10).s2v(1, 11).scc_to_v(2);
    cases.add("s_quadmask_b64", &pre, &[sop1(0x2d, 10, 10)], &post, &[0x8104, 0, 1]);
    cases.run_both("scalar_quadmask");
}

fn saveexec(c: &mut Cases, m: &'static str, op: u32, a: u32, f: fn(u32, u32) -> u32) {
    let exec0 = 1u32;
    let newx = f(a, exec0) & 1;
    let mut pre = pr();
    pre.sv(10, a).sv(11, 0xffff_ffff).sv(20, 0xdead_0000).sv(21, 0xdead_0001).raw(&[SCC0]);
    let mut post = pr();
    post.raw(&[sop1(3, 30, 126), sop1(3, 31, 127)]).scc_to_v(1).raw(&[sop1(3, 126, ic(1)), sop1(3, 127, ic(0))]);
    post.s2v(0, 20).s2v(2, 30);
    let wide = m.ends_with("_b64");
    if wide {
        post.s2v(3, 21);
    }
    let mut exp = vec![exec0, newx, newx];
    if wide {
        exp.push(0);
    }
    c.add(m, &pre, &[sop1(op, 20, 10)], &post, &exp);
}

fn wrexec(c: &mut Cases, m: &'static str, op: u32, a: u32, f: fn(u32, u32) -> u32) {
    let exec0 = 1u32;
    let newx = f(a, exec0) & 1;
    let mut pre = pr();
    pre.sv(10, a).sv(20, 0xdead_0000).sv(21, 0xdead_0001).raw(&[if newx == 1 { SCC0 } else { SCC1 }]);
    let mut post = pr();
    post.raw(&[sop1(3, 30, 126), sop1(3, 31, 127)]).scc_to_v(1).raw(&[sop1(3, 126, ic(1)), sop1(3, 127, ic(0))]);
    post.s2v(0, 20).s2v(2, 30);
    let wide = m.ends_with("_b64");
    if wide {
        post.s2v(3, 21);
    }
    let mut exp = vec![newx, newx, newx];
    if wide {
        exp.push(0);
    }
    c.add(m, &pre, &[sop1(op, 20, 10)], &post, &exp);
}

#[test]
fn wqm_of_exec_keeps_only_the_lane_bit_so_mask_loops_end() {
    let mut c = Cases::new();
    let mut post = pr();
    post.raw(&[sop1(4, 30, 126)]).s2v(0, 30).s2v(1, 31);
    c.add("s_wqm_b64", &pr(), &[sop1(0x0a, 126, 126)], &post, &[1, 0]);
    let mut pre = pr();
    pre.raw(&[sop1(4, 126, ic(-1)), sop1(4, 30, 126)]).sv(20, 0);
    let mut post = pr();
    post.raw(&[sop1(3, 126, ic(1)), sop1(3, 127, ic(0))]).s2v(0, 20).s2v(1, 30).s2v(2, 31);
    c.add("s_add_u32", &pre, &[sop2(0, 20, 20, ic(1)), sop2(0x15, 30, 30, 126), sopp(0x05, 0xfffd)], &post, &[1, 0, 0]);
    let mut pre = pr();
    pre.raw(&[sop1(0x0a, 126, 126), sop1(4, 30, 126)]).sv(20, 0).sv(106, 1).sv(107, 0);
    let mut post = pr();
    post.s2v(0, 20).s2v(1, 30);
    c.add("s_add_u32", &pre, &[sop2(0, 20, 20, ic(1)), sop1(0x24, 4, 106), sop2(0x15, 30, 30, 126), sop1(4, 126, 4), sopp(0x05, 0xfffb)], &post, &[1, 0]);
    c.run_both("exec_lane_bit");
}

#[test]
fn scalar_saveexec_lane_model() {
    let mut c = Cases::new();
    let ops: [(&'static str, u32, fn(u32, u32) -> u32); 18] = [
        ("s_and_saveexec_b32", 0x3c, |a, e| a & e),
        ("s_or_saveexec_b32", 0x3d, |a, e| a | e),
        ("s_xor_saveexec_b32", 0x3e, |a, e| a ^ e),
        ("s_andn2_saveexec_b32", 0x3f, |a, e| a & !e),
        ("s_orn2_saveexec_b32", 0x40, |a, e| a | !e),
        ("s_nand_saveexec_b32", 0x41, |a, e| !(a & e)),
        ("s_nor_saveexec_b32", 0x42, |a, e| !(a | e)),
        ("s_xnor_saveexec_b32", 0x43, |a, e| !(a ^ e)),
        ("s_andn1_saveexec_b32", 0x44, |a, e| !a & e),
        ("s_orn1_saveexec_b32", 0x45, |a, e| !a | e),
        ("s_or_saveexec_b64", 0x25, |a, e| a | e),
        ("s_xor_saveexec_b64", 0x26, |a, e| a ^ e),
        ("s_andn2_saveexec_b64", 0x27, |a, e| a & !e),
        ("s_orn2_saveexec_b64", 0x28, |a, e| a | !e),
        ("s_nor_saveexec_b64", 0x2a, |a, e| !(a | e)),
        ("s_xnor_saveexec_b64", 0x2b, |a, e| !(a ^ e)),
        ("s_andn1_saveexec_b64", 0x37, |a, e| !a & e),
        ("s_orn1_saveexec_b64", 0x38, |a, e| !a | e),
    ];
    for (m, op, f) in ops {
        for a in [0u32, 1, 0xffff_fffe, 0xffff_ffff] {
            saveexec(&mut c, m, op, a, f);
        }
    }
    c.run_both("saveexec");
}

#[test]
fn scalar_wrexec_lane_model() {
    let mut c = Cases::new();
    let ops: [(&'static str, u32, fn(u32, u32) -> u32); 4] = [
        ("s_andn1_wrexec_b32", 0x46, |a, e| !a & e),
        ("s_andn2_wrexec_b32", 0x47, |a, e| a & !e),
        ("s_andn1_wrexec_b64", 0x39, |a, e| !a & e),
        ("s_andn2_wrexec_b64", 0x3a, |a, e| a & !e),
    ];
    for (m, op, f) in ops {
        for a in [0u32, 1, 0xffff_fffe, 0xffff_ffff] {
            wrexec(&mut c, m, op, a, f);
        }
    }
    c.run_both("wrexec");
}

#[test]
fn scalar_sopk_sopc() {
    let mut c = Cases::new();
    for &d0 in &[0u32, 5, 0xffff_fff0, 0x8000_0000, 0x7fff, 0xffff_8000, 0x0000_8000] {
        for &imm in &[0u16, 5, 0xfff0, 0x8000, 0x7fff] {
            let si = imm as i16 as i32;
            let ui = imm as u32;
            let s = d0 as i32;
            sk(&mut c, "s_cmpk_lg_i32", 0x04, d0, imm, (s == si) as u32, d0, (s != si) as u32);
            sk(&mut c, "s_cmpk_gt_i32", 0x05, d0, imm, 0, d0, (s > si) as u32);
            sk(&mut c, "s_cmpk_ge_i32", 0x06, d0, imm, 0, d0, (s >= si) as u32);
            sk(&mut c, "s_cmpk_lt_i32", 0x07, d0, imm, 0, d0, (s < si) as u32);
            sk(&mut c, "s_cmpk_le_i32", 0x08, d0, imm, 0, d0, (s <= si) as u32);
            sk(&mut c, "s_cmpk_eq_u32", 0x09, d0, imm, 0, d0, (d0 == ui) as u32);
            sk(&mut c, "s_cmpk_lg_u32", 0x0a, d0, imm, 0, d0, (d0 != ui) as u32);
            sk(&mut c, "s_cmpk_ge_u32", 0x0c, d0, imm, 0, d0, (d0 >= ui) as u32);
            sk(&mut c, "s_cmpk_lt_u32", 0x0d, d0, imm, 0, d0, (d0 < ui) as u32);
            sk(&mut c, "s_cmpk_le_u32", 0x0e, d0, imm, 0, d0, (d0 <= ui) as u32);
            sk(&mut c, "s_mulk_i32", 0x10, d0, imm, 1, (s.wrapping_mul(si)) as u32, 1);
        }
    }
    for &imm in &[0u16, 0x7fff, 0x8000, 0xffff] {
        for cin in 0..2u32 {
            sk(&mut c, "s_cmovk_i32", 0x02, 0x1234_5678, imm, cin, if cin != 0 { imm as i16 as i32 as u32 } else { 0x1234_5678 }, cin);
        }
        sk(&mut c, "s_version", 0x01, 0x1234_5678, imm, 1, 0x1234_5678, 1);
        sk(&mut c, "s_waitcnt_vscnt", 0x17, 0x0, imm, 1, 0, 1);
        sk(&mut c, "s_waitcnt_vmcnt", 0x18, 0x0, imm, 1, 0, 1);
        sk(&mut c, "s_waitcnt_expcnt", 0x19, 0x0, imm, 0, 0, 0);
        sk(&mut c, "s_waitcnt_lgkmcnt", 0x1a, 0x0, imm, 1, 0, 1);
    }
    let v32: [u32; 5] = [0, 1, 0x8000_0000, 0xffff_ffff, 0x7fff_ffff];
    for &a in &v32 {
        for &b in &v32 {
            scmp(&mut c, "s_cmp_eq_i32", 0x00, a as u64, b as u64, (a == b) as u32);
            scmp(&mut c, "s_cmp_lg_i32", 0x01, a as u64, b as u64, (a != b) as u32);
            scmp(&mut c, "s_cmp_le_i32", 0x05, a as u64, b as u64, ((a as i32) <= (b as i32)) as u32);
        }
    }
    for &(a, b) in &[(0x8000_0001u32, 0u32), (0x8000_0001, 31), (0x8000_0001, 1), (0x8000_0001, 32), (0x8000_0001, 63)] {
        scmp(&mut c, "s_bitcmp0_b32", 0x0c, a as u64, b as u64, ((a >> (b & 31)) & 1 == 0) as u32);
        scmp(&mut c, "s_bitcmp1_b32", 0x0d, a as u64, b as u64, ((a >> (b & 31)) & 1 == 1) as u32);
    }
    for &(a, b) in &[(0x8000_0001_8000_0001u64, 0u64), (0x8000_0001_8000_0001, 31), (0x8000_0001_8000_0001, 32), (0x8000_0001_8000_0001, 33), (0x8000_0001_8000_0001, 63), (0x8000_0001_8000_0001, 64)] {
        scmp(&mut c, "s_bitcmp0_b64", 0x0e, a, b, ((a >> (b & 63)) & 1 == 0) as u32);
        scmp(&mut c, "s_bitcmp1_b64", 0x0f, a, b, ((a >> (b & 63)) & 1 == 1) as u32);
    }
    let v64: [u64; 4] = [0, 1, 0x0000_0001_0000_0000, u64::MAX];
    for &a in &v64 {
        for &b in &v64 {
            scmp(&mut c, "s_cmp_eq_u64", 0x12, a, b, (a == b) as u32);
            scmp(&mut c, "s_cmp_lg_u64", 0x13, a, b, (a != b) as u32);
        }
    }
    c.run_both("sopk_sopc");
}

fn fb(x: f32) -> u32 {
    x.to_bits()
}

fn h2f(h: u32) -> f32 {
    let (s, e, m) = ((h >> 15) & 1, (h >> 10) & 31, h & 0x3ff);
    let v = if e == 0 { m as f32 * 2f32.powi(-24) } else if e == 31 { if m == 0 { f32::INFINITY } else { f32::NAN } } else { (1.0 + m as f32 / 1024.0) * 2f32.powi(e as i32 - 15) };
    if s != 0 { -v } else { v }
}

fn f2h(x: f32) -> u32 {
    let b = x.to_bits();
    let s = (b >> 16) & 0x8000;
    let a = x.abs();
    if a.is_nan() {
        return s | 0x7e00;
    }
    if a >= 65520.0 {
        return s | 0x7c00;
    }
    let best;
    let mut lo = 0u32;
    let mut hi = 0x7c00u32;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if h2f(mid) < a { lo = mid + 1 } else { hi = mid }
    }
    let up = lo;
    if up == 0 {
        return s;
    }
    let dn = up - 1;
    let (du, dd) = (h2f(up) - a, a - h2f(dn));
    best = if du < dd || (du == dd && up & 1 == 0) { up } else { dn };
    s | best
}

const D16: u32 = 0xabcd_0000;

fn vc(c: &mut Cases, m: &'static str, ins: &[(u32, u32)], op: &[u32], outs: &[u32], exp: &[u32]) {
    let mut pre = pr();
    pre.vv(10, D16).vv(11, 0x5a5a_5a5a);
    for &(r, val) in ins {
        if r >= 1000 { pre.sv(r - 1000, val) } else { pre.vv(r, val) };
    }
    let mut post = pr();
    for (k, &o) in outs.iter().enumerate() {
        if o >= 1000 { post.s2v(k as u32, o - 1000) } else { post.raw(&[vop1(1, k as u32, v(o))]) };
    }
    c.add(m, &pre, op, &post, exp);
}

fn v1c(c: &mut Cases, m: &'static str, op: u32, a: u32, exp: u32) {
    vc(c, m, &[(1, a)], &[vop1(op, 10, v(1))], &[10], &[exp]);
}

fn v16(c: &mut Cases, m: &'static str, op: u32, a: u32, exp16: u32) {
    v1c(c, m, op, a, D16 | (exp16 & 0xffff));
}

fn v3c(c: &mut Cases, m: &'static str, op: u32, a: u32, b: u32, cc: u32, exp: u32) {
    vc(c, m, &[(1, a), (2, b), (3, cc)], &vop3(op, 10, [v(1), v(2), v(3)]), &[10], &[exp]);
}

#[test]
fn vector_vop2() {
    let mut c = Cases::new();
    for &(d, a, b, r) in &[(1.0f32, 2.0f32, 3.0f32, 7.0f32), (1.0, 0.0, f32::INFINITY, 1.0), (1.0, f32::NAN, 0.0, 1.0), (-2.0, 0.5, 4.0, 0.0)] {
        vc(&mut c, "v_mac_legacy_f32", &[(10, fb(d)), (1, fb(a)), (2, fb(b))], &[vop2(0x06, 10, v(1), 2)], &[10], &[fb(r)]);
    }
    let s24 = |x: u32| ((x << 8) as i32 >> 8) as i64;
    for &(a, b) in &[(0x00ff_ffffu32, 5u32), (0x1280_0000, 2), (0x0080_0000, 0x0080_0000), (0x0012_3456, 0x00ab_cdef), (0xff00_0007, 0x0000_0003)] {
        let p = s24(a) * s24(b);
        vc(&mut c, "v_mul_i32_i24", &[(1, a), (2, b)], &[vop2(0x09, 10, v(1), 2)], &[10], &[p as u32]);
        vc(&mut c, "v_mul_hi_i32_i24", &[(1, a), (2, b)], &[vop2(0x0a, 10, v(1), 2)], &[10], &[(p >> 32) as u32]);
        let pu = (a as u64 & 0xff_ffff) * (b as u64 & 0xff_ffff);
        vc(&mut c, "v_mul_hi_u32_u24", &[(1, a), (2, b)], &[vop2(0x0c, 10, v(1), 2)], &[10], &[(pu >> 32) as u32]);
        vc(&mut c, "v_xnor_b32", &[(1, a), (2, b)], &[vop2(0x1e, 10, v(1), 2)], &[10], &[!(a ^ b)]);
    }
    for &(a, k, b) in &[(2.0f32, 3.0f32, 1.0f32), (-1.5, 4.0, 0.25), (0.0, 1e30, -7.0)] {
        vc(&mut c, "v_madmk_f32", &[(1, fb(a)), (2, fb(b))], &[vop2(0x20, 10, v(1), 2), fb(k)], &[10], &[fb(a * k + b)]);
    }
    for &(a, b, cin) in &[(5u32, 3u32, 1u32), (3, 5, 0), (0, 0, 1), (5, 5, 0), (0xffff_ffff, 0xffff_ffff, 1), (0x8000_0000, 0x7fff_ffff, 1)] {
        let full = (a as u64).wrapping_sub(b as u64).wrapping_sub(cin as u64);
        let borrow = ((b as u64 + cin as u64) > a as u64) as u32;
        let mut pre = pr();
        pre.vv(1, a).vv(2, b).sv(106, cin).sv(107, 0);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10))]).vcc_to_v(1);
        c.add("v_sub_co_ci_u32", &pre, &[vop2(0x29, 10, v(1), 2)], &post, &[full as u32, borrow]);
    }
    for &(a, b) in &[(3.0f32, 1.0f32), (0.5, 0.25), (-2.0, 6.0), (65504.0, -65504.0)] {
        vc(&mut c, "v_subrev_f16", &[(1, 0x1234_0000 | f2h(a)), (2, 0x9876_0000 | f2h(b))], &[vop2(0x34, 10, v(1), 2)], &[10], &[D16 | f2h(b - a)]);
    }
    for &(a, k, b) in &[(2.0f32, 3.0f32, 1.0f32), (0.5, -4.0, 0.25), (100.0, 1000.0, 0.0)] {
        vc(&mut c, "v_fmamk_f16", &[(1, 0x1111_0000 | f2h(a)), (2, f2h(b))], &[vop2(0x37, 10, v(1), 2), f2h(k)], &[10], &[D16 | f2h(a * k + b)]);
    }
    let (p3, m1) = (0x3c03u32, f2h(-1.0));
    let fused = D16 | f2h(h2f(p3) * h2f(p3) - 1.0);
    vc(&mut c, "v_fmac_f16", &[(10, D16 | m1), (1, p3), (2, p3)], &[vop2(0x36, 10, v(1), 2)], &[10], &[fused]);
    vc(&mut c, "v_fmac_f16", &[(10, D16 | m1), (1, p3), (2, p3)], &vop3(0x136, 10, [v(1), v(2), 0]), &[10], &[fused]);
    vc(&mut c, "v_fmamk_f16", &[(1, p3), (2, m1)], &[vop2(0x37, 10, v(1), 2), p3], &[10], &[fused]);
    vc(&mut c, "v_fmaak_f16", &[(1, p3), (2, p3)], &[vop2(0x38, 10, v(1), 2), m1], &[10], &[fused]);
    for &(a, e, r) in &[(1.5f32, 3i32, 12.0f32), (1.0, -1, 0.5), (1.0, 100, f32::INFINITY), (1.0, -100, 0.0), (-3.0, 0x1_0002, -12.0)] {
        vc(&mut c, "v_ldexp_f16", &[(1, f2h(a)), (2, e as u32)], &[vop2(0x3b, 10, v(1), 2)], &[10], &[D16 | f2h(r)]);
    }
    vc(&mut c, "v_pk_fmac_f16", &[(10, 0x3c00_3c00), (1, 0x4000_4200), (2, 0x4400_3800)], &[vop2(0x3c, 10, v(1), 2)], &[10], &[0x4880_4100]);
    vc(&mut c, "v_pk_fmac_f16", &[(10, (f2h(-1.0) << 16) | f2h(0.25)), (1, (f2h(0.5) << 16) | f2h(-2.0)), (2, (f2h(8.0) << 16) | f2h(0.125))], &[vop2(0x3c, 10, v(1), 2)], &[10], &[(f2h(3.0) << 16) | f2h(0.0)]);
    c.run_both("vop2");
}

fn cvt_i_floor_nan(bits: u32, add_half: bool) -> u32 {
    let x = f32::from_bits(bits);
    if x.is_nan() {
        return if bits & 0x8000_0000 != 0 { 0x8000_0000 } else { 0x7FFF_FFFF };
    }
    let v = (f64::from(x) + if add_half { 0.5 } else { 0.0 }).floor();
    if v >= 2147483648.0 {
        0x7FFF_FFFF
    } else if v <= -2147483648.0 {
        0x8000_0000
    } else {
        v as i32 as u32
    }
}

#[test]
fn vector_vop1() {
    let mut c = Cases::new();
    for &x in &[2.5f32, -2.5, 1.49999, -0.5, 2.9, f32::NAN, 3e9, -3e9, f32::INFINITY, f32::NEG_INFINITY, 0.5, -0.0] {
        v1c(&mut c, "v_cvt_rpi_i32_f32", 0x0c, fb(x), cvt_i_floor_nan(fb(x), true));
        v1c(&mut c, "v_cvt_flr_i32_f32", 0x0d, fb(x), cvt_i_floor_nan(fb(x), false));
    }
    for &bits in &[
        0x3EFFFFFFu32,
        0x4B000001u32,
        0x4B000003u32,
        0xCB000001u32,
        0xBF000001u32,
        0x7FC00000u32,
        0xFFC00000u32,
        0x7F800001u32,
        0xFF800001u32,
    ] {
        v1c(&mut c, "v_cvt_rpi_i32_f32", 0x0c, bits, cvt_i_floor_nan(bits, true));
        v1c(&mut c, "v_cvt_flr_i32_f32", 0x0d, bits, cvt_i_floor_nan(bits, false));
    }
    for &nan_bits in &[0x7FC00000u32, 0xFFC00000u32, 0x7F800001u32, 0xFF800001u32] {
        v1c(&mut c, "v_cvt_i32_f32", 0x08, nan_bits, 0);
        v1c(&mut c, "v_cvt_u32_f32", 0x07, nan_bits, 0);
    }
    for &x in &[0x8u32, 0x7, 0xf3, 0x0, 0xf, 0x1234_5679] {
        v1c(&mut c, "v_cvt_off_f32_i4", 0x0e, x, fb((((x << 28) as i32) >> 28) as f32 / 16.0));
    }
    for &x in &[0u32, 1, 0x8000_0000, 0xffff_ffff, 0xffff_fffe, 0x4000_0000, 0x0001_0000, 6] {
        v1c(&mut c, "v_ffbh_u32", 0x39, x, if x == 0 { u32::MAX } else { x.leading_zeros() });
        v1c(&mut c, "v_ffbl_b32", 0x3a, x, if x == 0 { u32::MAX } else { x.trailing_zeros() });
        v1c(&mut c, "v_ffbh_i32", 0x3b, x, flbit_i32(x));
    }
    for &x in &[3u32, 65535, 2049, 0, 0x1234_0007] {
        v16(&mut c, "v_cvt_f16_u16", 0x50, x, f2h((x & 0xffff) as f32));
    }
    for &x in &[0xfffdu32, 0x7fff, 0x8000, 0x1234_0005] {
        v16(&mut c, "v_cvt_f16_i16", 0x51, x, f2h(x as u16 as i16 as f32));
    }
    let fs = [3.5f32, -1.0, 65504.0, 0.0, -3.5, 0.999, 40000.0, -40000.0];
    for &x in &fs {
        v16(&mut c, "v_cvt_u16_f16", 0x52, 0x7777_0000 | f2h(x), x.max(0.0).min(65535.0) as u32);
        v16(&mut c, "v_cvt_i16_f16", 0x53, 0x7777_0000 | f2h(x), (h2f(f2h(x)).max(-32768.0).min(32767.0) as i32 as u32) & 0xffff);
    }
    for (h, u, i) in [(0x7e00u32, 0u32, 0u32), (0x7c00, 0xffff, 0x7fff), (0xfc00, 0, 0x8000)] {
        v16(&mut c, "v_cvt_u16_f16", 0x52, h, u);
        v16(&mut c, "v_cvt_i16_f16", 0x53, h, i);
    }
    for &(x, sq, rs) in &[(4.0f32, 2.0f32, 0.5f32), (0.25, 0.5, 2.0), (16.0, 4.0, 0.25), (1.0, 1.0, 1.0)] {
        v16(&mut c, "v_sqrt_f16", 0x55, f2h(x), f2h(sq));
        v16(&mut c, "v_rsq_f16", 0x56, f2h(x), f2h(rs));
    }
    for &x in &[2.5f32, -2.5, 3.5, -0.5, 0.5, 1.75, -1.25, 1000.5] {
        v16(&mut c, "v_trunc_f16", 0x5d, f2h(x), f2h(x.trunc()));
        let rn = { let r = x.round(); if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 { r - x.signum() } else { r } }.copysign(x);
        v16(&mut c, "v_rndne_f16", 0x5e, f2h(x), f2h(rn));
        v16(&mut c, "v_fract_f16", 0x5f, f2h(x), f2h(x - x.floor()));
    }
    for &(x, l) in &[(8.0f32, 3.0f32), (1.0, 0.0), (0.25, -2.0), (1024.0, 10.0)] {
        v16(&mut c, "v_log_f16", 0x57, f2h(x), f2h(l));
        v16(&mut c, "v_exp_f16", 0x58, f2h(l), f2h(x));
    }
    for &(x, m, e) in &[(12.0f32, 0.75f32, 4u32), (-3.0, -0.75, 2), (0.5, 0.5, 0), (1.0, 0.5, 1), (0.0, 0.0, 0)] {
        v16(&mut c, "v_frexp_mant_f16", 0x59, f2h(x), f2h(m));
        v16(&mut c, "v_frexp_exp_i16_f16", 0x5a, f2h(x), e);
        v1c(&mut c, "v_frexp_mant_f32", 0x40, fb(x), fb(m));
        v1c(&mut c, "v_frexp_exp_i32_f32", 0x3f, fb(x), e);
    }
    v16(&mut c, "v_frexp_mant_f16", 0x59, 0x0001, 0x3800);
    v16(&mut c, "v_frexp_exp_i16_f16", 0x5a, 0x0001, (-23i32) as u32 & 0xffff);
    v16(&mut c, "v_frexp_exp_i16_f16", 0x5a, 0x7c00, 0);
    v16(&mut c, "v_frexp_mant_f16", 0x59, 0x7c00, 0x7c00);
    v1c(&mut c, "v_frexp_mant_f32", 0x40, 1, 0x3f00_0000);
    v1c(&mut c, "v_frexp_exp_i32_f32", 0x3f, 1, (-148i32) as u32);
    v1c(&mut c, "v_frexp_exp_i32_f32", 0x3f, fb(f32::INFINITY), 0);
    v1c(&mut c, "v_frexp_mant_f32", 0x40, fb(f32::NEG_INFINITY), fb(f32::NEG_INFINITY));
    for &(x, s) in &[(0.25f32, 1.0f32), (0.0, 0.0), (-0.25, -1.0)] {
        v16(&mut c, "v_sin_f16", 0x60, f2h(x), f2h(s));
    }
    for &(x, co) in &[(0.0f32, 1.0f32), (0.5, -1.0), (-0.5, -1.0)] {
        v16(&mut c, "v_cos_f16", 0x61, f2h(x), f2h(co));
    }
    for &(x, r) in &[(0xff00_0100u32, 0x00ffu32), (0x0080_007f, 0x807f), (0x7fff_8000, 0xff00), (0x0000_0000, 0)] {
        v16(&mut c, "v_sat_pk_u8_i16", 0x62, x, r);
    }
    for &(x, i, u) in &[(1.0f32, 0x7fffu32, 0xffffu32), (-1.0, 0x8001, 0), (0.5, 0x4000, 0x8000), (2.0, 0x7fff, 0xffff), (0.0, 0, 0), (-0.25, 0xe000, 0)] {
        v16(&mut c, "v_cvt_norm_i16_f16", 0x63, f2h(x), i);
        v16(&mut c, "v_cvt_norm_u16_f16", 0x64, f2h(x), u);
    }
    v16(&mut c, "v_cvt_norm_i16_f16", 0x63, 0x7e00, 0);
    v16(&mut c, "v_cvt_norm_u16_f16", 0x64, 0x7e00, 0);
    for (m, op) in [("v_pipeflush", 0x1bu32), ("v_clrexcp", 0x41)] {
        vc(&mut c, m, &[(10, 0x1357_9bdf), (1, 0x2468)], &[vop1(op, 0, 0)], &[10, 1], &[0x1357_9bdf, 0x2468]);
    }
    let mut pre = pr();
    pre.vv(1, 11).vv(2, 22).vv(3, 33).vv(4, 44).vv(10, 100).vv(11, 111).vv(12, 122).vv(13, 133).sv(124, 2);
    let mut post = pr();
    post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11)), vop1(1, 2, v(12)), vop1(1, 3, v(13))]);
    c.add("v_movrelsd_b32", &pre, &[vop1(0x44, 10, v(1))], &post, &[100, 111, 33, 133]);
    let rel = |m0: u32| {
        let mut p = pr();
        p.vv(1, 11).vv(2, 22).vv(3, 33).vv(4, 44).vv(10, 100).vv(11, 111).vv(12, 122).vv(13, 133).sv(124, m0);
        p
    };
    let post4 = |base: u32| {
        let mut p = pr();
        p.raw(&[vop1(1, 0, v(base)), vop1(1, 1, v(base + 1)), vop1(1, 2, v(base + 2)), vop1(1, 3, v(base + 3))]);
        p
    };
    for (m, op, m0, want) in [
        ("v_movrelsd_2_b32", 0x48u32, 0x0001_0002u32, [11u32, 22, 33, 44, 100, 33, 122, 133]),
        ("v_movrelsd_2_b32", 0x48, 0x0003_0000, [11, 22, 33, 44, 100, 111, 122, 11]),
        ("v_swaprel_b32", 0x68, 0x0001_0002, [11, 22, 111, 44, 100, 33, 122, 133]),
        ("v_swaprel_b32", 0x68, 0x0002_0001, [11, 122, 33, 44, 100, 111, 22, 133]),
    ] {
        c.add(m, &rel(m0), &[vop1(op, 10, v(1))], &post4(1), &want[..4]);
        c.add(m, &rel(m0), &[vop1(op, 10, v(1))], &post4(10), &want[4..]);
    }
    c.run_both("vop1");
}

#[test]
fn vector_vop3() {
    let mut c = Cases::new();
    let trip: [(u32, u32, u32); 6] = [(0x1122_3344, 0x5566_7788, 1), (0xff00_ff00, 0x0f0f_0f0f, 0xf0f0_f0f0), (0, 0xffff_ffff, 2), (0x8000_0000, 0x7fff_ffff, 3), (5, 7, 6), (0xfffe_fffd, 0x0001_0002, 0x8000_0001)];
    for &(a, b, d) in &trip {
        v3c(&mut c, "v_bfi_b32", 0x14a, a, b, d, (a & b) | (!a & d));
        let lerp = (0..4).fold(0u32, |acc, k| {
            let (x, y, z) = ((a >> (8 * k)) & 0xff, (b >> (8 * k)) & 0xff, (d >> (8 * k)) & 1);
            acc | (((x + y + z) >> 1) << (8 * k))
        });
        v3c(&mut c, "v_lerp_u8", 0x14d, a, b, d, lerp);
        v3c(&mut c, "v_alignbyte_b32", 0x14f, a, b, d, ((((a as u64) << 32) | b as u64) >> (8 * (d & 3))) as u32);
        v3c(&mut c, "v_min3_u32", 0x153, a, b, d, a.min(b).min(d));
        v3c(&mut c, "v_max3_u32", 0x156, a, b, d, a.max(b).max(d));
        let mut s = [a, b, d];
        s.sort();
        v3c(&mut c, "v_med3_u32", 0x159, a, b, d, s[1]);
        v3c(&mut c, "v_mul_lo_i32", 0x16b, a, b, d, (a as i32).wrapping_mul(b as i32) as u32);
        v3c(&mut c, "v_xor3_b32", 0x178, a, b, d, a ^ b ^ d);
        v3c(&mut c, "v_bfm_b32", 0x363, a & 0x3f, b & 0x3f, 0, if a & 31 == 0 { 0 } else { (((1u64 << (a & 31)) - 1) as u32) << (b & 31) });
        let sad8 = |hi: bool| -> u32 {
            let s: u32 = (0..4).map(|k| (((a >> (8 * k)) & 0xff) as i32 - ((b >> (8 * k)) & 0xff) as i32).unsigned_abs()).sum();
            if hi { (s << 16).wrapping_add(d) } else { s.wrapping_add(d) }
        };
        v3c(&mut c, "v_sad_u8", 0x15a, a, b, d, sad8(false));
        v3c(&mut c, "v_sad_hi_u8", 0x15b, a, b, d, sad8(true));
        let s16: u32 = (0..2).map(|k| (((a >> (16 * k)) & 0xffff) as i32 - ((b >> (16 * k)) & 0xffff) as i32).unsigned_abs()).sum();
        v3c(&mut c, "v_sad_u16", 0x15c, a, b, d, s16.wrapping_add(d));
        v3c(&mut c, "v_sad_u32", 0x15d, a, b, d, ((a as i64 - b as i64).unsigned_abs() as u32).wrapping_add(d));
        v3c(&mut c, "v_cvt_pk_u16_u32", 0x36a, a, b, 0, a.min(0xffff) | (b.min(0xffff) << 16));
        v3c(&mut c, "v_cvt_pk_i16_i32", 0x36b, a, b, 0, ((a as i32).clamp(-32768, 32767) as u32 & 0xffff) | (((b as i32).clamp(-32768, 32767) as u32) << 16));
        v3c(&mut c, "v_mad_u32_u16", 0x373, a, b, d, (a & 0xffff).wrapping_mul(b & 0xffff).wrapping_add(d));
        v3c(&mut c, "v_mad_i32_i16", 0x375, a, b, d, ((a as i16 as i32) * (b as i16 as i32)).wrapping_add(d as i32) as u32);
        v3c(&mut c, "v_sub_nc_i32", 0x376, a, b, 0, a.wrapping_sub(b));
        v3c(&mut c, "v_add_nc_i32", 0x37f, a, b, 0, a.wrapping_add(b));
        vc(&mut c, "v_sub_nc_i32", &[(1, a), (2, b)], &vop3m(0x376, 10, [v(1), v(2), 0], 0, 0, 0, true), &[10], &[(a as i32).saturating_sub(b as i32) as u32]);
        vc(&mut c, "v_add_nc_i32", &[(1, a), (2, b)], &vop3m(0x37f, 10, [v(1), v(2), 0], 0, 0, 0, true), &[10], &[(a as i32).saturating_add(b as i32) as u32]);
        let (au, bu, du) = (a & 0xffff, b & 0xffff, d & 0xffff);
        let (ai, bi, di) = (a as u16 as i16, b as u16 as i16, d as u16 as i16);
        let h = |x: u32| D16 | (x & 0xffff);
        v3c(&mut c, "v_add_nc_u16", 0x303, a, b, 0, h(au + bu));
        v3c(&mut c, "v_sub_nc_u16", 0x304, a, b, 0, h(au.wrapping_sub(bu)));
        v3c(&mut c, "v_mul_lo_u16", 0x305, a, b, 0, h(au * bu));
        v3c(&mut c, "v_lshrrev_b16", 0x307, a, b, 0, h(bu >> (au & 15)));
        v3c(&mut c, "v_ashrrev_i16", 0x308, a, b, 0, h((bi >> (au & 15)) as u32));
        v3c(&mut c, "v_lshlrev_b16", 0x314, a, b, 0, h(bu << (au & 15)));
        v3c(&mut c, "v_max_u16", 0x309, a, b, 0, h(au.max(bu)));
        v3c(&mut c, "v_max_i16", 0x30a, a, b, 0, h(ai.max(bi) as u32));
        v3c(&mut c, "v_min_u16", 0x30b, a, b, 0, h(au.min(bu)));
        v3c(&mut c, "v_min_i16", 0x30c, a, b, 0, h(ai.min(bi) as u32));
        v3c(&mut c, "v_add_nc_i16", 0x30d, a, b, 0, h(ai.wrapping_add(bi) as u32));
        v3c(&mut c, "v_sub_nc_i16", 0x30e, a, b, 0, h(ai.wrapping_sub(bi) as u32));
        vc(&mut c, "v_add_nc_i16", &[(1, a), (2, b)], &vop3m(0x30d, 10, [v(1), v(2), 0], 0, 0, 0, true), &[10], &[h(ai.saturating_add(bi) as u32)]);
        vc(&mut c, "v_sub_nc_u16", &[(1, a), (2, b)], &vop3m(0x304, 10, [v(1), v(2), 0], 0, 0, 0, true), &[10], &[h(au.saturating_sub(bu))]);
        v3c(&mut c, "v_mad_u16", 0x340, a, b, d, h(au.wrapping_mul(bu).wrapping_add(du)));
        v3c(&mut c, "v_mad_i16", 0x35e, a, b, d, h(ai.wrapping_mul(bi).wrapping_add(di) as u32));
        v3c(&mut c, "v_min3_i16", 0x352, a, b, d, h(ai.min(bi).min(di) as u32));
        v3c(&mut c, "v_min3_u16", 0x353, a, b, d, h(au.min(bu).min(du)));
        v3c(&mut c, "v_max3_i16", 0x355, a, b, d, h(ai.max(bi).max(di) as u32));
        v3c(&mut c, "v_max3_u16", 0x356, a, b, d, h(au.max(bu).max(du)));
        let mut si = [ai, bi, di];
        si.sort();
        let mut su = [au, bu, du];
        su.sort();
        v3c(&mut c, "v_med3_i16", 0x358, a, b, d, h(si[1] as u32));
        v3c(&mut c, "v_med3_u16", 0x359, a, b, d, h(su[1]));
        v3c(&mut c, "v_pack_b32_f16", 0x311, a, b, 0, (a & 0xffff) | (b << 16));
        vc(&mut c, "v_add_nc_u16", &[(1, a), (2, b)], &vop3m(0x303, 10, [v(1), v(2), 0], 0, 0, 0b1001, false), &[10], &[(D16 & 0xffff) | (((a >> 16) + bu) << 16)]);
        vc(&mut c, "v_pack_b32_f16", &[(1, a), (2, b)], &vop3m(0x311, 10, [v(1), v(2), 0], 0, 0, 0b0011, false), &[10], &[(a >> 16) | (b & 0xffff_0000)]);
    }
    for &(a, b) in &[(fb(1.0), 5u32), (fb(-3.0), 1)] {
        let _ = (a, b);
    }
    for &(a, b, d) in &[(0x1234_5678u32, 0x0000_0001u32, 0x9abc_def0u32), (0x7fff_ffff, 0xffff_ffff, 0), (0x8000_0000, 1, 0)] {
        for (m, op, r) in [("v_sub_co_u32", 0x310u32, (a as u64).wrapping_sub(b as u64)), ("v_subrev_co_u32", 0x319, (b as u64).wrapping_sub(a as u64))] {
            let borrow = if m == "v_sub_co_u32" { (b > a) as u32 } else { (a > b) as u32 };
            let _ = d;
            vc(&mut c, m, &[(1, a), (2, b), (1020, 0x1234)], &vop3b(op, 10, 20, [v(1), v(2), 0]), &[10, 1020], &[r as u32, borrow]);
        }
    }
    for &(x, e) in &[(1.5f32, 3i32), (1.0, 200), (1.0, -200), (1.5, -1), (-2.0, 127), (3.0, i32::MIN), (1.0, i32::MAX)] {
        let r = if e > 300 { x * f32::INFINITY } else if e < -300 { x * 0.0 } else { (x as f64 * 2f64.powi(e)) as f32 };
        vc(&mut c, "v_ldexp_f32", &[(1, fb(x)), (2, e as u32)], &vop3(0x362, 10, [v(1), v(2), 0]), &[10], &[fb(r)]);
    }
    for &(x, n) in &[(0x8123_4567_89ab_cdefu64, 0u32), (0x8123_4567_89ab_cdef, 4), (0x8123_4567_89ab_cdef, 32), (0x8123_4567_89ab_cdef, 63), (0x8123_4567_89ab_cdef, 0x44), (0x0000_0001_0000_0000, 33)] {
        let ins = [(1, n), (2, x as u32), (3, (x >> 32) as u32)];
        let r = x >> (n & 63);
        vc(&mut c, "v_lshrrev_b64", &ins, &vop3(0x300, 10, [v(1), v(2), 0]), &[10, 11], &[r as u32, (r >> 32) as u32]);
        let r = ((x as i64) >> (n & 63)) as u64;
        vc(&mut c, "v_ashrrev_i64", &ins, &vop3(0x301, 10, [v(1), v(2), 0]), &[10, 11], &[r as u32, (r >> 32) as u32]);
    }
    for &(x, sel, base, r) in &[(200.0f32, 2u32, 0x1122_3344u32, 0x11c8_3344u32), (300.0, 0, 0x1122_3344, 0x1122_33ff), (-5.0, 3, 0x1122_3344, 0x0022_3344), (7.0, 5, 0, 0x0000_0700)] {
        vc(&mut c, "v_cvt_pk_u8_f32", &[(1, fb(x)), (2, sel), (3, base)], &vop3(0x15e, 10, [v(1), v(2), v(3)]), &[10], &[r]);
    }
    for &(a, b, ri, ru) in &[(1.0f32, -1.0f32, 0x8001_7fffu32, 0x0000_ffffu32), (0.0, 2.0, 0x7fff_0000, 0xffff_0000), (-2.0, 0.0, 0x0000_8001, 0)] {
        vc(&mut c, "v_cvt_pknorm_i16_f32", &[(1, fb(a)), (2, fb(b))], &vop3(0x368, 10, [v(1), v(2), 0]), &[10], &[ri]);
        vc(&mut c, "v_cvt_pknorm_u16_f32", &[(1, fb(a)), (2, fb(b))], &vop3(0x369, 10, [v(1), v(2), 0]), &[10], &[ru]);
        vc(&mut c, "v_cvt_pknorm_i16_f16", &[(1, f2h(a)), (2, f2h(b))], &vop3(0x312, 10, [v(1), v(2), 0]), &[10], &[ri]);
        vc(&mut c, "v_cvt_pknorm_u16_f16", &[(1, f2h(a)), (2, f2h(b))], &vop3(0x313, 10, [v(1), v(2), 0]), &[10], &[ru]);
    }
    let pk = |lo: f32, hi: f32| f2h(lo) | (f2h(hi) << 16);
    for &((a0, a1), (b0, b1)) in &[((1.0f32, 2.0f32), (3.0f32, 0.5f32)), ((-1.0, 65504.0), (1.0, 65504.0)), ((0.25, -0.25), (0.125, 8.0))] {
        vc(&mut c, "v_pk_add_f16", &[(1, pk(a0, a1)), (2, pk(b0, b1))], &vop3p(0x0f, 10, [v(1), v(2), 0], 0, 3, 0, 0), &[10], &[pk(a0 + b0, a1 + b1)]);
        vc(&mut c, "v_pk_add_f16", &[(1, pk(a0, a1)), (2, pk(b0, b1))], &vop3p(0x0f, 10, [v(1), v(2), 0], 0b01, 0b10, 0b10, 0), &[10], &[pk(a1 - b0, a0 + b1)]);
    }
    c.run_both("vop3");
}

#[test]
fn vector_f16_preserves_high_half() {
    let mut c = Cases::new();
    vc(&mut c, "v_add_f16", &[(1, 0x1111_0000 | f2h(1.5)), (2, 0x2222_0000 | f2h(2.0))], &[vop2(0x32, 10, v(1), 2)], &[10], &[D16 | f2h(3.5)]);
    vc(&mut c, "v_cvt_f16_f32", &[(1, fb(0.5))], &[vop1(0x0a, 10, v(1))], &[10], &[D16 | f2h(0.5)]);
    vc(&mut c, "v_add_f16", &[(1, (f2h(4.0) << 16) | f2h(1.0)), (2, f2h(0.5))], &vop3m(0x132, 10, [v(1), v(2), 0], 0, 0, 0b1001, false), &[10], &[(D16 & 0xffff) | (f2h(4.5) << 16)]);
    c.run_both("f16_high");
}

pub fn shader_root(words: &[u32], root: tombox_xshader::meta::RootSig) -> ShaderExtract {
    let mut ex = shader(words);
    ex.has_root_sig = true;
    ex.root_sig = Some(root);
    ex
}

#[test]
#[ignore]
fn probe_resource_hlsl() {
    use tombox_xshader::meta::{RootParam, RootParamKind, RootRange, RootSig};
    let table = |range_type: u32, num: u32| RootParam { visibility: 5, kind: RootParamKind::Table(vec![RootRange { range_type, num, base_register: 0, space: 0, offset_in_table: u32::MAX }]) };
    let root = RootSig { version: 2, flags: 1, params: vec![table(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut p = Prog::new();
    p.vv(1, 4).raw(&[0xf408_0004, 0xfa00_0000, 0xe020_1005, 0x8000_0401, vop1(1, 0, v(4))]);
    println!("{}", translate(&shader_root(&p.words(), root.clone())).map(|t| t.hlsl).unwrap_or_else(|e| e));
    let mut p = Prog::new();
    p.vv(1, 4).vv(2, 9).raw(&[0xf408_0004, 0xfa00_0000, 0xe0dc_4000, 0x8000_0201, vop1(1, 0, v(2))]);
    println!("{}", translate(&shader_root(&p.words(), root.clone())).map(|t| t.hlsl).unwrap_or_else(|e| e));
    let root2 = RootSig { version: 2, flags: 1, params: vec![table(0, 64), table(3, 16)], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut p = Prog::new();
    p.vv(1, fb(0.5)).vv(2, fb(0.5)).raw(&[0xf40c_0004, 0xfa00_0000, 0xf408_0305, 0xfa00_0000, 0xf080_0f08, 0x0060_0401, vop1(1, 0, v(4))]);
    println!("{}", translate(&shader_root(&p.words(), root2)).map(|t| t.hlsl).unwrap_or_else(|e| e));
}

fn rtable(range_type: u32, num: u32) -> tombox_xshader::meta::RootParam {
    use tombox_xshader::meta::{RootParam, RootParamKind, RootRange};
    RootParam { visibility: 5, kind: RootParamKind::Table(vec![RootRange { range_type, num, base_register: 0, space: 0, offset_in_table: u32::MAX }]) }
}

fn mubuf(op: u32, offset: u32, offen: bool, idxen: bool, glc: bool, vaddr: u32, vdata: u32) -> [u32; 2] {
    [0xe000_0000 | (op << 18) | ((glc as u32) << 14) | ((idxen as u32) << 13) | ((offen as u32) << 12) | offset, vaddr | (vdata << 8) | (0x80 << 24)]
}

const BUF: [u32; 8] = [0x8382_8180, 0x8786_8584, 0x0302_0100, 0x0706_0504, 0xffff_fff0, 0x0000_0010, 0x7fff_ffff, 0x8000_0000];

struct BufCase {
    m: &'static str,
    pre: Vec<u32>,
    op: [u32; 2],
    rt: Vec<u32>,
    mem: Vec<(usize, u32)>,
}

fn run_buf_cases(group: &str, cs: &[BufCase]) {
    run_buf_cases_psv(group, cs, &[]);
}

fn run_buf_cases_psv(group: &str, cs: &[BufCase], psv: &[tombox_xshader::meta::PsvResource]) {
    run_buf_cases_core(group, cs, &BUF, psv);
}

fn run_buf_cases_buf(group: &str, cs: &[BufCase], buf: &[u32]) {
    run_buf_cases_core(group, cs, buf, &[]);
}

fn run_buf_cases_core(group: &str, cs: &[BufCase], buf: &[u32], psv: &[tombox_xshader::meta::PsvResource]) {
    let root = tombox_xshader::meta::RootSig { version: 2, flags: 1, params: vec![rtable(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut fails = Vec::new();
    let mut all: std::collections::BTreeSet<&str> = Default::default();
    let mut bad: std::collections::BTreeSet<&str> = Default::default();
    for (i, c, rt) in cs.iter().enumerate().flat_map(|(i, c)| [(i, c, false), (i, c, true)]) {
        all.insert(c.m);
        let tag = if rt { " (runtime inputs)" } else { "" };
        let isa: Vec<u8> = c.op.iter().flat_map(|w| w.to_le_bytes()).collect();
        let got_m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
        if got_m != c.m {
            fails.push(format!("#{i} {}: decodes as {got_m}", c.m));
            bad.insert(c.m);
            continue;
        }
        let mut p = Prog::new();
        p.raw(&if rt { vop::opaque(&c.pre) } else { c.pre.clone() }).raw(&[0xf408_0004, 0xfa00_0000]).raw(&c.op).raw(&[vop1(1, 0, v(4)), vop1(1, 1, v(5))]);
        match run_res_psv(&p.words(), root.clone(), Some(buf), None, sampler(0, 3, 0), rt, psv) {
            Ok(o) => {
                let mut ok = o.rt[..c.rt.len()] == c.rt[..];
                let mut got = Vec::new();
                for &(k, want) in &c.mem {
                    got.push(o.buf[k]);
                    ok &= o.buf[k] == want;
                }
                if !ok {
                    let h = translate(&shader_psv(&p.words(), root.clone(), psv)).map(|t| t.hlsl).unwrap_or_default();
                    let body: String = h.lines().filter(|l| l.starts_with("    ") && !l.contains("TB_RTFMT")).collect::<Vec<_>>().join("\n");
                    fails.push(format!("#{i} {}{tag}: rt {:08x?} want {:08x?}; mem {:08x?} want {:08x?}\n{body}", c.m, &o.rt[..c.rt.len()], c.rt, got, c.mem));
                    bad.insert(c.m);
                }
            }
            Err(e) => {
                fails.push(format!("#{i} {}{tag}: {}", c.m, e.lines().take(3).collect::<Vec<_>>().join(" | ")));
                bad.insert(c.m);
            }
        }
    }
    let ok: Vec<&str> = all.iter().filter(|m| !bad.contains(*m)).copied().collect();
    if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{group}.txt")), ok.join("\n") + "\n");
    }
    println!("{group}: {} mnemonics verified, {} failing, {} cases", ok.len(), bad.len(), cs.len());
    assert!(fails.is_empty(), "{group}: {} failing\n{}", fails.len(), fails.join("\n\n"));
}

fn bytes_of(b: &[u32]) -> Vec<u8> {
    b.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn mubuf_scalar_offset() {
    let mut cases = Vec::new();
    let source = bytes_of(&BUF);
    let with_soffset = |opcode, immediate, offen| {
        let mut words = mubuf(opcode, immediate, offen, false, false, 1, 4);
        words[1] = (words[1] & 0x00ff_ffff) | (12 << 24);
        words
    };
    for (scalar, vector, immediate, offen) in [(5, 0, 0, false), (0, 4, 2, true), (8, 3, 2, true)] {
        let offset = (scalar + vector + immediate) as usize;
        let mut pre = Prog::new();
        pre.sv(12, scalar).vv(1, vector).vv(4, 0xab);
        cases.push(BufCase {
            m: "buffer_load_ubyte",
            pre: pre.w.clone(),
            op: with_soffset(0x08, immediate, offen),
            rt: vec![u32::from(source[offset])],
            mem: vec![],
        });
        let word = offset / 4;
        let shift = (offset % 4) * 8;
        let expected = (BUF[word] & !(0xff << shift)) | (0xab << shift);
        cases.push(BufCase {
            m: "buffer_store_byte",
            pre: pre.w,
            op: with_soffset(0x18, immediate, offen),
            rt: vec![],
            mem: vec![(word, expected), (word ^ 1, BUF[word ^ 1])],
        });
    }
    let value = 0x1234_56ab;
    for (scalar, vector, immediate, offen) in [(8, 0, 0, false), (0, 4, 4, true), (8, 4, 4, true)] {
        let word = ((scalar + vector + immediate) / 4) as usize;
        let mut pre = Prog::new();
        pre.sv(12, scalar).vv(1, vector).vv(4, value);
        cases.push(BufCase {
            m: "buffer_load_dword",
            pre: pre.w.clone(),
            op: with_soffset(0x0c, immediate, offen),
            rt: vec![BUF[word]],
            mem: vec![],
        });
        cases.push(BufCase {
            m: "buffer_store_dword",
            pre: pre.w,
            op: with_soffset(0x1c, immediate, offen),
            rt: vec![],
            mem: vec![(word, value), (word ^ 1, BUF[word ^ 1])],
        });
    }
    run_buf_cases("mubuf_soffset", &cases);
}

#[test]
fn mubuf_atomic_scalar_offset() {
    type At = fn(u32, u32, u32) -> u32;
    let atoms: [(&'static str, u32, At); 3] = [
        ("buffer_atomic_add", 0x32, |m, s, _| m.wrapping_add(s)),
        ("buffer_atomic_umax", 0x38, |m, s, _| m.max(s)),
        ("buffer_atomic_cmpswap", 0x31, |m, s, c| if m == c { s } else { m }),
    ];
    let forms: [(u32, u32, u32, u32, bool, bool, Option<u32>); 7] = [
        (0, 0, 0, 20, false, false, Some(12)),
        (0, 0, 4, 8, false, false, Some(12)),
        (8, 0, 4, 8, true, false, Some(12)),
        (1, 8, 0, 0, true, true, None),
        (2, 4, 4, 8, true, true, Some(12)),
        (3, 0, 16, 0, false, true, None),
        (1, 0, 0, 12, false, true, Some(12)),
    ];
    let raw = tombox_xshader::meta::PsvResource { res_type: 7, space: 0, lower: 0, upper: 7, kind: 1 };
    for (group, psv) in [("mubuf_atomic_soffset_typed", Vec::new()), ("mubuf_atomic_soffset_raw", vec![raw])] {
        let mut cs = Vec::new();
        for (m, op, f) in atoms {
            for &(a1, a2, imm, scalar, offen, idxen, sreg) in &forms {
                if idxen && !psv.is_empty() {
                    continue;
                }
                let w = if idxen { a1 + (a2 * offen as u32 + scalar + imm) / 4 } else { (a1 * offen as u32 + scalar + imm) / 4 } as usize;
                let at = |k: usize| BUF.get(k).copied().unwrap_or(0);
                let (old, data) = (at(w), 0x10);
                for glc in [true, false] {
                    let mut p = Prog::new();
                    p.vv(1, a1).vv(2, a2).vv(4, data).vv(5, old).sv(12, scalar);
                    let mut word = mubuf(op, imm, offen, idxen, glc, 1, 4);
                    if let Some(s) = sreg {
                        word[1] = (word[1] & 0x00ff_ffff) | (s << 24);
                    }
                    cs.push(BufCase { m, pre: p.w, op: word, rt: vec![if glc { old } else { data }], mem: vec![(w, f(old, data, old)), (w ^ 1, at(w ^ 1))] });
                }
            }
        }
        run_buf_cases_psv(group, &cs, &psv);
    }
}

#[test]
fn mubuf_atomic_x2() {
    type At64 = fn(u64, u64, u64) -> u64;
    let atoms: [(&'static str, u32, At64); 13] = [
        ("buffer_atomic_swap_x2", 0x50, |_, s, _| s),
        ("buffer_atomic_cmpswap_x2", 0x51, |m, s, c| if m == c { s } else { m }),
        ("buffer_atomic_add_x2", 0x52, |m, s, _| m.wrapping_add(s)),
        ("buffer_atomic_sub_x2", 0x53, |m, s, _| m.wrapping_sub(s)),
        ("buffer_atomic_smin_x2", 0x55, |m, s, _| (m as i64).min(s as i64) as u64),
        ("buffer_atomic_umin_x2", 0x56, |m, s, _| m.min(s)),
        ("buffer_atomic_smax_x2", 0x57, |m, s, _| (m as i64).max(s as i64) as u64),
        ("buffer_atomic_umax_x2", 0x58, |m, s, _| m.max(s)),
        ("buffer_atomic_and_x2", 0x59, |m, s, _| m & s),
        ("buffer_atomic_or_x2", 0x5a, |m, s, _| m | s),
        ("buffer_atomic_xor_x2", 0x5b, |m, s, _| m ^ s),
        ("buffer_atomic_inc_x2", 0x5c, |m, s, _| if m >= s { 0 } else { m + 1 }),
        ("buffer_atomic_dec_x2", 0x5d, |m, s, _| if m == 0 || m > s { s } else { m - 1 }),
    ];
    let q64 = |q: usize| ((BUF[q * 2 + 1] as u64) << 32) | BUF[q * 2] as u64;
    let inputs: [(usize, u64, u64); 8] = [
        (0, 0x0000_0000_0000_0001, 0x8786_8584_8382_8180),
        (1, 0xffff_ffff_ffff_ffff, 0x0706_0504_0302_0100),
        (2, 0x0000_0000_0000_0010, 0x0000_0010_ffff_fff0),
        (3, 0x8000_0000_0000_0000, 0x8000_0000_7fff_ffff),
        (2, 0x0000_0000_0000_0020, 0xdead_beef_cafe_babe),
        (0, 0x0000_0000_0000_0000, 0x0000_0000_0000_0000),
        (1, 0x8000_0000_0000_0001, 0x0000_0000_0000_0000),
        (3, 0x0000_0000_0000_0007, 0xffff_ffff_ffff_ffff),
    ];
    let raw = tombox_xshader::meta::PsvResource { res_type: 7, space: 0, lower: 0, upper: 7, kind: 1 };
    for (group, psv) in [("mubuf_atomic_x2_typed", Vec::new()), ("mubuf_atomic_x2_raw", vec![raw])] {
        let mut cs = Vec::new();
        for (m, opc, f) in atoms {
            for &(q, s, cmp) in &inputs {
                let old = q64(q);
                let new = f(old, s, cmp);
                for glc in [true, false] {
                    let mut p = Prog::new();
                    p.vv(1, (q * 8) as u32).vv(4, s as u32).vv(5, (s >> 32) as u32).vv(6, cmp as u32).vv(7, (cmp >> 32) as u32);
                    let rv = if glc { old } else { s };
                    cs.push(BufCase {
                        m,
                        pre: p.w,
                        op: mubuf(opc, 0, true, false, glc, 1, 4),
                        rt: vec![rv as u32, (rv >> 32) as u32],
                        mem: vec![(q * 2, new as u32), (q * 2 + 1, (new >> 32) as u32)],
                    });
                }
            }
        }
        run_buf_cases_psv(group, &cs, &psv);
    }
}

#[test]
fn buffer_mubuf() {
    let mut cs = Vec::new();
    let by = bytes_of(&BUF);
    let pre = |a: u32, d: u32, cmp: u32| {
        let mut p = Prog::new();
        p.vv(1, a).vv(4, d).vv(5, cmp).vv(2, 0x1111);
        p.w
    };
    for &(a, imm) in &[(4u32, 1u32), (8, 1), (0, 3), (12, 0), (5, 2)] {
        let o = (a + imm) as usize;
        cs.push(BufCase { m: "buffer_load_ubyte", pre: pre(a, 0x5555_5555, 0), op: mubuf(0x08, imm, true, false, false, 1, 4), rt: vec![by[o] as u32], mem: vec![] });
        cs.push(BufCase { m: "buffer_load_sbyte", pre: pre(a, 0x5555_5555, 0), op: mubuf(0x09, imm, true, false, false, 1, 4), rt: vec![by[o] as i8 as i32 as u32], mem: vec![] });
        let o2 = o & !1;
        let h = u16::from_le_bytes([by[o2], by[o2 + 1]]);
        cs.push(BufCase { m: "buffer_load_ushort", pre: pre(a & !1, 0x5555_5555, 0), op: mubuf(0x0a, imm & !1, true, false, false, 1, 4), rt: vec![h as u32], mem: vec![] });
        cs.push(BufCase { m: "buffer_load_sshort", pre: pre(a & !1, 0x5555_5555, 0), op: mubuf(0x0b, imm & !1, true, false, false, 1, 4), rt: vec![h as i16 as i32 as u32], mem: vec![] });
        let w = o / 4;
        let mut nb = by.clone();
        nb[o] = 0xab;
        let word = u32::from_le_bytes(nb[w * 4..w * 4 + 4].try_into().unwrap());
        cs.push(BufCase { m: "buffer_store_byte", pre: pre(a, 0x1234_56ab, 0), op: mubuf(0x18, imm, true, false, false, 1, 4), rt: vec![], mem: vec![(w, word), (w ^ 1, BUF[w ^ 1])] });
        let mut nb = by.clone();
        nb[o2] = 0xab;
        nb[o2 + 1] = 0x56;
        let w2 = o2 / 4;
        let word = u32::from_le_bytes(nb[w2 * 4..w2 * 4 + 4].try_into().unwrap());
        cs.push(BufCase { m: "buffer_store_short", pre: pre(a & !1, 0x1234_56ab, 0), op: mubuf(0x1a, imm & !1, true, false, false, 1, 4), rt: vec![], mem: vec![(w2, word)] });
        let keep = 0x5a5a_a5a5u32;
        let lo = |v: u32| (keep & 0xffff_0000) | (v & 0xffff);
        let hi = |v: u32| (keep & 0xffff) | (v << 16);
        let sb = by[o] as i8 as i32 as u32;
        for (m, op, aa, ii, want) in [
            ("buffer_load_ubyte_d16", 0x20, a, imm, lo(by[o] as u32)),
            ("buffer_load_ubyte_d16_hi", 0x21, a, imm, hi(by[o] as u32)),
            ("buffer_load_sbyte_d16", 0x22, a, imm, lo(sb)),
            ("buffer_load_sbyte_d16_hi", 0x23, a, imm, hi(sb)),
            ("buffer_load_short_d16", 0x24, a & !1, imm & !1, lo(h as u32)),
            ("buffer_load_short_d16_hi", 0x25, a & !1, imm & !1, hi(h as u32)),
        ] {
            cs.push(BufCase { m, pre: pre(aa, keep, 0), op: mubuf(op, ii, true, false, false, 1, 4), rt: vec![want], mem: vec![] });
        }
        let mut nb = by.clone();
        nb[o] = 0xab;
        let word = u32::from_le_bytes(nb[w * 4..w * 4 + 4].try_into().unwrap());
        cs.push(BufCase { m: "buffer_store_byte_d16_hi", pre: pre(a, 0x56ab_1234, 0), op: mubuf(0x19, imm, true, false, false, 1, 4), rt: vec![], mem: vec![(w, word), (w ^ 1, BUF[w ^ 1])] });
        let mut nb = by.clone();
        nb[o2] = 0xab;
        nb[o2 + 1] = 0x56;
        let word = u32::from_le_bytes(nb[w2 * 4..w2 * 4 + 4].try_into().unwrap());
        cs.push(BufCase { m: "buffer_store_short_d16_hi", pre: pre(a & !1, 0x56ab_1234, 0), op: mubuf(0x1b, imm & !1, true, false, false, 1, 4), rt: vec![], mem: vec![(w2, word)] });
    }
    for &idx in &[0u32, 2, 5] {
        cs.push(BufCase { m: "buffer_store_format_x", pre: pre(idx, 0xc0ff_ee00, 0), op: mubuf(0x04, 0, false, true, false, 1, 4), rt: vec![], mem: vec![(idx as usize, 0xc0ff_ee00), ((idx as usize + 1) % 8, BUF[(idx as usize + 1) % 8])] });
    }
    let pre_u4 = |idx: u32, x: u32, y: u32, z: u32| {
        let mut p = Prog::new();
        p.vv(1, idx).vv(4, x).vv(5, y).vv(6, z).raw(&[vop2(0x25, 4, ic(0), 4)]);
        p.w
    };
    cs.push(BufCase {
        m: "buffer_store_format_xy",
        pre: pre_u4(0, 0x1122_3344, 0x5566_7788, 0x99aa_bbcc),
        op: mubuf(0x05, 0, false, true, false, 1, 4),
        rt: vec![],
        mem: vec![(0, 0x1122_3344), (1, 0x5566_7788), (2, 0), (3, 0), (4, BUF[4])],
    });
    cs.push(BufCase {
        m: "buffer_store_format_xyz",
        pre: pre_u4(1, 0xdead_0001, 0xbeef_0002, 0xcafe_0003),
        op: mubuf(0x06, 0, false, true, false, 1, 4),
        rt: vec![],
        mem: vec![(0, BUF[0]), (4, 0xdead_0001), (5, 0xbeef_0002), (6, 0xcafe_0003), (7, 0)],
    });
    type At = fn(u32, u32, u32) -> u32;
    let atoms: [(&'static str, u32, At); 9] = [
        ("buffer_atomic_sub", 0x33, |m, s, _| m.wrapping_sub(s)),
        ("buffer_atomic_smin", 0x35, |m, s, _| (m as i32).min(s as i32) as u32),
        ("buffer_atomic_smax", 0x37, |m, s, _| (m as i32).max(s as i32) as u32),
        ("buffer_atomic_and", 0x39, |m, s, _| m & s),
        ("buffer_atomic_or", 0x3a, |m, s, _| m | s),
        ("buffer_atomic_xor", 0x3b, |m, s, _| m ^ s),
        ("buffer_atomic_inc", 0x3c, |m, s, _| if m >= s { 0 } else { m + 1 }),
        ("buffer_atomic_dec", 0x3d, |m, s, _| if m == 0 || m > s { s } else { m - 1 }),
        ("buffer_atomic_cmpswap", 0x31, |m, s, c| if m == c { s } else { m }),
    ];
    for (m, op, f) in atoms {
        for &(w, s, cmp) in &[(4usize, 5u32, 0xffff_fff0u32), (5, 0x10, 0x10), (6, 0x8000_0000, 7), (7, 0xffff_ffff, 0x8000_0000), (5, 0x20, 1), (2, 0, 0)] {
            let old = BUF[w];
            cs.push(BufCase { m, pre: pre(w as u32 * 4, s, cmp), op: mubuf(op, 0, true, false, true, 1, 4), rt: vec![old], mem: vec![(w, f(old, s, cmp))] });
            cs.push(BufCase { m, pre: pre(w as u32 * 4, s, cmp), op: mubuf(op, 0, true, false, false, 1, 4), rt: vec![s], mem: vec![(w, f(old, s, cmp))] });
        }
    }
    run_buf_cases("mubuf", &cs);
    let raw = tombox_xshader::meta::PsvResource { res_type: 7, space: 0, lower: 0, upper: 7, kind: 1 };
    let flat: Vec<BufCase> = cs.into_iter().filter(|c| c.op[0] & (1 << 13) == 0 && !c.m.contains("_format_")).collect();
    assert!(flat.len() > 60, "raw-view case count {}", flat.len());
    run_buf_cases_psv("mubuf_raw_view", &flat, &[raw]);
}

#[test]
fn buffer_mubuf_format_d16() {
    let vals: [u32; 8] = [fb(1.5), fb(0.1), fb(1.0), fb(-2.0), fb(1000.5), fb(0.5), fb(1.000_000_1), fb(0.25)];
    let mut cs = Vec::new();
    for (idx, &w) in vals.iter().enumerate() {
        let h = f2h(f32::from_bits(w));
        let keep = 0x5a5a_a5a5u32;
        cs.push(BufCase { m: "buffer_load_format_d16_x", pre: { let mut p = Prog::new(); p.vv(1, idx as u32).vv(4, keep); p.w }, op: mubuf(0x80, 0, false, true, false, 1, 4), rt: vec![(keep & 0xffff_0000) | h], mem: vec![] });
        cs.push(BufCase { m: "buffer_load_format_d16_hi_x", pre: { let mut p = Prog::new(); p.vv(1, idx as u32).vv(4, keep); p.w }, op: mubuf(0x26, 0, false, true, false, 1, 4), rt: vec![(keep & 0xffff) | (h << 16)], mem: vec![] });
        cs.push(BufCase { m: "buffer_store_format_d16_x", pre: { let mut p = Prog::new(); p.vv(1, idx as u32).vv(4, h); p.w }, op: mubuf(0x84, 0, false, true, false, 1, 4), rt: vec![], mem: vec![(idx, fb(h2f(h))), ((idx + 1) % 8, vals[(idx + 1) % 8])] });
        cs.push(BufCase { m: "buffer_store_format_d16_hi_x", pre: { let mut p = Prog::new(); p.vv(1, idx as u32).vv(4, h << 16); p.w }, op: mubuf(0x27, 0, false, true, false, 1, 4), rt: vec![], mem: vec![(idx, fb(h2f(h))), ((idx + 1) % 8, vals[(idx + 1) % 8])] });
    }
    run_buf_cases_buf("mubuf_format_d16", &cs, &vals);
}

#[test]
fn mubuf_format_d16_refuses_integer_slots() {
    let root = tombox_xshader::meta::RootSig { version: 2, flags: 1, params: vec![rtable(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut w = vec![vop1(1, 1, LIT), 0, 0xf408_0004, 0xfa00_0000];
    w.extend_from_slice(&mubuf(0x80, 0, false, true, false, 1, 4));
    w.push(vop2(0x25, 5, ic(0), 4));
    w.push(S_ENDPGM);
    let e = translate(&shader_root(&w, root)).err().expect("d16 on an integer slot must be refused");
    assert!(e.contains("integer slot"), "{e}");
}

fn dw(x: f64) -> [u32; 2] {
    let b = x.to_bits();
    [b as u32, (b >> 32) as u32]
}

fn sad4(s0: u64, s1: u32, off: u32, masked: bool) -> u32 {
    (0..4)
        .map(|k| {
            let x = ((s0 >> (8 * (off + k))) & 0xff) as i32;
            let y = ((s1 >> (8 * k)) & 0xff) as i32;
            if masked && y == 0 { 0 } else { (x - y).unsigned_abs() }
        })
        .sum()
}

fn msad_field(s0: u64, s1: u32, off: u32, c: u32) -> u32 {
    sad4(s0, s1, off, true).wrapping_add(c)
}

#[test]
fn sad_quad_packed() {
    let mut c = Cases::new();
    let pk = |s0: u64, s1: u32, s2: u64, masked: bool| -> [u32; 2] {
        let f: Vec<u32> = (0..4)
            .map(|k| (sad4(s0, s1, k, masked) + ((s2 >> (16 * k)) as u32 & 0xffff)) & 0xffff)
            .collect();
        [f[0] | (f[1] << 16), f[2] | (f[3] << 16)]
    };
    let triples: [(u64, u32, u64); 5] = [
        (0, 0, 0),
        (0x0807_0605_0403_0201, 0x0403_0201, 0x0004_0003_0002_0001),
        (0xffff_ffff_ffff_ffff, 0x0102_0304, 0xffff_ffff_ffff_ffff),
        (0x0000_0000_0000_00ff, 0x0000_0000, 0x8000_0000_8000_0000),
        (0x5566_7788_1122_3344, 0x00ff_0000, 0x0100_0000_ff00_0000),
    ];
    for &(s0, s1, s2) in &triples {
        let src = [(2, s0 as u32), (3, (s0 >> 32) as u32), (4, s1), (6, s2 as u32), (7, (s2 >> 32) as u32)];
        vc(&mut c, "v_qsad_pk_u16_u8", &src, &vop3(0x172, 10, [v(2), v(4), v(6)]), &[10, 11], &pk(s0, s1, s2, false));
        vc(&mut c, "v_mqsad_pk_u16_u8", &src, &vop3(0x173, 10, [v(2), v(4), v(6)]), &[10, 11], &pk(s0, s1, s2, true));
    }
    let quads: [(u64, u32, u128); 5] = [
        (0x0807_0605_0403_0201, 0x0403_0201, 0x0000_0000_0000_0000_0000_0000_0000_0000),
        (0xffff_ffff_ffff_ffff, 0x0000_0000, 0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
        (0x0000_0000_0000_00ff, 0x00ff_00ff, 0x8000_0000_0000_0001_ffff_fffe_0000_0002),
        (0x5566_7788_1122_3344, 0x00ff_0000, 0x0000_0000_ffff_ffff_0101_0101_7fff_ffff),
        (0x0102_0304_0506_0708, 0x0807_0605, 0x1111_1111_2222_2222_3333_3333_4444_4444),
    ];
    for &(s0, s1, s2) in &quads {
        let src = [(2, s0 as u32), (3, (s0 >> 32) as u32), (4, s1), (6, s2 as u32), (7, (s2 >> 32) as u32), (8, (s2 >> 64) as u32), (9, (s2 >> 96) as u32)];
        let e: Vec<u32> = (0..4).map(|k| msad_field(s0, s1, k, ((s2 >> (32 * k)) & 0xffff_ffff) as u32)).collect();
        vc(&mut c, "v_mqsad_u32_u8", &src, &vop3(0x175, 10, [v(2), v(4), v(6)]), &[10, 11, 12, 13], &e);
        let ec: Vec<u32> = (0..4)
            .map(|k| {
                let cc = ((s2 >> (32 * k)) & 0xffff_ffff) as u32;
                let t = msad_field(s0, s1, k, cc);
                if t < cc { 0xffff_ffff } else { t }
            })
            .collect();
        vc(&mut c, "v_mqsad_u32_u8", &src, &vop3m(0x175, 10, [v(2), v(4), v(6)], 0, 0, 0, true), &[10, 11, 12, 13], &ec);
    }
    c.run("sad quad");
}

fn d1(c: &mut Cases, m: &'static str, op: u32, x: f64, out: f64) {
    let [lo, hi] = dw(x);
    let [ol, oh] = dw(out);
    vc(c, m, &[(2, lo), (3, hi)], &[vop1(op, 10, v(2))], &[10, 11], &[ol, oh]);
}

fn d3(c: &mut Cases, m: &'static str, op: u32, a: u64, b: u64, cc: u64, out: [u32; 2]) {
    vc(c, m, &[(2, a as u32), (3, (a >> 32) as u32), (4, b as u32), (5, (b >> 32) as u32), (6, cc as u32), (7, (cc >> 32) as u32)], &vop3(op, 10, [v(2), v(4), v(6)]), &[10, 11], &out);
}

fn cvt_i64(x: f64, lo: f64, hi: f64) -> u32 {
    if x.is_nan() { 0 } else { x.max(lo).min(hi).trunc() as i64 as u32 }
}

fn rne64(x: f64) -> f64 {
    let r = x.round();
    (if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 { r - x.signum() } else { r }).copysign(x)
}

#[test]
fn vector_f64() {
    let mut c = Cases::new();
    let xs = [2.5f64, -2.5, 3.5, -0.5, 1e300, -1e300, f64::NAN, 4294967296.5, 0.75, -1.25, 1e-310];
    for &x in &xs {
        vc(&mut c, "v_cvt_i32_f64", &[(2, dw(x)[0]), (3, dw(x)[1])], &[vop1(0x03, 10, v(2))], &[10], &[cvt_i64(x, -2147483648.0, 2147483647.0)]);
        vc(&mut c, "v_cvt_u32_f64", &[(2, dw(x)[0]), (3, dw(x)[1])], &[vop1(0x15, 10, v(2))], &[10], &[if x.is_nan() { 0 } else { x.max(0.0).min(4294967295.0).trunc() as u32 }]);
        if !x.is_nan() && x.abs() < 1e38 && x.abs() > 1e-30 {
            vc(&mut c, "v_cvt_f32_f64", &[(2, dw(x)[0]), (3, dw(x)[1])], &[vop1(0x0f, 10, v(2))], &[10], &[fb(x as f32)]);
        }
        if !x.is_nan() {
            d1(&mut c, "v_trunc_f64", 0x17, x, x.trunc());
            d1(&mut c, "v_ceil_f64", 0x18, x, x.ceil());
            d1(&mut c, "v_floor_f64", 0x1a, x, x.floor());
            d1(&mut c, "v_rndne_f64", 0x19, x, rne64(x));
            if x.abs() < 1e300 && x.abs() > 1e-300 {
                d1(&mut c, "v_fract_f64", 0x3e, x, x - x.floor());
            }
        }
    }
    vc(&mut c, "v_cvt_f32_f64", &[(2, dw(1.0 + 2f64.powi(-24))[0]), (3, dw(1.0 + 2f64.powi(-24))[1])], &[vop1(0x0f, 10, v(2))], &[10], &[fb(1.0)]);
    vc(&mut c, "v_cvt_f32_f64", &[(2, dw(1.0 + 3.0 * 2f64.powi(-24))[0]), (3, dw(1.0 + 3.0 * 2f64.powi(-24))[1])], &[vop1(0x0f, 10, v(2))], &[10], &[fb(1.0 + 2f32.powi(-22))]);
    for &i in &[0i32, -5, i32::MIN, i32::MAX, 123456789] {
        vc(&mut c, "v_cvt_f64_i32", &[(2, i as u32)], &[vop1(0x04, 10, v(2))], &[10, 11], &dw(i as f64));
        vc(&mut c, "v_cvt_f64_u32", &[(2, i as u32)], &[vop1(0x16, 10, v(2))], &[10, 11], &dw(i as u32 as f64));
    }
    for &x in &[1.5f32, -0.0, f32::INFINITY, 1e-40, 3.4e38] {
        vc(&mut c, "v_cvt_f64_f32", &[(2, fb(x))], &[vop1(0x10, 10, v(2))], &[10, 11], &dw(x as f64));
    }
    for &(x, r, s, q) in &[(4.0f64, 0.25f64, 0.5f64, 2.0f64), (0.25, 4.0, 2.0, 0.5), (16.0, 0.0625, 0.25, 4.0), (1.0, 1.0, 1.0, 1.0)] {
        d1(&mut c, "v_rcp_f64", 0x2f, x, r);
        d1(&mut c, "v_rsq_f64", 0x31, x, s);
        d1(&mut c, "v_sqrt_f64", 0x34, x, f64::from_bits(q.to_bits() | 0x0400_0000));
    }
    for &(x, m, e) in &[(12.0f64, 0.75f64, 4i32), (-3.0, -0.75, 2), (0.5, 0.5, 0), (1e-310, 1e-310 * 2f64.powi(515) * 2f64.powi(514), -1029), (0.0, 0.0, 0)] {
        d1(&mut c, "v_frexp_mant_f64", 0x3d, x, m);
        vc(&mut c, "v_frexp_exp_i32_f64", &[(2, dw(x)[0]), (3, dw(x)[1])], &[vop1(0x3c, 10, v(2))], &[10], &[e as u32]);
    }
    let pairs = [(1.5f64, 2.25f64, -0.75f64), (-1e10, 3e-5, 7.0), (0.1, 0.2, 0.3), (1.0 + 2f64.powi(-52), 1.0 - 2f64.powi(-52), -1.0)];
    for &(a, b, cc) in &pairs {
        let (ab, bb, cb) = (a.to_bits(), b.to_bits(), cc.to_bits());
        d3(&mut c, "v_add_f64", 0x164, ab, bb, 0, dw(a + b));
        d3(&mut c, "v_mul_f64", 0x165, ab, bb, 0, dw(a * b));
        d3(&mut c, "v_min_f64", 0x166, ab, bb, 0, dw(a.min(b)));
        d3(&mut c, "v_max_f64", 0x167, ab, bb, 0, dw(a.max(b)));
        d3(&mut c, "v_fma_f64", 0x14c, ab, bb, cb, dw(a.mul_add(b, cc)));
    }
    for &(x, e) in &[(1.5f64, 3i32), (1.0, 2000), (1.0, -2000), (-3.0, -1), (1.0, -1074)] {
        vc(&mut c, "v_ldexp_f64", &[(2, dw(x)[0]), (3, dw(x)[1]), (4, e as u32)], &vop3(0x168, 10, [v(2), v(4), 0]), &[10, 11], &dw(x * 2f64.powi(e.clamp(-1100, 1100) / 2) * 2f64.powi(e.clamp(-1100, 1100) - e.clamp(-1100, 1100) / 2)));
    }
    for &(q, den, num, r) in &[(2.5f64, 2.0f64, 5.0f64, 2.5f64), (7.0, 0.0, 1.0, f64::INFINITY), (7.0, 0.0, -1.0, f64::NEG_INFINITY), (7.0, f64::INFINITY, 1.0, 0.0), (7.0, 1.0, f64::INFINITY, f64::INFINITY)] {
        d3(&mut c, "v_div_fixup_f64", 0x160, q.to_bits(), den.to_bits(), num.to_bits(), dw(r));
    }
    for &(a, b, cc) in &[(6.0f64, 2.0f64, 6.0f64), (1.0, 3.0, 1.0)] {
        let mut pre = pr();
        for (k, x) in [a, b, cc].iter().enumerate() {
            pre.vv(2 + 2 * k as u32, dw(*x)[0]).vv(3 + 2 * k as u32, dw(*x)[1]);
        }
        pre.sv(20, 0x77);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 20);
        c.add("v_div_scale_f64", &pre, &vop3b(0x16e, 10, 20, [v(2), v(4), v(6)]), &post, &[dw(a)[0], dw(a)[1], 0]);
    }
    for &(a, b, cc, vcc) in &[(1.5f64, 2.0f64, 0.25f64, 0u32)][..0] {
        let mut pre = pr();
        for (k, x) in [a, b, cc].iter().enumerate() {
            pre.vv(2 + 2 * k as u32, dw(*x)[0]).vv(3 + 2 * k as u32, dw(*x)[1]);
        }
        pre.sv(106, vcc).sv(107, 0);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]);
        let r = a.mul_add(b, cc) * if vcc != 0 { 2f64.powi(64) } else { 1.0 };
        c.add("v_div_fmas_f64", &pre, &vop3(0x170, 10, [v(2), v(4), v(6)]), &post, &dw(r));
    }
    for &(a, b, cc) in &[(0xffff_ffffu32, 0xffff_ffffu32, 0xffff_ffff_ffff_ffffu64), (0x1234_5678, 0x9abc_def0, 0x1111_2222_3333_4444), (3, 5, 7)] {
        let wide = a as u128 * b as u128 + cc as u128;
        let mut pre = pr();
        pre.vv(2, a).vv(4, b).vv(6, cc as u32).vv(7, (cc >> 32) as u32).sv(20, 0x55);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 20);
        c.add("v_mad_u64_u32", &pre, &vop3b(0x176, 10, 20, [v(2), v(4), v(6)]), &post, &[wide as u32, (wide >> 32) as u32, (wide >> 64 != 0) as u32]);
        let sw = (a as i32 as i128) * (b as i32 as i128) + (cc as i64 as i128);
        c.add("v_mad_i64_i32", &pre, &vop3b(0x177, 10, 20, [v(2), v(4), v(6)]), &post, &[sw as u64 as u32, ((sw as u64) >> 32) as u32, (sw < 0) as u32]);
    }
    let gfx1036_sign_out: [(u32, u32, u64, bool); 10] = [
        (2, 3, (-10i64) as u64, true),
        (0xffff_fffe, 3, (-1i64) as u64, true),
        (0xffff_ffff, 0xffff_ffff, (-2i64) as u64, true),
        (0x8000_0000, 0x8000_0000, 0x7fff_ffff_ffff_ffff, false),
        (0x7fff_ffff, 0x7fff_ffff, 0x7fff_ffff_ffff_ffff, false),
        (0x8000_0000, 0x7fff_ffff, 0x8000_0000_0000_0000, true),
        (0x8000_0000, 1, 0x8000_0000, false),
        (5, 7, 0, false),
        (0, 0, 0x8000_0000_0000_0000, true),
        (0xffff_ffff, 1, 1, false),
    ];
    for &(a, b, cc, neg) in &gfx1036_sign_out {
        let sw = (a as i32 as i128) * (b as i32 as i128) + (cc as i64 as i128);
        assert_eq!(sw < 0, neg);
        let mut pre = pr();
        pre.vv(2, a).vv(4, b).vv(6, cc as u32).vv(7, (cc >> 32) as u32).sv(20, 0x55);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 20);
        c.add("v_mad_i64_i32", &pre, &vop3b(0x177, 10, 20, [v(2), v(4), v(6)]), &post, &[sw as u64 as u32, ((sw as u64) >> 32) as u32, neg as u32]);
    }
    for &(m, op, a, b, cc) in &[("v_mad_u64_u32", 0x176u32, 0xffff_ffffu32, 0xffff_ffffu32, 0xffff_ffff_ffff_ffffu64), ("v_mad_i64_i32", 0x177, 0x7fff_ffff, 0x7fff_ffff, 0x7fff_ffff_ffff_ffff)] {
        let mut pre = pr();
        pre.vv(2, a).vv(4, b).vv(6, cc as u32).vv(7, (cc >> 32) as u32).vv(10, 0xaaaa).vv(11, 0xbbbb).sv(20, 0x55).sv(126, 0);
        let mut post = pr();
        post.sv(126, 0xffff_ffff).raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 20);
        c.add(m, &pre, &vop3b(op, 10, 20, [v(2), v(4), v(6)]), &post, &[0xaaaa, 0xbbbb, 0]);
    }
    for &(a, b, d) in &[(0x1122_3344u32, 0x0066_0088u32, 5u32), (0xff00_ff00, 0x0101_0101, 0), (0x8080_8080, 0, 9)] {
        let s: u32 = (0..4).map(|k| { let (x, y) = ((a >> (8 * k)) & 0xff, (b >> (8 * k)) & 0xff); if y == 0 { 0 } else { (x as i32 - y as i32).unsigned_abs() } }).sum();
        v3c(&mut c, "v_msad_u8", 0x171, a, b, d, s + d);
    }
    let classes: [(u32, u32); 6] = [(fb(f32::NAN), 1 << 1), (0xff80_0001, 1 << 0), (fb(f32::NEG_INFINITY), 1 << 2), (fb(-1.0), 1 << 3), (0x8000_0001, 1 << 4), (fb(-0.0), 1 << 5)];
    for &(x, bit) in &classes {
        for mask in [bit, !bit & 0x3ff] {
            let mut pre = pr();
            pre.vv(1, x).vv(2, mask).vv(10, 0x1234);
            let mut post = pr();
            post.raw(&[sop1(3, 30, 126)]).raw(&[sop1(3, 126, ic(1))]).raw(&[vop1(1, 0, v(10))]).s2v(1, 30);
            let pass = (mask & bit != 0) as u32;
            c.add("v_cmpx_class_f32", &pre, &[vopc(0x98, v(1), 2), vop1(1, 10, ic(7))], &post, &[if pass != 0 { 7 } else { 0x1234 }, pass]);
        }
    }
    let dclasses: [(f64, u32); 5] = [(f64::NAN, 1 << 1), (f64::NEG_INFINITY, 1 << 2), (-2.0, 1 << 3), (-0.0, 1 << 5), (1e-310, 1 << 7)];
    for &(x, bit) in &dclasses {
        for mask in [bit, !bit & 0x3ff] {
            let mut pre = pr();
            pre.vv(2, dw(x)[0]).vv(3, dw(x)[1]).vv(4, mask).vv(10, 0x1234);
            let pass = (mask & bit != 0) as u32;
            let mut post = pr();
            post.vcc_to_v(0);
            c.add("v_cmp_class_f64", &pre, &[vopc(0xa8, v(2), 4)], &post, &[pass]);
            let mut post = pr();
            post.raw(&[sop1(3, 30, 126)]).raw(&[sop1(3, 126, ic(1))]).raw(&[vop1(1, 0, v(10))]).s2v(1, 30);
            c.add("v_cmpx_class_f64", &pre, &[vopc(0xb8, v(2), 4), vop1(1, 10, ic(7))], &post, &[if pass != 0 { 7 } else { 0x1234 }, pass]);
        }
    }
    c.run_both("f64_class");
}

#[test]
fn scalar_hwreg_and_sopp() {
    let mut c = Cases::new();
    let hw = |id: u32, off: u32, size: u32| (id | (off << 6) | ((size - 1) << 11)) as u16;
    for &(off, size, val) in &[(0u32, 4u32, 5u32), (4, 4, 0xa), (0, 8, 0xc3), (2, 3, 7)] {
        let mut pre = pr();
        pre.sv(10, val | 0xffff_ff00).sv(20, 0);
        let mut post = pr();
        post.raw(&[sopk(0x12, 21, hw(1, off, size))]).s2v(0, 21);
        c.add("s_setreg_b32", &pre, &[sopk(0x13, 10, hw(1, off, size))], &post, &[val & ((1 << size) - 1)]);
        let mut pre = pr();
        pre.sv(10, val).raw(&[sopk(0x13, 10, hw(1, 0, 8))]);
        let mut post = pr();
        post.s2v(0, 21);
        c.add("s_getreg_b32", &pre, &[sopk(0x12, 21, hw(1, off, size))], &post, &[(val >> off) & ((1 << size) - 1)]);
    }
    let nops: [(&'static str, u32, u16); 8] = [
        ("s_wakeup", 0x03, 0),
        ("s_sleep", 0x0e, 1),
        ("s_setprio", 0x0f, 3),
        ("s_icache_inv", 0x13, 0),
        ("s_incperflevel", 0x14, 1),
        ("s_decperflevel", 0x15, 1),
        ("s_clause", 0x21, 3),
        ("s_waitcnt_depctr", 0x23, 0xfffe),
    ];
    for (m, op, imm) in nops {
        for scc in [SCC0, SCC1] {
            let mut pre = pr();
            pre.vv(10, 0x1357_9bdf).sv(20, 0x2468_ace0).sv(106, 1).sv(107, 0).raw(&[scc]);
            let mut post = pr();
            post.raw(&[vop1(1, 0, v(10))]).s2v(1, 20).scc_to_v(2).vcc_to_v(3);
            c.add(m, &pre, &[sopp(op, imm)], &post, &[0x1357_9bdf, 0x2468_ace0, (scc == SCC1) as u32, 1]);
        }
    }
    c.run_both("hwreg_sopp");
    let mut p = Prog::new();
    p.vv(0, 0x1357_9bdf).vv(1, 2);
    let mut w = p.words();
    w.extend_from_slice(&[sopp(0x1f, 0), sopp(0x1f, 0), sopp(0x1f, 0)]);
    assert_eq!(&run(&w).unwrap()[..2], &[0x1357_9bdf, 2]);
    if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
        let _ = std::fs::write(std::path::Path::new(&dir).join("code_end.txt"), "s_code_end\n");
    }
}

#[test]
fn fma_single_rounding() {
    let mut c = Cases::new();
    let (p, q) = (1.0 + 2f32.powi(-23), 1.0 - 2f32.powi(-23));
    v3c(&mut c, "v_fma_f32", 0x14b, fb(p), fb(q), fb(-1.0), fb(p.mul_add(q, -1.0)));
    vc(&mut c, "v_fmac_f32", &[(10, fb(-1.0)), (1, fb(p)), (2, fb(q))], &[vop2(0x2b, 10, v(1), 2)], &[10], &[fb(p.mul_add(q, -1.0))]);
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..120 {
        let r = rnd();
        let a = f32::from_bits((r as u32 & 0x807f_ffff) | (((r >> 40) as u32 % 40 + 107) << 23));
        let b = f32::from_bits(((r >> 32) as u32 & 0x807f_ffff) | (((r >> 48) as u32 % 40 + 107) << 23));
        let r2 = rnd();
        let p = a * b;
        let cc = if i % 3 == 0 { -p * (1.0 + (r2 as u32 >> 9) as f32 * 2f32.powi(-30)) } else { f32::from_bits(r2 as u32 & 0x80ff_ffff | ((((p.to_bits() >> 23) & 0xff) as u32).saturating_sub((r2 >> 40) as u32 % 30).max(1) << 23)) };
        v3c(&mut c, "v_fma_f32", 0x14b, fb(a), fb(b), fb(cc), fb(a.mul_add(b, cc)));
    }
    let h = 1.0 + 2f32.powi(-12);
    for &(x, y, z) in &[(h, h, 2f32.powi(-80)), (h, h, -(2f32.powi(-80))), (h, -h, 2f32.powi(-80)), (3.0, 1e-45, 0.0), (1e38, 10.0, -1e38), (f32::INFINITY, 0.0, 1.0), (1e20, 1e20, f32::NEG_INFINITY)] {
        v3c(&mut c, "v_fma_f32", 0x14b, fb(x), fb(y), fb(z), vop::fm(x, y, z));
    }
    for &(a, k, b) in &[(2.0f32, 3.0f32, 1.0f32), (p, q, -1.0)] {
        vc(&mut c, "v_fmamk_f32", &[(1, fb(a)), (2, fb(b))], &[vop2(0x2c, 10, v(1), 2), fb(k)], &[10], &[fb(a.mul_add(k, b))]);
    }
    c.run_both("fma");
}

pub fn smem(op: u32, sdata: u32, sbase: u32, imm: u32, soff: u32) -> [u32; 2] {
    [0xf400_0000 | (op << 18) | (sdata << 6) | (sbase >> 1), (soff << 25) | (imm & 0x1f_ffff)]
}

const SNULL: u32 = 0x7d;

fn sopp_state(c: &mut Cases, m: &'static str, op: &[u32]) {
    for scc in [SCC0, SCC1] {
        let mut pre = pr();
        pre.vv(10, 0x1357_9bdf).sv(20, 0x2468_ace0).sv(106, 1).sv(107, 0).raw(&[scc]);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10))]).s2v(1, 20).scc_to_v(2).vcc_to_v(3);
        c.add(m, &pre, op, &post, &[0x1357_9bdf, 0x2468_ace0, (scc == SCC1) as u32, 1]);
    }
}

#[test]
fn scalar_sopp_control() {
    let mut c = Cases::new();
    let nops: [(&'static str, u32, u16); 16] = [
        ("s_nop", 0x00, 0),
        ("s_nop", 0x00, 7),
        ("s_setkill", 0x0b, 1),
        ("s_waitcnt", 0x0c, 0),
        ("s_waitcnt", 0x0c, 0xc07f),
        ("s_barrier", 0x0a, 0),
        ("s_sethalt", 0x0d, 0),
        ("s_ttracedata", 0x16, 0),
        ("s_ttracedata_imm", 0x28, 0x1234),
        ("s_wait_idle", 0x22, 0),
        ("s_inst_prefetch", 0x20, 1),
        ("s_sendmsg", 0x10, 1),
        ("s_sendmsg", 0x10, 4),
        ("s_sendmsg", 0x10, 5),
        ("s_sendmsg", 0x10, 7),
        ("s_sendmsg", 0x10, 8),
    ];
    for (m, op, imm) in nops {
        sopp_state(&mut c, m, &[sopp(op, imm)]);
    }
    for (m, op) in [("s_cbranch_cdbgsys", 0x17), ("s_cbranch_cdbguser", 0x18), ("s_cbranch_cdbgsys_or_user", 0x19), ("s_cbranch_cdbgsys_and_user", 0x1a)] {
        let mut pre = pr();
        pre.vv(10, 0x1111);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10))]);
        c.add(m, &pre, &[sopp(op, 1), vop1(1, 10, ic(7))], &post, &[7]);
    }
    let br = |c: &mut Cases, m: &'static str, op: u32, setup: &Prog, taken: bool| {
        let mut pre = pr();
        pre.vv(10, 0x1111).sv(20, 0x2222).raw(&setup.w);
        let mut post = pr();
        post.sv(126, 0xffff_ffff).raw(&[vop1(1, 0, v(10))]).s2v(1, 20);
        c.add(m, &pre, &[sopp(op, 2), vop1(1, 10, ic(7)), sop1(3, 20, ic(9))], &post, &if taken { [0x1111, 0x2222] } else { [7, 9] });
    };
    let scc = |b: bool| {
        let mut p = pr();
        p.raw(&[if b { SCC1 } else { SCC0 }]);
        p
    };
    let vcc = |lo: u32| {
        let mut p = pr();
        p.sv(106, lo).sv(107, 0);
        p
    };
    br(&mut c, "s_branch", 0x02, &scc(false), true);
    br(&mut c, "s_branch", 0x02, &scc(true), true);
    br(&mut c, "s_cbranch_scc0", 0x04, &scc(false), true);
    br(&mut c, "s_cbranch_scc0", 0x04, &scc(true), false);
    br(&mut c, "s_cbranch_scc1", 0x05, &scc(true), true);
    br(&mut c, "s_cbranch_scc1", 0x05, &scc(false), false);
    br(&mut c, "s_cbranch_vccz", 0x06, &vcc(0), true);
    br(&mut c, "s_cbranch_vccz", 0x06, &vcc(1), false);
    br(&mut c, "s_cbranch_vccnz", 0x07, &vcc(1), true);
    br(&mut c, "s_cbranch_vccnz", 0x07, &vcc(0), false);
    let mut ex0 = pr();
    ex0.sv(126, 0);
    let mut ex1 = pr();
    ex1.sv(126, 1);
    let brx = |c: &mut Cases, m: &'static str, op: u32, setup: &Prog, taken: bool| {
        let mut pre = pr();
        pre.sv(20, 0x2222).raw(&setup.w);
        let mut post = pr();
        post.sv(126, 0xffff_ffff).s2v(0, 20);
        c.add(m, &pre, &[sopp(op, 1), sop1(3, 20, ic(9))], &post, &[if taken { 0x2222 } else { 9 }]);
    };
    brx(&mut c, "s_cbranch_execz", 0x08, &ex0, true);
    brx(&mut c, "s_cbranch_execz", 0x08, &ex1, false);
    brx(&mut c, "s_cbranch_execnz", 0x09, &ex1, true);
    brx(&mut c, "s_cbranch_execnz", 0x09, &ex0, false);
    {
        let mut pre = pr();
        pre.sv(20, 0).sv(21, 0).raw(&[sopk(0x0f, 20, 1), sopk(0x0f, 21, 3), sopc(0x04, 20, ic(5))]);
        let mut post = pr();
        post.s2v(0, 20).s2v(1, 21);
        c.add("s_cbranch_scc1", &pre, &[sopp(0x05, 0xfffc)], &post, &[5, 15]);
    }
    let hw = |id: u32, off: u32, size: u32| (id | (off << 6) | ((size - 1) << 11)) as u16;
    for &(m, op, imm, want) in &[
        ("s_round_mode", 0x24u32, 0x5u16, 0xf5u32),
        ("s_round_mode", 0x24, 0xa, 0xfa),
        ("s_round_mode", 0x24, 0xff3, 0xf3),
        ("s_denorm_mode", 0x25, 0x3, 0x30),
        ("s_denorm_mode", 0x25, 0xc, 0xc0),
        ("s_denorm_mode", 0x25, 0xf0, 0x00),
    ] {
        let mut post = pr();
        post.raw(&[sopk(0x12, 21, hw(1, 0, 8))]).s2v(0, 21);
        c.add(m, &pr(), &[sopp(op, imm)], &post, &[want]);
    }
    {
        let mut post = pr();
        post.raw(&[sopk(0x12, 21, hw(1, 0, 8))]).s2v(0, 21);
        c.add("s_round_mode", &pr(), &[sopp(0x24, 0x6), sopp(0x25, 0x9)], &post, &[0x96]);
    }
    for (m, op) in [("s_gl1_inv", 0x1fu32), ("s_dcache_inv", 0x20), ("s_dcache_wb", 0x21)] {
        sopp_state(&mut c, m, &smem(op, 0, 0, 0, 0));
    }
    for (m, op) in [("s_atc_probe", 0x26u32), ("s_atc_probe_buffer", 0x27)] {
        sopp_state(&mut c, m, &smem(op, 7, 0, 0x40, SNULL));
    }
    for (m, op, step) in [("s_memtime", 0x24u32, 16u32), ("s_memrealtime", 0x25, 1)] {
        let mut post = pr();
        post.s2v(0, 20).s2v(1, 21).s2v(2, 22).s2v(3, 23);
        let mut op2 = smem(op, 20, 0, 0, SNULL).to_vec();
        op2.extend_from_slice(&smem(op, 22, 0, 0, SNULL));
        c.add(m, &pr(), &op2, &post, &[step, 0, 2 * step, 0]);
    }
    c.run_both("sopp_control");
    let ends: [(&'static str, u32); 3] = [("s_endpgm", 0x01), ("s_endpgm_saved", 0x1b), ("s_endpgm_ordered_ps_done", 0x1e)];
    let mut ok = Vec::new();
    for (m, op) in ends {
        let isa: Vec<u8> = sopp(op, 0).to_le_bytes().to_vec();
        assert_eq!(tombox_xshader::rdna2::disassemble(&isa)[0].mnemonic, m);
        for skip in [false, true] {
            let mut p = Prog::new();
            p.vv(0, 0x1357_9bdf).vv(1, 2).sv(20, skip as u32).raw(&[sopc(0x06, 20, ic(1)), sopp(0x05, 1), sopp(op, 0)]).vv(0, 0x4444);
            let r = run(&p.words()).unwrap_or_else(|e| panic!("{m}: {e}"));
            if skip {
                assert_eq!(r[..2], [0x4444, 2], "{m} skipped");
            } else {
                assert_ne!(r[0], 0x4444, "{m}: instructions after an executed {m} ran");
            }
        }
        ok.push(m);
    }
    if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
        let _ = std::fs::write(std::path::Path::new(&dir).join("sopp_endpgm.txt"), ok.join("\n") + "\n");
    }
}

#[test]
fn scalar_buffer_stores() {
    let vals = [0x1122_3344u32, 0x5566_7788, 0x99aa_bbcc, 0xddee_ff00];
    let mut cs = Vec::new();
    for (m, op, n) in [("s_buffer_store_dword", 0x18u32, 1u32), ("s_buffer_store_dwordx2", 0x19, 2), ("s_buffer_store_dwordx4", 0x1a, 4)] {
        for (imm, soff) in [(0u32, SNULL), (4, SNULL), (8, SNULL), (0x40, SNULL), (4, 20)] {
            let byte = if soff == SNULL { imm } else { imm + 12 } as usize;
            let w = byte / 4;
            let mut pre = Prog::new();
            for k in 0..n {
                pre.sv(10 + k, vals[k as usize]);
            }
            if soff != SNULL {
                pre.sv(soff, 12);
            }
            let mut mem: Vec<(usize, u32)> = (0..n as usize).map(|k| (w + k, vals[k])).collect();
            mem.push((w + n as usize, *BUF.get(w + n as usize).unwrap_or(&0)));
            cs.push(BufCase { m, pre: pre.w, op: smem(op, 10, 0, imm, soff), rt: vec![], mem });
        }
    }
    run_buf_cases("smem_buffer_store", &cs);
}

#[test]
fn scalar_smem_store_refusals() {
    let root = tombox_xshader::meta::RootSig { version: 2, flags: 1, params: vec![rtable(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() };
    for (m, op) in [("s_store_dword", 0x10u32), ("s_store_dwordx2", 0x11), ("s_store_dwordx4", 0x12)] {
        let mut w = Prog::new();
        w.sv(10, 0x1234).sv(11, 0x5678).raw(&smem(op, 10, 8, 0x10, SNULL));
        let isa: Vec<u8> = w.w.iter().flat_map(|x| x.to_le_bytes()).collect();
        assert!(tombox_xshader::rdna2::disassemble(&isa).iter().any(|i| i.mnemonic == m));
        let e = translate(&shader_root(&w.words(), root.clone())).err().unwrap_or_else(|| panic!("{m}: translated"));
        assert!(e.contains("stores through a 64-bit scalar address (SBASE)"), "{m}: {e}");
    }
    let mut w = Prog::new();
    w.sv(10, 1).raw(&smem(0x18, 10, 50, 0, SNULL));
    let e = translate(&shader_root(&w.words(), root.clone())).err().expect("s_buffer_store from an unmatched base");
    assert!(e.contains("is not a buffer descriptor"), "{e}");
    let mixed = tombox_xshader::meta::RootSig {
        version: 2,
        flags: 1,
        params: vec![tombox_xshader::meta::RootParam {
            visibility: 5,
            kind: tombox_xshader::meta::RootParamKind::Descriptor { kind: 2, register: 0, space: 0 },
        }],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    };
    let mut w = Prog::new();
    w.sv(10, 1).raw(&smem(0x19, 10, 8, 0, SNULL));
    let e = translate(&shader_root(&w.words(), mixed)).err().expect("s_buffer_store through a constant-buffer root descriptor");
    assert!(e.contains("is not writable"), "{e}");
}

fn smem_data() -> Vec<u32> {
    (0..256u32).map(|i| 0xa500_0000 | (i * 0x0101) ).collect()
}

#[test]
fn scalar_smem_loads() {
    let root = tombox_xshader::meta::RootSig { version: 2, flags: 1, params: vec![rtable(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() };
    let data = smem_data();
    let word = |slot: u32, byte: u32| data[(slot * 32 + byte / 4) as usize];
    struct C {
        m: &'static str,
        w: Vec<u32>,
        out: Vec<u32>,
        want: Vec<u32>,
    }
    let mut cs: Vec<C> = Vec::new();
    let load_v0 = smem(0x02, 0, 8, 0, SNULL);
    for (m, op, n) in [("s_buffer_load_dword", 0x08u32, 1u32), ("s_buffer_load_dwordx2", 0x09, 2), ("s_buffer_load_dwordx4", 0x0a, 4), ("s_buffer_load_dwordx8", 0x0b, 8), ("s_buffer_load_dwordx16", 0x0c, 16)] {
        for imm in [0u32, 8, 0x40] {
            for j in 0..n.div_ceil(4) {
                let mut w = load_v0.to_vec();
                w.extend_from_slice(&smem(op, 20, 0, imm, SNULL));
                let regs: Vec<u32> = (0..n.min(4)).map(|k| 20 + 4 * j + k).collect();
                let want = regs.iter().map(|&r| word(0, imm + 4 * (r - 20))).collect();
                cs.push(C { m, w, out: regs, want });
            }
        }
        let mut w = Prog::new();
        w.sv(12, 12);
        let mut w = w.w;
        w.extend_from_slice(&load_v0);
        w.extend_from_slice(&smem(op, 20, 0, 4, 12));
        let regs: Vec<u32> = (0..n.min(4)).map(|k| 20 + k).collect();
        let want = regs.iter().map(|&r| word(0, 16 + 4 * (r - 20))).collect();
        cs.push(C { m, w, out: regs, want });
    }
    for &(so, imm, n, byte) in &[(0xeu32, 2i32, 1u32, 0xcu32), (3, 3, 1, 0), (2, 6, 1, 4), (8, -4, 1, 4), (0xb, -1, 1, 4), (0x10, -8, 2, 8)] {
        let mut w = Prog::new();
        w.sv(12, so);
        let mut w = w.w;
        w.extend_from_slice(&load_v0);
        w.extend_from_slice(&smem(if n == 2 { 0x09 } else { 0x08 }, 20, 0, imm as u32, 12));
        let regs: Vec<u32> = (0..n).map(|k| 20 + k).collect();
        let want = regs.iter().map(|&r| word(0, byte + 4 * (r - 20))).collect();
        cs.push(C { m: if n == 2 { "s_buffer_load_dwordx2" } else { "s_buffer_load_dword" }, w, out: regs, want });
    }
    {
        let mut w = load_v0.to_vec();
        w.extend_from_slice(&smem(0x08, 20, 0, 0x13, SNULL));
        cs.push(C { m: "s_buffer_load_dword", w, out: vec![20], want: vec![word(0, 0x10)] });
    }
    let probe = |base: u32| smem(0x08, 30, base, 0x24, SNULL);
    let mut w = Vec::new();
    for k in 0..4u32 {
        w.extend_from_slice(&smem(0x00, k, 8, 32 + 4 * k, SNULL));
    }
    w.extend_from_slice(&probe(0));
    cs.push(C { m: "s_load_dword", w, out: vec![30], want: vec![word(1, 0x24)] });
    let mut w = smem(0x01, 0, 8, 64, SNULL).to_vec();
    w.extend_from_slice(&smem(0x01, 2, 8, 72, SNULL));
    w.extend_from_slice(&probe(0));
    cs.push(C { m: "s_load_dwordx2", w, out: vec![30], want: vec![word(2, 0x24)] });
    for slot in [0u32, 6] {
        let mut w = smem(0x02, 4, 8, 32 * slot, SNULL).to_vec();
        w.extend_from_slice(&probe(4));
        cs.push(C { m: "s_load_dwordx4", w, out: vec![30], want: vec![word(slot, 0x24)] });
    }
    let mut w = smem(0x03, 0, 8, 96, SNULL).to_vec();
    w.extend_from_slice(&probe(0));
    cs.push(C { m: "s_load_dwordx8", w, out: vec![30], want: vec![word(3, 0x24)] });
    for (k, slot) in [(0u32, 4u32), (8, 5)] {
        let mut w = smem(0x04, 0, 8, 128, SNULL).to_vec();
        w.extend_from_slice(&probe(k));
        cs.push(C { m: "s_load_dwordx16", w, out: vec![30], want: vec![word(slot, 0x24)] });
    }
    {
        let mut w = smem(0x04, 0, 8, 128, SNULL).to_vec();
        w.extend_from_slice(&smem(0x08, 30, 0, 0x24, SNULL));
        w.extend_from_slice(&smem(0x08, 31, 8, 0x28, SNULL));
        cs.push(C { m: "s_load_dwordx16", w, out: vec![30, 31], want: vec![word(4, 0x24), word(5, 0x28)] });
    }
    let mut fails = Vec::new();
    let mut all: std::collections::BTreeSet<&str> = Default::default();
    let mut bad: std::collections::BTreeSet<&str> = Default::default();
    for (i, c) in cs.iter().enumerate() {
        all.insert(c.m);
        let mut p = Prog::new();
        p.raw(&c.w);
        for (k, &r) in c.out.iter().enumerate() {
            p.s2v(k as u32, r);
        }
        let isa: Vec<u8> = c.w.iter().flat_map(|x| x.to_le_bytes()).collect();
        if !tombox_xshader::rdna2::disassemble(&isa).iter().any(|x| x.mnemonic == c.m) {
            fails.push(format!("#{i} {}: encoding does not decode", c.m));
            bad.insert(c.m);
            continue;
        }
        match run_res(&p.words(), root.clone(), Some(&data), None, sampler(0, 3, 0)) {
            Ok(o) if o.rt[..c.want.len()] == c.want[..] => {}
            Ok(o) => {
                let h = translate(&shader_root(&p.words(), root.clone())).map(|t| t.hlsl).unwrap_or_default();
                let body: String = h.lines().filter(|l| l.starts_with("    ") || l.contains("ubuf")).collect::<Vec<_>>().join("\n");
                fails.push(format!("#{i} {}: got {:08x?} want {:08x?}\n{body}", c.m, &o.rt[..c.want.len()], c.want));
                bad.insert(c.m);
            }
            Err(e) => {
                fails.push(format!("#{i} {}: {}", c.m, e.lines().take(4).collect::<Vec<_>>().join(" | ")));
                bad.insert(c.m);
            }
        }
    }
    let ok: Vec<&str> = all.iter().filter(|m| !bad.contains(*m)).copied().collect();
    if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::Path::new(&dir).join("smem.txt"), ok.join("\n") + "\n");
    }
    println!("smem: {} mnemonics verified, {} failing, {} cases", ok.len(), bad.len(), cs.len());
    assert!(fails.is_empty(), "smem: {} failing\n{}", fails.len(), fails.join("\n\n"));
}

#[test]
fn harness_moves_constants() {
    let mut p = Prog::new();
    p.vv(0, 0x1234_5678).vv(1, 0xffff_ffff).sv(5, 0x8000_0001).s2v(2, 5).vv(3, 0x7fc0_0001);
    let r = run(&p.words()).unwrap();
    assert_eq!(r, [0x1234_5678, 0xffff_ffff, 0x8000_0001, 0x7fc0_0001]);
}

fn f32_prev(b: u32) -> u32 {
    if b & 0x7fff_ffff == 0 { 0x8000_0001 } else if b >> 31 != 0 { b + 1 } else { b - 1 }
}

fn f32_next(b: u32) -> u32 {
    if b & 0x7fff_ffff == 0 { 1 } else if b >> 31 != 0 { b - 1 } else { b + 1 }
}

fn round_dir(x: f64, mode: u32) -> u32 {
    let r = x as f32;
    if mode == 0 || r as f64 == x {
        return r.to_bits();
    }
    let rb = r.to_bits();
    let (lo, hi) = if r as f64 > x { (f32_prev(rb), rb) } else { (rb, f32_next(rb)) };
    match mode {
        1 => hi,
        2 => lo,
        _ => if x > 0.0 { lo } else { hi },
    }
}

#[test]
fn scalar_mode_effects() {
    let mut c = Cases::new();
    let rm = |mode: u32| sopp(0x24, mode as u16);
    let op2 = |c: &mut Cases, m: &'static str, w: &[u32], a: u32, b: u32, want: u32| {
        vc(c, m, &[(1, a), (2, b)], w, &[10], &[want]);
    };
    let p1 = fb(1.0 + 2f32.powi(-23));
    let cases: [(u32, u32, [u32; 4]); 6] = [
        (fb(1.0), fb(2f32.powi(-25)), [0x3f80_0000, 0x3f80_0001, 0x3f80_0000, 0x3f80_0000]),
        (fb(-1.0), fb(-(2f32.powi(-25))), [0xbf80_0000, 0xbf80_0000, 0xbf80_0001, 0xbf80_0000]),
        (fb(1.0), fb(3.0 * 2f32.powi(-25)), [0x3f80_0001, 0x3f80_0001, 0x3f80_0000, 0x3f80_0000]),
        (fb(1.0), fb(-1.0), [0, 0, 0x8000_0000, 0]),
        (fb(f32::MAX), fb(f32::MAX), [0x7f80_0000, 0x7f80_0000, 0x7f7f_ffff, 0x7f7f_ffff]),
        (fb(-f32::MAX), fb(-f32::MAX), [0xff80_0000, 0xff7f_ffff, 0xff80_0000, 0xff7f_ffff]),
    ];
    for &(a, b, want) in &cases {
        for mode in 0..4u32 {
            op2(&mut c, "s_round_mode", &[rm(mode), vop2(0x03, 10, v(1), 2)], a, b, want[mode as usize]);
            op2(&mut c, "s_round_mode", &[rm(mode), vop2(0x04, 10, v(1), 2)], a, b ^ 0x8000_0000, want[mode as usize]);
            op2(&mut c, "s_round_mode", &[rm(mode), vop2(0x05, 10, v(2), 1)], a, b ^ 0x8000_0000, want[mode as usize]);
        }
    }
    let mulc: [(u32, u32, [u32; 4]); 3] = [
        (p1, p1, [0x3f80_0002, 0x3f80_0003, 0x3f80_0002, 0x3f80_0002]),
        (p1, p1 ^ 0x8000_0000, [0xbf80_0002, 0xbf80_0002, 0xbf80_0003, 0xbf80_0002]),
        (fb(f32::MAX), fb(2.0), [0x7f80_0000, 0x7f80_0000, 0x7f7f_ffff, 0x7f7f_ffff]),
    ];
    for &(a, b, want) in &mulc {
        for mode in 0..4u32 {
            op2(&mut c, "s_round_mode", &[rm(mode), vop2(0x08, 10, v(1), 2)], a, b, want[mode as usize]);
        }
    }
    let fmac: [(u32, u32, u32, [u32; 4]); 3] = [
        (p1, p1, fb(-1.0), [0x3480_0000, 0x3480_0001, 0x3480_0000, 0x3480_0000]),
        (p1, p1 ^ 0x8000_0000, fb(1.0), [0xb480_0000, 0xb480_0000, 0xb480_0001, 0xb480_0000]),
        (fb(2.0), fb(3.0), fb(-6.0), [0, 0, 0x8000_0000, 0]),
    ];
    for &(a, b, cc, want) in &fmac {
        for mode in 0..4u32 {
            vc(&mut c, "s_round_mode", &[(1, a), (2, b), (3, cc)], &[rm(mode), vop3(0x14b, 10, [v(1), v(2), v(3)])[0], vop3(0x14b, 10, [v(1), v(2), v(3)])[1]], &[10], &[want[mode as usize]]);
            vc(&mut c, "s_round_mode", &[(1, a), (2, b), (10, cc)], &[rm(mode), vop2(0x2b, 10, v(1), 2)], &[10], &[want[mode as usize]]);
        }
    }
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..24 {
        let r = rnd();
        let a = f32::from_bits((r as u32 & 0x807f_ffff) | ((((r >> 40) as u32 % 16) + 120) << 23));
        let b = f32::from_bits(((r >> 32) as u32 & 0x807f_ffff) | ((((r >> 48) as u32 % 16) + 120) << 23));
        for mode in 1..4u32 {
            op2(&mut c, "s_round_mode", &[rm(mode), vop2(0x03, 10, v(1), 2)], fb(a), fb(b), round_dir(a as f64 + b as f64, mode));
            op2(&mut c, "s_round_mode", &[rm(mode), vop2(0x08, 10, v(1), 2)], fb(a), fb(b), round_dir(a as f64 * b as f64, mode));
        }
    }
    for &(x, want) in &[(0xffff_ffffu32, [0x4f80_0000u32, 0x4f80_0000, 0x4f7f_ffff, 0x4f7f_ffff]), (0x0100_0001, [0x4b80_0000, 0x4b80_0001, 0x4b80_0000, 0x4b80_0000])] {
        for mode in 0..4u32 {
            vc(&mut c, "s_round_mode", &[(1, x)], &[rm(mode), vop1(0x06, 10, v(1))], &[10], &[want[mode as usize]]);
        }
    }
    for &(x, want) in &[((-16_777_217i32) as u32, [0xcb80_0000u32, 0xcb80_0000, 0xcb80_0001, 0xcb80_0000]), (16_777_219, [0x4b80_0002, 0x4b80_0002, 0x4b80_0001, 0x4b80_0001])] {
        for mode in 0..4u32 {
            vc(&mut c, "s_round_mode", &[(1, x)], &[rm(mode), vop1(0x05, 10, v(1))], &[10], &[want[mode as usize]]);
        }
    }
    {
        let (a, b) = (fb(1.0), fb(2f32.powi(-25)));
        vc(&mut c, "s_round_mode", &[(1, a), (2, b)], &[rm(1), vop2(0x03, 10, v(1), 2), rm(0), vop2(0x03, 11, v(1), 2)], &[10, 11], &[0x3f80_0001, 0x3f80_0000]);
        vc(&mut c, "s_round_mode", &[(1, a), (2, b)], &[rm(0), vop2(0x03, 10, v(1), 2), rm(1), vop2(0x03, 11, v(1), 2)], &[10, 11], &[0x3f80_0000, 0x3f80_0001]);
    }
    let hw = |id: u32, off: u32, size: u32| (id | (off << 6) | ((size - 1) << 11)) as u16;
    for (val, want) in [(0u32, 0x3f80_0002u32), (1, 0x3f80_0002), (2, 0x3f80_0001), (3, 0x3f80_0001)] {
        let (a, b) = (fb(1.0), fb(1.5 * 2f32.powi(-23)));
        vc(&mut c, "s_setreg_imm32_b32", &[(1, a), (2, b)], &[sopk(0x15, 0, hw(1, 0, 2)), val, vop2(0x03, 10, v(1), 2)], &[10], &[want]);
    }
    let tiny = fb(2f32.powi(-70));
    let den = 0x0000_0200u32;
    let big = fb(2f32.powi(20));
    vc(&mut c, "v_mul_f32", &[(1, tiny), (2, tiny)], &[vop2(0x08, 10, v(1), 2)], &[10], &[den]);
    vc(&mut c, "v_mul_f32", &[(1, den), (2, big)], &[vop2(0x08, 10, v(1), 2)], &[10], &[0x0380_0000]);
    for (imm, mul_out, mul_in, add_in) in [(0xcu16, 0u32, 0u32, 0u32), (0xd, 0, 0x0380_0000, 0), (0xe, den, 0, 0), (0xf, den, 0x0380_0000, den)] {
        vc(&mut c, "s_denorm_mode", &[(1, tiny), (2, tiny)], &[sopp(0x25, imm), vop2(0x08, 10, v(1), 2)], &[10], &[mul_out]);
        vc(&mut c, "s_denorm_mode", &[(1, tiny ^ 0x8000_0000), (2, tiny)], &[sopp(0x25, imm), vop2(0x08, 10, v(1), 2)], &[10], &[mul_out | 0x8000_0000]);
        vc(&mut c, "s_denorm_mode", &[(1, den), (2, big)], &[sopp(0x25, imm), vop2(0x08, 10, v(1), 2)], &[10], &[mul_in]);
        vc(&mut c, "s_denorm_mode", &[(1, den), (2, 0)], &[sopp(0x25, imm), vop2(0x03, 10, v(1), 2)], &[10], &[add_in]);
    }
    vc(&mut c, "s_denorm_mode", &[(1, tiny), (2, tiny)], &[sopp(0x25, 0xc), vop2(0x08, 10, v(1), 2), sopp(0x25, 0xf), vop2(0x08, 11, v(1), 2)], &[10, 11], &[0, den]);
    let fr = |x: u32| f32::from_bits(x) as f64;
    let mad_ref = |a: u32, b: u32, cc: u32, mode: u32| round_dir(fr(round_dir(fr(a) * fr(b), mode)) + fr(cc), mode);
    let fma_ref = |a: u32, b: u32, cc: u32, mode: u32| round_dir(fr(a) * fr(b) + fr(cc), mode);
    let m1 = fb(-1.0);
    for mode in 0..4u32 {
        let (mad, fma) = (mad_ref(p1, p1, m1, mode), fma_ref(p1, p1, m1, mode));
        let ops: [(&[u32], &[(u32, u32)], u32); 7] = [
            (&[vop2(0x1f, 10, v(1), 2)], &[(1, p1), (2, p1), (10, m1)], mad),
            (&[vop3(0x141, 10, [v(1), v(2), v(3)])[0], vop3(0x141, 10, [v(1), v(2), v(3)])[1]], &[(1, p1), (2, p1), (3, m1)], mad),
            (&[vop2(0x20, 10, v(1), 2), p1], &[(1, p1), (2, m1)], mad),
            (&[vop2(0x21, 10, v(1), 2), m1], &[(1, p1), (2, p1)], mad),
            (&[vop2(0x2c, 10, v(1), 2), p1], &[(1, p1), (2, m1)], fma),
            (&[vop2(0x2d, 10, v(1), 2), m1], &[(1, p1), (2, p1)], fma),
            (&[vop2(0x07, 10, v(1), 2)], &[(1, 0), (2, fb(f32::INFINITY))], 0),
        ];
        for (k, (w, ins, want)) in ops.iter().enumerate() {
            let isa: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
            let want_m = ["v_mac_f32", "v_mad_f32", "v_madmk_f32", "v_madak_f32", "v_fmamk_f32", "v_fmaak_f32", "v_mul_legacy_f32"][k];
            assert_eq!(tombox_xshader::rdna2::disassemble(&isa)[0].mnemonic, want_m);
            let mut op = vec![rm(mode)];
            op.extend_from_slice(w);
            vc(&mut c, "s_round_mode", ins, &op, &[10], &[*want]);
        }
        vc(&mut c, "s_round_mode", &[(1, 0), (2, fb(f32::INFINITY)), (10, fb(1.5))], &[rm(mode), vop2(0x06, 10, v(1), 2)], &[10], &[fb(1.5)]);
    }
    assert_ne!(mad_ref(p1, p1, m1, 1), fma_ref(p1, p1, m1, 1));
    for (imm, want) in [(0xeu16, 0x8000_0000u32), (0xd, 0xbf80_0000)] {
        vc(&mut c, "s_denorm_mode", &[(1, 0x8000_0001)], &[sopp(0x25, imm), vop1(0x24, 10, v(1))], &[10], &[want]);
    }
    for (imm, want) in [(0xeu16, 0x8000_0000u32), (0xd, 0x8000_0000), (0xf, 0x8000_0001)] {
        vc(&mut c, "s_denorm_mode", &[(1, 0x8000_0001), (2, fb(-0.5))], &[sopp(0x25, imm), vop2(0x10, 10, v(1), 2)], &[10], &[want]);
    }
    c.run_both("mode_effects");
    let mut p = Prog::new();
    p.vv(1, 0x3f80_0800).vv(10, 0).raw(&[sopp(0x24, 0x5), vop1(0x0a, 10, v(1))]).raw(&[vop1(1, 0, v(10))]);
    assert_eq!(run(&p.words()).unwrap()[0], 0x3c01);
}

#[test]
fn f16_omod_follows_mode() {
    let mut c = Cases::new();
    let hw = |id: u32, off: u32, size: u32| (id | (off << 6) | ((size - 1) << 11)) as u16;
    let flush16 = [sopk(0x15, 0, hw(1, 6, 2)), 0];
    let om = |op: u32, a: u32, b: u32, omod: u32, clamp: bool| {
        let mut w = vop3m(op, 12, [v(a), v(b), 0], 0, 0, 0, clamp);
        w[1] |= omod << 27;
        w
    };
    let ins = [(1, 0x3a00), (2, 0), (3, 0x8000), (4, 0x3e00), (5, 0x3c00)];
    let cases: [(u32, u32, u32, u32, bool, u32, u32); 6] = [
        (0x132, 1, 2, 2, false, 0x3a00, 0x4200),
        (0x132, 1, 2, 3, false, 0x3a00, 0x3600),
        (0x132, 3, 3, 1, false, 0x8000, 0x0000),
        (0x132, 1, 2, 1, true, 0x3a00, 0x3c00),
        (0x135, 4, 5, 1, false, 0x3e00, 0x4200),
        (0x135, 4, 5, 3, true, 0x3c00, 0x3a00),
    ];
    for (op, a, b, omod, clamp, keep, flushed) in cases {
        let w = om(op, a, b, omod, clamp);
        vc(&mut c, if op == 0x132 { "v_add_f16" } else { "v_mul_f16" }, &ins, &w, &[12], &[keep]);
        let mut p = flush16.to_vec();
        p.extend_from_slice(&w);
        vc(&mut c, "s_setreg_imm32_b32", &ins, &p, &[12], &[flushed]);
        let mut p = vec![sopk(0x15, 0, hw(1, 6, 2)), 1];
        p.extend_from_slice(&w);
        vc(&mut c, "s_setreg_imm32_b32", &ins, &p, &[12], &[flushed]);
        let mut p = vec![sopk(0x15, 0, hw(1, 6, 2)), 2];
        p.extend_from_slice(&w);
        vc(&mut c, "s_setreg_imm32_b32", &ins, &p, &[12], &[keep]);
    }
    let tiny = [(1, 0x0400), (2, 0x3c00), (3, 0x8400), (4, 0x07ff)];
    for (a, omod, keep, flushed) in [(1u32, 3u32, 0x0400u32, 0x0000u32), (3, 3, 0x8400, 0x8000), (4, 3, 0x07ff, 0x0000), (1, 1, 0x0400, 0x0800)] {
        let w = om(0x135, a, 2, omod, false);
        vc(&mut c, "v_mul_f16", &tiny, &w, &[12], &[keep]);
        let mut p = flush16.to_vec();
        p.extend_from_slice(&w);
        vc(&mut c, "s_setreg_imm32_b32", &tiny, &p, &[12], &[flushed]);
    }
    c.run_both("f16_omod_follows_mode");
}

fn run_rsrc1(words: &[u32], rsrc1: u32) -> Result<[u32; 4], String> {
    let t = tombox_xshader::hlsl::translate_with_pgm_rsrc1(&shader(words), None, rsrc1)?;
    if !t.unknowns.is_empty() {
        return Err(format!("unknowns reported with a known PGM_RSRC1: {:?}", t.unknowns));
    }
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}
{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { draw(&mut c, &code) }.map_err(|e| format!("{e}
{}", t.hlsl))
}

#[test]
fn f16_omod_follows_initial_mode_from_pgm_rsrc1() {
    let hw = |id: u32, off: u32, size: u32| (id | (off << 6) | ((size - 1) << 11)) as u16;
    let om = |op: u32, a: u32, b: u32, omod: u32, clamp: bool| {
        let mut w = vop3m(op, 12, [v(a), v(b), 0], 0, 0, 0, clamp);
        w[1] |= omod << 27;
        w
    };
    let ins = [(1, 0x3a00), (2, 0), (3, 0x8000), (4, 0x3e00), (5, 0x3c00)];
    let prog = |ins: &[(u32, u32)], pre: &[u32], op: &[u32]| -> Vec<u32> {
        let mut p = Prog::new();
        p.vv(10, D16).vv(11, 0x5a5a_5a5a);
        for &(r, val) in ins {
            p.vv(r, val);
        }
        p.raw(pre).raw(op).raw(&[vop1(1, 0, v(12))]);
        p.words()
    };
    let keep16 = 0xf0u32 << 12;
    let flush16 = 0x30u32 << 12;
    let rsrc: [(u32, bool); 7] = [
        (keep16, false),
        (flush16, true),
        (flush16 | (1 << 23), false),
        (flush16 | (1 << 21), true),
        (flush16 | (1 << 20) | (1 << 22) | 0x3ff, true),
        (keep16 | (1 << 23), false),
        (0, true),
    ];
    let cases: [(u32, u32, u32, u32, bool, u32, u32); 6] = [
        (0x132, 1, 2, 2, false, 0x3a00, 0x4200),
        (0x132, 1, 2, 3, false, 0x3a00, 0x3600),
        (0x132, 3, 3, 1, false, 0x8000, 0x0000),
        (0x132, 1, 2, 1, true, 0x3a00, 0x3c00),
        (0x135, 4, 5, 1, false, 0x3e00, 0x4200),
        (0x135, 4, 5, 3, true, 0x3c00, 0x3a00),
    ];
    let mut fails = Vec::new();
    let mut n = 0;
    let mut check = |label: String, words: Vec<u32>, rsrc1: u32, want: u32| {
        n += 1;
        match run_rsrc1(&words, rsrc1) {
            Ok(r) if r[0] == want => {}
            Ok(r) => fails.push(format!("{label} rsrc1 {rsrc1:#010x}: got {:#x} want {want:#x}", r[0])),
            Err(e) => fails.push(format!("{label} rsrc1 {rsrc1:#010x}: {}", e.lines().take(3).collect::<Vec<_>>().join(" | "))),
        }
    };
    for (op, a, b, omod, clamp, keep, flushed) in cases {
        let w = om(op, a, b, omod, clamp);
        for (r, acts) in rsrc {
            check(format!("op {op:#x} omod {omod} clamp {clamp}"), prog(&ins, &[], &w), r, if acts { flushed } else { keep });
        }
        check(format!("op {op:#x} omod {omod} clamp {clamp} after MODE f16 denormals on"), prog(&ins, &[sopk(0x15, 0, hw(1, 6, 2)), 3], &w), flush16, keep);
        check(format!("op {op:#x} omod {omod} clamp {clamp} after MODE f16 denormals off"), prog(&ins, &[sopk(0x15, 0, hw(1, 6, 2)), 0], &w), keep16, flushed);
    }
    let add32 = [vop2(0x03, 12, v(1), 2)];
    for (r, want) in [(keep16, 1u32), (0xc0 << 12, 0), (0x30 << 12, 1), (0, 0)] {
        check("v_add_f32 of a denormal".into(), prog(&[(1, 1), (2, 0)], &[], &add32), r, want);
    }
    let t = translate(&shader(&prog(&ins, &[], &om(0x132, 1, 2, 2, false)))).unwrap();
    assert!(t.unknowns.iter().any(|u| u.starts_with("f16 output modifier at ")), "{:?}", t.unknowns);
    let t = translate(&shader(&prog(&ins, &[], &om(0x132, 1, 2, 0, true)))).unwrap();
    assert!(t.unknowns.is_empty(), "{:?}", t.unknowns);
    let t = translate(&shader(&prog(&[(1, 1), (2, 0)], &[], &add32))).unwrap();
    assert!(t.unknowns.is_empty(), "{:?}", t.unknowns);
    println!("f16_omod_follows_initial_mode_from_pgm_rsrc1: {n} cases, {} failing", fails.len());
    assert!(fails.is_empty(), "{}", fails.join("
"));
}

#[test]
fn pk_int16_constants_and_clamp() {
    let mut c = Cases::new();
    let pk = |lo: u32, hi: u32| (lo & 0xffff) | (hi << 16);
    let ins = [(1, pk(0x0100, 0x0200))];
    let consts: [(u32, Option<u32>, u32, u32); 9] = [
        (133, None, 0x0105, 0x0200),
        (192, None, 0x0140, 0x0200),
        (193, None, 0x00ff, 0x01ff),
        (208, None, 0x00f0, 0x01ff),
        (240, None, 0x0100, 0x4100),
        (242, None, 0x0100, 0x4180),
        (245, None, 0x0100, 0xc200),
        (246, None, 0x0100, 0x4280),
        (255, Some(0x8001_7fff), 0x80ff, 0x8201),
    ];
    for (src, lit, lo, hi) in consts {
        let mut p = vop3p(0x0a, 10, [v(1), src, 0], 0, 0b011, 0, 0).to_vec();
        if let Some(l) = lit {
            p.push(l);
        }
        vc(&mut c, "v_pk_add_u16", &ins, &p, &[10], &[pk(lo, hi)]);
    }
    let mut p = vop3p(0x0a, 10, [v(1), 255, 0], 0b010, 0b011, 0, 0).to_vec();
    p.push(0x1234_5678);
    vc(&mut c, "v_pk_add_u16", &ins, &p, &[10], &[pk(0x1334, 0x1434)]);
    let clamped: [(&str, u32, u32, u32, u32, u32); 8] = [
        ("v_pk_add_u16", 0x0a, pk(0xfff0, 0x0001), pk(0x0020, 0x0002), 0, pk(0xffff, 0x0003)),
        ("v_pk_sub_u16", 0x0b, pk(0x0010, 0x0005), pk(0x0020, 0x0003), 0, pk(0x0000, 0x0002)),
        ("v_pk_add_i16", 0x02, pk(0x7ff0, 0x8010), pk(0x0020, 0xffe0), 0, pk(0x7fff, 0x8000)),
        ("v_pk_sub_i16", 0x03, pk(0x8000, 0x7fff), pk(0x0001, 0xffff), 0, pk(0x8000, 0x7fff)),
        ("v_pk_mad_u16", 0x09, pk(0x0100, 0x0002), pk(0x0100, 0x0003), pk(0x0000, 0x0001), pk(0xffff, 0x0007)),
        ("v_pk_mad_i16", 0x00, pk(0x0100, 0xff00), pk(0x0100, 0x0100), 0, pk(0x7fff, 0x8000)),
        ("v_pk_add_i16", 0x02, pk(0x8000, 0x7fff), pk(0x0001, 0xffff), 0, pk(0x8001, 0x7ffe)),
        ("v_pk_mad_i16", 0x00, pk(0x8000, 0x7fff), pk(0x0001, 0x0001), pk(0x0001, 0xffff), pk(0x8001, 0x7ffe)),
    ];
    for (m, op, a, b, d, want) in clamped {
        let mad = m.contains("mad");
        let mut w = vop3p(op, 10, [v(1), v(2), if mad { v(3) } else { 0 }], 0, if mad { 0b111 } else { 0b011 }, 0, 0);
        w[0] |= 1 << 15;
        vc(&mut c, m, &[(1, a), (2, b), (3, d)], &w, &[10], &[want]);
    }
    c.run_both("pk_int16_constants_and_clamp");
}

#[test]
fn int24_mul_mad_clamp_saturates_the_exact_result() {
    let mut c = Cases::new();
    let rows: [(&str, u32, bool, u32, u32, u32, u32); 17] = [
        ("v_mul_u32_u24", 0x10b, true, 0x00ff_ffff, 0x00ff_ffff, 0, 0xffff_ffff),
        ("v_mul_u32_u24", 0x10b, true, 0x0001_0000, 0x0000_ffff, 0, 0xffff_0000),
        ("v_mul_u32_u24", 0x10b, true, 0xff00_0002, 0x7f00_0003, 0, 6),
        ("v_mad_u32_u24", 0x143, true, 0x00ff_ffff, 0x00ff_ffff, 0xffff_ffff, 0xffff_ffff),
        ("v_mad_u32_u24", 0x143, true, 1, 0x10, 0xffff_fff8, 0xffff_ffff),
        ("v_mad_u32_u24", 0x143, true, 1, 0x10, 0xffff_ffe0, 0xffff_fff0),
        ("v_mad_u32_u24", 0x143, true, 3, 5, 7, 22),
        ("v_mad_u32_u24", 0x143, false, 0x00ff_ffff, 0x00ff_ffff, 0xffff_ffff, 0xfe00_0000),
        ("v_mul_i32_i24", 0x109, true, 0x0080_0000, 0x0080_0000, 0, 0x7fff_ffff),
        ("v_mul_i32_i24", 0x109, true, 0x0080_0000, 0x007f_ffff, 0, 0x8000_0000),
        ("v_mul_i32_i24", 0x109, true, 0x00ff_fffe, 7, 0, 0xffff_fff2),
        ("v_mad_i32_i24", 0x142, true, 1, 1, 0x7fff_ffff, 0x7fff_ffff),
        ("v_mad_i32_i24", 0x142, true, 0x00ff_ffff, 1, 0x8000_0000, 0x8000_0000),
        ("v_mad_i32_i24", 0x142, true, 0x0080_0000, 0x101, 0x0100_0000, 0x8080_0000),
        ("v_mad_i32_i24", 0x142, true, 0x0080_0000, 0x100, 1, 0x8000_0001),
        ("v_mad_i32_i24", 0x142, true, 0x0080_0000, 0x100, 0xffff_ffff, 0x8000_0000),
        ("v_mad_i32_i24", 0x142, false, 0x0080_0000, 0x0080_0000, 0, 0),
    ];
    for (m, op, clamp, a, b, d, want) in rows {
        let w = vop3m(op, 10, [v(1), v(2), v(3)], 0, 0, 0, clamp);
        vc(&mut c, m, &[(1, a), (2, b), (3, d)], &w, &[10], &[want]);
    }
    c.run_both("int24_mul_mad_clamp_saturates_the_exact_result");
}

#[test]
fn pk_int16_mul_shift_minmax() {
    let mut c = Cases::new();
    let pk = |lo: u32, hi: u32| (lo & 0xffff) | (hi << 16);
    let half = |x: u32, hi: bool| if hi { x >> 16 } else { x & 0xffff };
    let ops: [(&str, u32, fn(u32, u32) -> u32); 6] = [
        ("v_pk_mul_lo_u16", 0x01, |a, b| a.wrapping_mul(b) & 0xffff),
        ("v_pk_ashrrev_i16", 0x06, |a, b| ((b as u16 as i16) >> (a & 15)) as u16 as u32),
        ("v_pk_max_i16", 0x07, |a, b| (a as u16 as i16).max(b as u16 as i16) as u16 as u32),
        ("v_pk_min_i16", 0x08, |a, b| (a as u16 as i16).min(b as u16 as i16) as u16 as u32),
        ("v_pk_max_u16", 0x0c, |a, b| a.max(b)),
        ("v_pk_min_u16", 0x0d, |a, b| a.min(b)),
    ];
    let inputs = [
        (pk(0x0003, 0x8000), pk(0x7fff, 0x0002)),
        (pk(0xffff, 0x0000), pk(0x0001, 0xffff)),
        (pk(0x8000, 0x7fff), pk(0x8001, 0x7ffe)),
        (pk(0x0011, 0x001f), pk(0x8000, 0xc350)),
        (pk(0x0100, 0x00ff), pk(0x0101, 0x0102)),
    ];
    let sels = [(0b000, 0b011), (0b011, 0b000), (0b001, 0b010)];
    for (m, op, f) in ops {
        for &(a, b) in &inputs {
            for &(sel, sel_hi) in &sels {
                let lo = f(half(a, sel & 1 != 0), half(b, sel & 2 != 0));
                let hi = f(half(a, sel_hi & 1 != 0), half(b, sel_hi & 2 != 0));
                let w = vop3p(op, 10, [v(1), v(2), 0], sel, sel_hi, 0, 0);
                vc(&mut c, m, &[(1, a), (2, b)], &w, &[10], &[pk(lo, hi)]);
            }
        }
    }
    c.run_both("pk_int16_mul_shift_minmax");
}

#[path = "exec_mem/mod.rs"]
mod exec_mem;

fn h_round(x: f64, mode: u32) -> u32 {
    let ng = x < 0.0 || (x == 0.0 && x.is_sign_negative());
    let a = x.abs();
    let sg = if ng { 0x8000 } else { 0 };
    let up_dir = |ng: bool| (mode == 1 && !ng) || (mode == 2 && ng);
    if a > 65504.0 {
        let inf = match mode {
            0 => a >= 65520.0,
            _ => up_dir(ng),
        };
        return sg | if inf { 0x7c00 } else { 0x7bff };
    }
    let (mut lo, mut hi) = (0u32, 0x7bffu32);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if h2f(mid) as f64 <= a { lo = mid } else { hi = mid - 1 }
    }
    if h2f(lo) as f64 == a {
        return sg | lo;
    }
    let hi = lo + 1;
    let b = match mode {
        0 => {
            let (dl, dh) = (a - h2f(lo) as f64, h2f(hi) as f64 - a);
            if dl < dh || (dl == dh && lo & 1 == 0) { lo } else { hi }
        }
        3 => lo,
        _ => if up_dir(ng) { hi } else { lo },
    };
    sg | b
}

fn d1f(x: f64) -> [u32; 2] {
    [x.to_bits() as u32, (x.to_bits() >> 32) as u32]
}

#[test]
fn scalar_mode_f16_f64() {
    let mut c = Cases::new();
    let rm = |sp: u32, dp: u32| sopp(0x24, (sp | (dp << 2)) as u16);
    let hb = |x: f32| f2h(x);
    let one = 0x3c00u32;
    let q = hb(2f32.powi(-12));
    let p1h = 0x3c01u32;
    let ph = h2f(p1h) as f64;
    let p1d = 1.0 + 2f64.powi(-52);
    let p1 = fb(1.0 + 2f32.powi(-23));
    for mode in 0..4u32 {
        let w = |x: f64| D16 | h_round(x, mode);
        let up = |rne: u64, pos: bool| if (mode == 1 && pos) || (mode == 2 && !pos) { rne + 1 } else { rne };
        vc(&mut c, "s_round_mode", &[(1, one), (2, q)], &[rm(0, mode), vop2(0x32, 10, v(1), 2)], &[10], &[w(1.0 + 2f64.powi(-12))]);
        vc(&mut c, "s_round_mode", &[(1, one | 0x8000), (2, q | 0x8000)], &[rm(0, mode), vop2(0x32, 10, v(1), 2)], &[10], &[w(-(1.0 + 2f64.powi(-12)))]);
        vc(&mut c, "s_round_mode", &[(1, one), (2, one)], &[rm(0, mode), vop2(0x33, 10, v(1), 2)], &[10], &[D16 | if mode == 2 { 0x8000 } else { 0 }]);
        vc(&mut c, "s_round_mode", &[(1, 0x7bff), (2, 0x7bff)], &[rm(0, mode), vop2(0x32, 10, v(1), 2)], &[10], &[w(131008.0)]);
        vc(&mut c, "s_round_mode", &[(1, p1h), (2, p1h)], &[rm(0, mode), vop2(0x35, 10, v(1), 2)], &[10], &[w(ph * ph)]);
        vc(&mut c, "s_round_mode", &[(1, p1h), (2, p1h | 0x8000)], &[rm(0, mode), vop2(0x35, 10, v(1), 2)], &[10], &[w(-ph * ph)]);
        let f3 = vop3(0x34b, 10, [v(1), v(2), v(3)]);
        vc(&mut c, "s_round_mode", &[(1, p1h), (2, p1h), (3, one | 0x8000)], &[rm(0, mode), f3[0], f3[1]], &[10], &[w(ph * ph - 1.0)]);
        vc(&mut c, "s_round_mode", &[(1, 0x3f80_0800)], &[rm(0, mode), vop1(0x0a, 10, v(1))], &[10], &[w(1.0 + 2f64.powi(-12))]);
        vc(&mut c, "s_round_mode", &[(1, 2049)], &[rm(0, mode), vop1(0x50, 10, v(1))], &[10], &[w(2049.0)]);
        vc(&mut c, "s_round_mode", &[(1, (-2049i32) as u32 & 0xffff)], &[rm(0, mode), vop1(0x51, 10, v(1))], &[10], &[w(-2049.0)]);
        vc(&mut c, "s_round_mode", &[(1, p1h), (2, (-30i32) as u32)], &[rm(0, mode), vop2(0x3b, 10, v(1), 2)], &[10], &[w(ph * 2f64.powi(-30))]);
        let pk = vop3p(0x0f, 10, [v(1), v(2), 0], 0, 3, 0, 0);
        let x = 1.0 + 2f64.powi(-12);
        vc(&mut c, "s_round_mode", &[(1, one | ((one | 0x8000) << 16)), (2, q | ((q | 0x8000) << 16))], &[rm(0, mode), pk[0], pk[1]], &[10], &[h_round(x, mode) | (h_round(-x, mode) << 16)]);
        let pkm = vop3p(0x10, 10, [v(1), v(2), 0], 0, 3, 0, 0);
        vc(&mut c, "s_round_mode", &[(1, p1h | (p1h << 16)), (2, p1h | ((p1h | 0x8000) << 16))], &[rm(0, mode), pkm[0], pkm[1]], &[10], &[h_round(ph * ph, mode) | (h_round(-ph * ph, mode) << 16)]);
        let pkf = vop3p(0x0e, 10, [v(1), v(2), v(3)], 0, 7, 0, 0);
        let m1 = one | 0x8000;
        vc(&mut c, "s_round_mode", &[(1, p1h * 0x10001), (2, p1h * 0x10001), (3, m1 * 0x10001)], &[rm(0, mode), pkf[0], pkf[1]], &[10], &[h_round(ph * ph - 1.0, mode) * 0x10001]);
        vc(&mut c, "s_round_mode", &[(1, p1h), (2, p1h), (10, D16 | m1)], &[rm(0, mode), vop2(0x36, 10, v(1), 2)], &[10], &[w(ph * ph - 1.0)]);
        vc(&mut c, "s_round_mode", &[(1, p1h), (2, p1h)], &[rm(0, mode), vop2(0x38, 10, v(1), 2), m1], &[10], &[w(ph * ph - 1.0)]);
        let a3 = vop3(0x164, 10, [v(2), v(4), 0]);
        let want = up(1f64.to_bits(), true);
        let t = d1f(2f64.powi(-55));
        vc(&mut c, "s_round_mode", &[(2, d1f(1.0)[0]), (3, d1f(1.0)[1]), (4, t[0]), (5, t[1])], &[rm(0, mode), a3[0], a3[1]], &[10, 11], &[want as u32, (want >> 32) as u32]);
        let m3 = vop3(0x165, 10, [v(2), v(4), 0]);
        let want = up(0x3ff0_0000_0000_0002, true);
        let pd = d1f(p1d);
        vc(&mut c, "s_round_mode", &[(2, pd[0]), (3, pd[1]), (4, pd[0]), (5, pd[1])], &[rm(0, mode), m3[0], m3[1]], &[10, 11], &[want as u32, (want >> 32) as u32]);
        let fm3 = vop3(0x14c, 10, [v(2), v(4), v(6)]);
        let want = up(2f64.powi(-51).to_bits(), true);
        let n1 = d1f(-1.0);
        vc(&mut c, "s_round_mode", &[(2, pd[0]), (3, pd[1]), (4, pd[0]), (5, pd[1]), (6, n1[0]), (7, n1[1])], &[rm(0, mode), fm3[0], fm3[1]], &[10, 11], &[want as u32, (want >> 32) as u32]);
        let ld = vop3(0x168, 10, [v(2), v(4), 0]);
        let want: u32 = if mode <= 1 { 2 } else { 1 };
        vc(&mut c, "s_round_mode", &[(2, d1f(1.5)[0]), (3, d1f(1.5)[1]), (4, (-1074i32) as u32)], &[rm(0, mode), ld[0], ld[1]], &[10, 11], &[want, 0]);
        let r32 = |rne: u32| up(rne as u64, true) as u32;
        let x25 = d1f(1.0 + 2f64.powi(-25));
        vc(&mut c, "s_round_mode", &[(2, x25[0]), (3, x25[1])], &[rm(mode, 0), vop1(0x0f, 10, v(2))], &[10], &[r32(0x3f80_0000)]);
        let l3 = vop3(0x362, 10, [v(1), v(2), 0]);
        vc(&mut c, "s_round_mode", &[(1, fb(1.5)), (2, (-149i32) as u32)], &[rm(mode, 0), l3[0], l3[1]], &[10], &[if mode <= 1 { 2 } else { 1 }]);
        let mx = vop3p(0x20, 10, [v(1), v(2), v(3)], 0, 0, 0, 0);
        vc(&mut c, "s_round_mode", &[(1, p1), (2, p1), (3, fb(-1.0))], &[rm(mode, 0), mx[0], mx[1]], &[10], &[r32(0x3480_0000)]);
        let ml = vop3(0x150, 10, [v(1), v(2), v(3)]);
        vc(&mut c, "s_round_mode", &[(1, p1), (2, p1), (3, fb(1.0))], &[rm(mode, 0), ml[0], ml[1]], &[10], &[r32(0x3f80_0002)]);
    }
    for (imm, want) in [(0xeu16, 1u32), (0xd, 0)] {
        let mut pre = pr();
        pre.vv(1, 1).vv(2, 0);
        let mut post = pr();
        post.vcc_to_v(0);
        c.add("s_denorm_mode", &pre, &[sopp(0x25, imm), vopc(0x02, v(1), 2)], &post, &[want]);
    }
    vc(&mut c, "s_denorm_mode", &[(1, 0x0040_0000)], &[sopp(0x25, 0xe), vop1(0x40, 10, v(1))], &[10], &[0]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x0040_0000)], &[sopp(0x25, 0xd), vop1(0x40, 10, v(1))], &[10], &[fb(0.5)]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x8001), (2, hb(-0.5))], &[sopp(0x25, 0x3), vop2(0x39, 10, v(1), 2)], &[10], &[D16 | 0x8000]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x8001), (2, hb(-0.5))], &[sopp(0x25, 0xf), vop2(0x39, 10, v(1), 2)], &[10], &[D16 | 0x8001]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x0001), (2, hb(2048.0))], &[sopp(0x25, 0x7), vop2(0x35, 10, v(1), 2)], &[10], &[D16 | hb(2f32.powi(-13))]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x0001), (2, hb(2048.0))], &[sopp(0x25, 0xb), vop2(0x35, 10, v(1), 2)], &[10], &[D16]);
    let e12 = hb(2f32.powi(-12));
    vc(&mut c, "s_denorm_mode", &[(1, e12), (2, e12)], &[sopp(0x25, 0x7), vop2(0x35, 10, v(1), 2)], &[10], &[D16]);
    vc(&mut c, "s_denorm_mode", &[(1, e12), (2, e12)], &[sopp(0x25, 0xb), vop2(0x35, 10, v(1), 2)], &[10], &[D16 | 0x0001]);
    let n1 = d1f(-1.0);
    let mx64 = vop3(0x167, 10, [v(1), v(3), 0]);
    vc(&mut c, "s_denorm_mode", &[(1, 1), (2, 0), (3, n1[0]), (4, n1[1])], &[sopp(0x25, 0x3), mx64[0], mx64[1]], &[10, 11], &[0, 0]);
    vc(&mut c, "s_denorm_mode", &[(1, 1), (2, 0), (3, n1[0]), (4, n1[1])], &[sopp(0x25, 0xf), mx64[0], mx64[1]], &[10, 11], &[1, 0]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x8001)], &[sopp(0x25, 0x3), vop1(0x0b, 10, v(1))], &[10], &[0x8000_0000]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x8001)], &[sopp(0x25, 0x7), vop1(0x0b, 10, v(1))], &[10], &[fb(-(2f32.powi(-24)))]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x200)], &[sopp(0x25, 0xc), vop1(0x10, 10, v(1))], &[10, 11], &[0, 0]);
    vc(&mut c, "s_denorm_mode", &[(1, 0x200), (2, 0x7777)], &[sopp(0x25, 0xc), vop2(0x03, 3, v(1), 2)], &[1], &[0x200]);
    c.run_both("mode_f16_f64");
    let mut z = Cases::new();
    let tiny = fb(2f32.powi(-70));
    let big = fb(2f32.powi(20));
    let m3 = vop3(0x141, 10, [v(1), v(2), v(3)]);
    vc(&mut z, "v_mad_f32", &[(1, 0x200), (2, big), (3, 0)], &m3, &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, tiny), (2, tiny), (3, 0)], &m3, &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, fb(2.0)), (2, fb(3.0)), (3, 0x200)], &m3, &[10], &[fb(6.0)]);
    let mo = |op: u32, omod: u32, clamp: bool| {
        let mut w = vop3m(op, 10, [v(1), v(2), v(3)], 0, 0, 0, clamp);
        w[1] |= omod << 27;
        w
    };
    let (fmin, one, nz) = (0x0080_0000u32, fb(1.0), 0x8000_0000u32);
    vc(&mut z, "v_mad_f32", &[(1, fmin), (2, one), (3, 0)], &mo(0x141, 3, false), &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, fmin | nz), (2, one), (3, nz)], &mo(0x141, 3, false), &[10], &[nz]);
    vc(&mut z, "v_mad_f32", &[(1, fmin | nz), (2, one), (3, nz)], &mo(0x141, 3, true), &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, 0x00c0_0000), (2, one), (3, fmin | nz)], &mo(0x141, 1, false), &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, 0x00c0_0000), (2, one), (3, fmin | nz)], &mo(0x141, 2, false), &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, 0x0040_0000), (2, one), (3, 0)], &mo(0x141, 1, false), &[10], &[0]);
    vc(&mut z, "v_mad_legacy_f32", &[(1, fmin), (2, one), (3, 0)], &mo(0x140, 3, false), &[10], &[0]);
    vc(&mut z, "v_mad_legacy_f32", &[(1, 0x00c0_0000), (2, one), (3, fmin | nz)], &mo(0x140, 1, false), &[10], &[0]);
    vc(&mut z, "v_mad_f32", &[(1, fb(3.0)), (2, one), (3, fb(1.0))], &mo(0x141, 3, false), &[10], &[fb(2.0)]);
    vc(&mut z, "v_mac_f32", &[(1, tiny), (2, tiny), (10, 0)], &[vop2(0x1f, 10, v(1), 2)], &[10], &[0]);
    vc(&mut z, "v_madak_f32", &[(1, 0x200), (2, big)], &[vop2(0x21, 10, v(1), 2), 0], &[10], &[0]);
    vc(&mut z, "v_madmk_f32", &[(1, tiny), (2, 0)], &[vop2(0x20, 10, v(1), 2), tiny], &[10], &[0]);
    let f3 = vop3(0x14b, 10, [v(1), v(2), v(3)]);
    vc(&mut z, "v_fma_f32", &[(1, 0x200), (2, big), (3, 0)], &f3, &[10], &[0x0380_0000]);
    vc(&mut z, "v_fmac_f16", &[(1, 1), (2, hb(2048.0)), (10, D16)], &[vop2(0x36, 10, v(1), 2)], &[10], &[D16 | 0x0800]);
    vc(&mut z, "v_fmaak_f16", &[(1, e12), (2, e12)], &[vop2(0x38, 10, v(1), 2), 0], &[10], &[D16 | 0x0001]);
    z.run_both("mad_denorm");
}

#[test]
fn scalar_s_trap() {
    let mut c = Cases::new();
    for imm in [0u16, 2, 3, 0xff] {
        sopp_state(&mut c, "s_trap", &[sopp(0x12, imm)]);
        let mut pre = pr();
        pre.vv(10, 0x1111).vv(11, 0x2222).sv(20, 0x3333).raw(&[vop1(1, 10, ic(5))]);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 20);
        c.add("s_trap", &pre, &[sopp(0x12, imm), vop1(1, 11, ic(6)), sop1(3, 20, ic(9))], &post, &[5, 6, 9]);
        let mut pre = pr();
        pre.vv(10, 0x1111).vv(11, 0x2222).sv(20, 0x3333).sv(126, 0).raw(&[vop1(1, 10, ic(5))]);
        let mut post = pr();
        post.sv(126, 0xffff_ffff).raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 20);
        c.add("s_trap", &pre, &[sopp(0x12, imm), vop1(1, 11, ic(6)), sop1(3, 20, ic(9))], &post, &[0x1111, 0x2222, 9]);
    }
    c.run("s_trap");
}

#[test]
fn second_forward_branch_into_open_guard_skips() {
    let mut c = Cases::new();
    let mut pre = pr();
    pre.vv(10, 0x1111).raw(&[SCC0]);
    let mut post = pr();
    post.raw(&[vop1(1, 0, v(10))]);
    c.add("s_cbranch_scc1", &pre, &[sopp(0x05, 3), vop1(1, 10, ic(7)), sopp(0x02, 1), vop1(1, 10, ic(8))], &post, &[7]);
    let mut pre = pr();
    pre.vv(10, 0x1111).raw(&[SCC1]);
    c.add("s_cbranch_scc1", &pre, &[sopp(0x05, 3), vop1(1, 10, ic(7)), sopp(0x02, 1), vop1(1, 10, ic(8))], &post, &[0x1111]);
    let mut pre = pr();
    pre.vv(10, 0x1111).raw(&[SCC0]);
    c.add("s_cbranch_scc1", &pre, &[sopp(0x05, 4), vop1(1, 10, ic(7)), SCC0, sopp(0x05, 1), vop1(1, 10, ic(8))], &post, &[8]);
    c.run("branch_guard");
}

#[test]
fn legacy_minmax_on_warp_keeps_a_nan_in_either_source() {
    fn legacy(op: u32, a: u32, b: u32) -> u32 {
        let mov = |dst: u32, val: u32| [0x7e00_0000 | (dst << 17) | (1 << 9) | 255, val];
        let mut code = Vec::new();
        code.extend_from_slice(&mov(1, a));
        code.extend_from_slice(&mov(2, b));
        code.push((op << 25) | (2 << 9) | (256 + 1));
        code.push(S_ENDPGM);
        let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
        let lowered = tombox_xshader::gcn::to_rdna2(&bytes).unwrap_or_else(|e| panic!("{e}"));
        let mut words: Vec<u32> = lowered.isa[..lowered.code_len]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(words.pop(), Some(S_ENDPGM));
        let mut p = Prog::new();
        p.raw(&words);
        run(&p.words()).unwrap_or_else(|e| panic!("{e}"))[0]
    }
    let nan = 0x7fc0_0000u32;
    let one = 0x3f80_0000u32;
    assert_eq!(legacy(0x0d, nan, one), one, "min with NaN in source 0");
    assert_eq!(legacy(0x0d, one, nan), nan, "min with NaN in source 1");
    assert_eq!(legacy(0x0e, nan, one), one, "max with NaN in source 0");
    assert_eq!(legacy(0x0e, one, nan), nan, "max with NaN in source 1");
    assert_eq!(legacy(0x0e, 0, 0x8000_0000), 0, "max of +0 and -0 keeps source 0");
    assert_eq!(legacy(0x0e, 0x8000_0000, 0), 0x8000_0000, "max of -0 and +0 keeps source 0");
}

#[test]
fn gcn_clamp_transcendentals_match_the_isa() {
    fn run_op(op: u32, x: u32) -> u32 {
        let mov = [0x7e00_0000 | (1 << 17) | (1 << 9) | 255, x];
        let mut code = vec![mov[0], mov[1]];
        code.push(0x7e00_0000 | (op << 9) | (256 + 1));
        code.push(S_ENDPGM);
        let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
        let lowered = tombox_xshader::gcn::to_rdna2(&bytes).unwrap_or_else(|e| panic!("op {op:#x}: {e}"));
        let mut words: Vec<u32> = lowered.isa[..lowered.code_len]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(words.pop(), Some(S_ENDPGM), "op {op:#x} must end in s_endpgm");
        let mut p = Prog::new();
        p.raw(&words);
        run(&p.words()).unwrap_or_else(|e| panic!("op {op:#x}: {e}"))[0]
    }
    let cases: &[(u32, &[(f32, u32)])] = &[
        (0x28, &[(0.0, 0x7f7f_ffff), (-0.0, 0xff7f_ffff), (f32::INFINITY, 0), (1.0, 0x3f80_0000), (2.0, 0x3f00_0000), (0.125, 0x4100_0000)]),
        (0x2c, &[(0.0, 0x7f7f_ffff), (-0.0, 0xff7f_ffff), (f32::INFINITY, 0), (1.0, 0x3f80_0000), (4.0, 0x3f00_0000), (0.25, 0x4000_0000), (-1.0, 0xffc0_0000)]),
        (0x26, &[(0.0, 0xff7f_ffff), (-0.0, 0xff7f_ffff), (1.0, 0), (2.0, 0x3f80_0000), (4.0, 0x4000_0000), (8.0, 0x4040_0000), (f32::INFINITY, 0x7f80_0000), (-1.0, 0xffc0_0000)]),
    ];
    for op in [0x29u32, 0x2d, 0x45, 0x46] {
        let code: Vec<u8> = [0x7e00_0000u32 | (1 << 17) | (op << 9) | (256 + 1), S_ENDPGM].iter().flat_map(|w| w.to_le_bytes()).collect();
        let err = match tombox_xshader::gcn::to_rdna2(&code) {
            Ok(_) => panic!("legacy op {op:#x} must stay refused"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("legacy transcendental") && err.contains("not established"), "op {op:#x}: {err}");
    }
    for &(op, list) in cases {
        for &(x, exp) in list {
            assert_eq!(run_op(op, x.to_bits()), exp, "op {op:#x} input {x:?}");
        }
    }
}

#[test]
fn pkaccum_u8_packs_four_bytes_into_one_register() {
    fn mov_lit(dst: u32, val: u32) -> [u32; 2] {
        [0x7e00_0000 | (dst << 17) | (1 << 9) | 255, val]
    }
    fn mov_imm(dst: u32, imm: u32) -> u32 {
        0x7e00_0000 | (dst << 17) | (1 << 9) | (128 + imm)
    }
    let mut code = vec![mov_imm(1, 0)];
    for (i, bits) in [0x3f80_0000u32, 0x4000_0000, 0x4040_0000, 0x4080_0000].into_iter().enumerate() {
        code.extend_from_slice(&mov_lit(0, bits));
        code.push(mov_imm(2, i as u32));
        code.push((0x2c << 25) | (1 << 17) | (2 << 9) | 256);
    }
    code.push(S_ENDPGM);
    let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let lowered = tombox_xshader::gcn::to_rdna2(&bytes).unwrap_or_else(|e| panic!("{e}"));
    let mut words: Vec<u32> = lowered.isa[..lowered.code_len]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(words.pop(), Some(S_ENDPGM));
    let mut p = Prog::new();
    p.raw(&words);
    let got = run(&p.words()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got[1], 0x0403_0201, "{got:08x?}");
}

#[test]
fn merged_vertex_shader_prologue_keeps_the_vertex_active() {
    let input = |name, format, offset| VertexElement {
        semantic: name, semantic_index: 0, format, slot: 0, offset, input_class: 0, step_rate: 0,
    };
    let layout = [
        input(b"POSITION\0".as_ptr(), 16, 0),
        input(b"NORMAL\0".as_ptr(), 2, 8),
        input(b"TEXCOORD\0".as_ptr(), 42, 24),
    ];
    let mut words = vec![sop2(0x28, 32, 3, LIT), 0x80000, sop2(0x25, 126, 32, ic(0)), sopc(0x06, 32, ic(64)), sop2(0x0b, 126, ic(-1), 126)];
    words.extend_from_slice(&[0xbe84_2104, 0xf800_00cf, 0x0c0b_0a09, 0xf800_0a0f, 0x100f_0e0d, S_ENDPGM]);
    let mut vs = shader(&words);
    vs.stage = Stage::Vertex;
    vs.input_sig = vec![sig("POSITION", 0, 3, 0, 3), sig("NORMAL", 0, 3, 1, 0xf), sig("TEXCOORD", 0, 1, 3, 4)];
    vs.input_sig[0].rw_mask = 3;
    vs.input_sig[2].rw_mask = 4;
    vs.output_sig = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)];
    let vs = translate(&vs).unwrap();
    let vertex_code = dxc::compile(&vs.hlsl, &dxc::profile_for(vs.profile)).unwrap();
    let pixel_code = dxc::compile(
        "uint4 main(float4 p : SV_Position, nointerpolation uint4 v : TEXCOORD0) : SV_Target { return v ^ uint4(uint2(p.xy), 0, 0); }",
        "ps_6_0",
    ).unwrap();
    let vertices: Vec<u8> = [(-1.0f32, 1.0f32), (3.0, 1.0), (-1.0, -3.0)]
        .into_iter()
        .flat_map(|(x, y)| [x.to_bits(), y.to_bits(), 0x41a8_0000, 0, 0, 0, 0x1234])
        .flat_map(u32::to_le_bytes)
        .collect();
    let mut c = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() })).lock().unwrap_or_else(|e| e.into_inner());
    let result = unsafe {
        let vb = c.gpu.buffer(vertices.len() as u64, 2, 0xac3, false).unwrap();
        assert!(Gpu::map_write(vb, &vertices));
        let view = [Gpu::gpu_va(vb), ((28u64) << 32) | vertices.len() as u64];
        let rs = c.rs;
        let result = draw_graphics(&mut c, &pixel_code, rs, &|list| {
            vcall!(list, 44, fn(u32, u32, *const u64) -> (), 0, 1, view.as_ptr());
        }, &|_| {}, Some((&vertex_code, &layout)));
        d3d::release(vb);
        result
    };
    assert_eq!(result.unwrap_or_else(|e| panic!("{e}\n{}", vs.hlsl)), [0, 0, 0x1234, 0x3f80_0000], "{}", vs.hlsl);
}

#[test]
fn merged_vertex_shader_reads_vertex_and_instance_index_from_v5_and_v8() {
    let mut words = vec![sop2(0x28, 32, 3, LIT), 0x80000, sop2(0x25, 126, 32, ic(0)), sopc(0x06, 32, ic(64)), sop2(0x0b, 126, ic(-1), 126)];
    words.extend_from_slice(&[0xbe84_2104, 0xf800_00cf, 0x0c0b_0a09, 0xf800_0a0f, 0x0909_0805, S_ENDPGM]);
    let mut vs = shader(&words);
    vs.stage = Stage::Vertex;
    vs.input_sig = vec![sig("POSITION", 0, 3, 0, 0xf), sig("SV_VertexID", 6, 1, 1, 1), sig("SV_InstanceID", 8, 1, 2, 1)];
    vs.input_sig[0].rw_mask = 0xf;
    vs.output_sig = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)];
    let h = translate(&vs).unwrap().hlsl;
    assert!(h.contains("s3 = 0x40u;"), "{h}");
    assert!(h.contains("v5 = (uint)(i.i1);") && h.contains("v8 = (uint)(i.i2);"), "{h}");
    assert!(h.contains("v9 = asuint(i.i0.x);") && !h.contains("v4 = asuint(i.i0.x);"), "{h}");
    assert!(!h.contains("v0 = (uint)(i.i") && !h.contains("v3 = (uint)(i.i"), "{h}");
    if dxc::available() {
        dxc::compile(&h, "vs_6_0").expect("dxc compiles the merged vertex shader");
    }
}

#[test]
fn vop3b_null_carry_out() {
    const M0_SENT: u32 = 0x1357_2468;
    const VCC_SENT: u32 = 0x55;
    const SNULL: u32 = 125;
    let mut c = Cases::new();

    for (m, op, a, b, res) in [
        ("v_add_co_u32", 0x30fu32, 0xffff_ffffu32, 5u32, 4u32),
        ("v_sub_co_u32", 0x310, 1, 5, 1u32.wrapping_sub(5)),
        ("v_subrev_co_u32", 0x319, 1, 5, 5u32.wrapping_sub(1)),
    ] {
        let mut pre = pr();
        pre.vv(1, a).vv(2, b).sv(124, M0_SENT).sv(VCC, VCC_SENT);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10))]).s2v(1, 124).s2v(2, VCC);
        c.add(m, &pre, &vop3b(op, 10, SNULL, [v(1), v(2), 0]), &post, &[res, M0_SENT, VCC_SENT]);
    }
    {
        let mut pre = pr();
        pre.vv(1, 0xffff_ffff).vv(2, 5).sv(124, M0_SENT).sv(VCC, 1);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10))]).s2v(1, 124).s2v(2, VCC);
        c.add("v_add_co_ci_u32", &pre, &vop3b(0x128, 10, SNULL, [v(1), v(2), VCC]), &post, &[5, M0_SENT, 1]);
    }
    {
        let (a, b, cc) = (0xffff_ffffu32, 0xffff_ffffu32, 0xffff_ffff_ffff_ffffu64);
        let wide = a as u128 * b as u128 + cc as u128;
        let mut pre = pr();
        pre.vv(2, a).vv(4, b).vv(6, cc as u32).vv(7, (cc >> 32) as u32).sv(124, M0_SENT).sv(VCC, VCC_SENT);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 124).s2v(3, VCC);
        c.add("v_mad_u64_u32", &pre, &vop3b(0x176, 10, SNULL, [v(2), v(4), v(6)]), &post, &[wide as u32, (wide >> 32) as u32, M0_SENT, VCC_SENT]);

        let sw = (i32::MAX as i128) * (i32::MAX as i128) + (i64::MAX as i128);
        let mut pre = pr();
        pre.vv(2, i32::MAX as u32).vv(4, i32::MAX as u32).vv(6, i64::MAX as u64 as u32).vv(7, (i64::MAX as u64 >> 32) as u32).sv(124, M0_SENT).sv(VCC, VCC_SENT);
        c.add("v_mad_i64_i32", &pre, &vop3b(0x177, 10, SNULL, [v(2), v(4), v(6)]), &post, &[sw as u64 as u32, (sw as u64 >> 32) as u32, M0_SENT, VCC_SENT]);
    }
    {
        let (x, d, n) = (1e-300f64, 1e-300f64, 1e300f64);
        let scaled = x * 2f64.powi(128);
        let mut pre = pr();
        for (k, val) in [x, d, n].iter().enumerate() {
            let bits = val.to_bits();
            pre.vv(2 + 2 * k as u32, bits as u32).vv(3 + 2 * k as u32, (bits >> 32) as u32);
        }
        pre.sv(124, M0_SENT).sv(VCC, VCC_SENT);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 124).s2v(3, VCC);
        let sb = scaled.to_bits();
        c.add("v_div_scale_f64", &pre, &vop3b(0x16e, 10, SNULL, [v(2), v(4), v(6)]), &post, &[sb as u32, (sb >> 32) as u32, M0_SENT, VCC_SENT]);
    }
    c.run_both("vop3b_null_carry_out");

    for (m, op) in [("v_add_co_u32", 0x30fu32), ("v_mad_u64_u32", 0x176), ("v_div_scale_f64", 0x16e)] {
        let mut p = pr();
        p.raw(&vop3b(op, 10, 124, [v(2), v(4), v(6)]));
        let err = translate(&shader(&p.words())).err().unwrap_or_else(|| panic!("{m} with sdst=124 (m0) should fail"));
        assert!(err.contains("carry-out register code 124 unsupported"), "{m}: unexpected error: {err}");
    }
}
