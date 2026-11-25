#![cfg(windows)]
#![allow(dead_code)]

#[path = "../../tools/xreplay/src/d3d.rs"]
mod d3d;

use core::ffi::c_void;
use d3d::{Gpu, Guid};
use tombox_xshader::hlsl::translate;
use tombox_xshader::meta::{parse_psv0_mesh, GsInfo, MeshInfo, SigElement};
use tombox_xshader::{dxc, extract, rdna2, ShaderExtract, Stage};

const S_ENDPGM: u32 = 0xbf81_0000;
const LIT: u32 = 0xff;
const EXEC: u32 = 126;
const M0: u32 = 124;
const F_NEG1: u32 = 243;
const F_ONE: u32 = 242;

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

fn sig(name: &str, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index: 0, sysval, comp_type, register, mask, rw_mask: 0 }
}

fn mesh_info(threads: u32, mv: u16, mp: u16, topology: u8) -> MeshInfo {
    MeshInfo { stage: 13, groupshared_bytes: 0, payload_bytes: 0, max_vertices: mv, max_primitives: mp, prim_vectors: 0, topology, num_threads: [threads, 1, 1] }
}

fn ms(words: &[u32], mi: MeshInfo, outs: Vec<SigElement>) -> ShaderExtract {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderExtract {
        stage: Stage::Mesh,
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
        has_output_sig: true,
        has_psv0: true,
        has_root_sig: false,
        input_sig: vec![],
        output_sig: outs,
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: Some(GsInfo { input_primitive: 0, output_topology: 0, max_vertex_count: 0, mesh: Some(mi), tess: None }),
        root_sig: None,
    }
}

fn pos_param_outs() -> Vec<SigElement> {
    vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0x3)]
}

fn console_ms(nv: u32, np: u32, prim: u32) -> Vec<u32> {
    console_ms_b(nv, np, prim, false)
}

fn console_ms_b(nv: u32, np: u32, prim: u32, div_barrier: bool) -> Vec<u32> {
    ms_prog((np << 12) | nv, prim, div_barrier, false, &[], [2, 3])
}

fn ms_prog(m0: u32, prim: u32, div_barrier: bool, wave32: bool, pre: &[u32], tex: [u32; 2]) -> Vec<u32> {
    let mut w = vec![];
    w.push(sop1(if wave32 { 3 } else { 4 }, EXEC, ic(-1)));
    w.extend(vop3(0x365, 0, [ic(-1), ic(0), 0]));
    if !wave32 {
        w.extend(vop3(0x366, 0, [ic(-1), v(0), 0]));
    }
    w.extend_from_slice(pre);
    w.extend([sop2(0x27, 3, 3, LIT), 0x40018]);
    w.extend(vop3(0x346, 0, [3, ic(if wave32 { 5 } else { 6 }), v(0)]));
    w.push(vop1(2, 2, v(5)));
    w.push(sop1(if wave32 { 3 } else { 4 }, 10, EXEC));
    w.push(vopc(0xd2, ic(0), 0));
    w.push(sopp(8, 3 + div_barrier as u16));
    w.extend([sop1(3, M0, LIT), m0]);
    w.push(sopp(0x10, 9));
    if div_barrier {
        w.push(sopp(0x0a, 0));
    }
    w.push(sop1(if wave32 { 3 } else { 4 }, EXEC, 10));
    w.push(sopp(0x0a, 0));
    w.extend([vop1(1, 2, LIT), 4.0f32.to_bits()]);
    w.extend([vopc(0xc2, LIT, 0), 65]);
    w.push(vop2(0x01, 1, F_NEG1, 2));
    w.extend([vopc(0xc2, LIT, 0), 66]);
    w.push(vop2(0x01, 3, F_NEG1, 2));
    w.push(vop1(1, 4, ic(0)));
    w.push(vop1(1, 6, F_ONE));
    w.push(vop1(1, 7, tex[0]));
    w.push(vop1(1, 8, tex[1]));
    w.extend([vop1(1, 9, LIT), prim]);
    w.extend(exp(12, 0xf, false, [1, 3, 4, 6]));
    w.extend(exp(20, 0x1, false, [9, 0, 0, 0]));
    w.extend(exp(32, 0x3, true, [7, 8, 0, 0]));
    w.push(S_ENDPGM);
    w
}

const TRI: u32 = 64 | (65 << 10) | (66 << 20);

#[test]
fn psv0_mesh_info_layout() {
    let mut info = vec![0u8; 52];
    info[0..4].copy_from_slice(&256u32.to_le_bytes());
    info[8..12].copy_from_slice(&0u32.to_le_bytes());
    info[12..14].copy_from_slice(&64u16.to_le_bytes());
    info[14..16].copy_from_slice(&126u16.to_le_bytes());
    info[24] = 13;
    info[26] = 0;
    info[27] = 2;
    info[36..40].copy_from_slice(&128u32.to_le_bytes());
    info[40..44].copy_from_slice(&1u32.to_le_bytes());
    info[44..48].copy_from_slice(&1u32.to_le_bytes());
    let m = parse_psv0_mesh(&info).expect("mesh info");
    assert_eq!(m, MeshInfo { stage: 13, groupshared_bytes: 256, payload_bytes: 0, max_vertices: 64, max_primitives: 126, prim_vectors: 0, topology: 2, num_threads: [128, 1, 1] });
    info[24] = 14;
    info[0..4].copy_from_slice(&48u32.to_le_bytes());
    let a = parse_psv0_mesh(&info).expect("amplification info");
    assert_eq!((a.stage, a.payload_bytes, a.num_threads), (14, 48, [128, 1, 1]));
    info[24] = 5;
    assert!(parse_psv0_mesh(&info).is_none());
}

#[test]
fn encoded_sample_decodes_as_console_mesh_abi() {
    let insns = rdna2::disassemble(&console_ms(67, 1, TRI).iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>());
    let m: Vec<&str> = insns.iter().map(|i| i.mnemonic.as_str()).collect();
    for want in ["s_mov_b64", "v_mbcnt_lo_u32_b32", "v_mbcnt_hi_u32_b32", "s_bfe_u32", "v_lshl_add_u32", "v_readfirstlane_b32", "v_cmpx_eq_u32", "s_cbranch_execz", "s_sendmsg", "s_barrier", "v_cmp_eq_u32", "v_cndmask_b32", "exp", "s_endpgm"] {
        assert!(m.contains(&want), "missing {want} in {m:?}");
    }
}

#[test]
fn mesh_hlsl_entry_and_tail() {
    let t = translate(&ms(&console_ms(67, 1, TRI), mesh_info(128, 128, 128, 2), pos_param_outs())).expect("translate");
    assert_eq!(t.profile, "ms_6_5");
    let h = &t.hlsl;
    for want in [
        "[numthreads(128, 1, 1)]",
        "[outputtopology(\"triangle\")]",
        "out vertices SOut verts[128], out indices uint3 tris[128], out primitives SPrim prims[128]",
        "v5 = gid.x * 128u + gi;",
        "s3 = 0x20000000u | ((gi / 64u) << 24) | (min(64u, 128u - (gi & 0xffffffc0u)) * 0x101u);",
        "uint tb_nv = tb_mscnt & 0x3ffu, tb_np = (tb_mscnt >> 12) & 0x7ffu;",
        "s2 = (gid.x * 128u + (gi & 0xffffffc0u));",
        "(min(gi & 63u, 32u) + 0x0u)",
        "(max(gi & 63u, 32u) - 32u + v0)",
        "if (exec_lo & 1u) tb_mscnt = m0;",
        "SetMeshOutputCounts(tb_nv, tb_np);",
        "if (gi < tb_nv) verts[gi] = o;",
        "tris[gi] = uint3(tb_prim & 0x1ffu, (tb_prim >> 10) & 0x1ffu, (tb_prim >> 20) & 0x1ffu); prims[gi].cull = (tb_prim >> 31) != 0u;",
        "precise float4 o0 : SV_Position0;",
        "uint2 o1 : TEXCOORD0;",
        "if (exec_lo & 1u) tb_prim = v9;",
    ] {
        assert!(h.contains(want), "missing `{want}` in\n{h}");
    }
    let bar = h.matches("GroupMemoryBarrierWithGroupSync();").count();
    assert_eq!(bar, 3, "{h}");
}

#[test]
fn mesh_line_topology_and_wave32() {
    let mut w = vec![];
    w.extend(vop3(0x365, 0, [ic(-1), ic(0), 0]));
    w.extend([sop1(3, M0, LIT), (1 << 12) | 2]);
    w.push(sopp(0x10, 9));
    w.extend([vop1(1, 9, LIT), 1 << 10]);
    w.extend(exp(12, 0xf, false, [0, 0, 0, 0]));
    w.extend(exp(20, 0x1, true, [9, 0, 0, 0]));
    w.push(S_ENDPGM);
    let t = translate(&ms(&w, mesh_info(32, 2, 1, 1), vec![sig("SV_Position", 1, 3, 0, 0xf)])).expect("translate");
    assert!(t.hlsl.contains("[outputtopology(\"line\")]"), "{}", t.hlsl);
    assert!(t.hlsl.contains("out indices uint2 tris[1]"), "{}", t.hlsl);
    assert!(t.hlsl.contains("((gi & 31u) + 0x0u)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("tris[gi] = uint2(tb_prim & 0x1ffu, (tb_prim >> 10) & 0x1ffu);"), "{}", t.hlsl);
}

#[test]
fn mesh_clip_distance_export_maps_to_pos1() {
    let mut w = vec![];
    w.extend([sop1(3, M0, LIT), (1 << 12) | 3]);
    w.push(sopp(0x10, 9));
    w.extend(exp(12, 0xf, false, [0, 0, 0, 0]));
    w.extend(exp(13, 0x3, false, [1, 2, 0, 0]));
    w.extend(exp(20, 0x1, true, [9, 0, 0, 0]));
    w.push(S_ENDPGM);
    let outs = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_ClipDistance", 2, 3, 1, 0x3)];
    let t = translate(&ms(&w, mesh_info(64, 64, 64, 2), outs)).expect("translate");
    assert!(t.hlsl.contains("if (exec_lo & 1u) o.o1.x = asfloat(v1);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("if (exec_lo & 1u) o.o1.y = asfloat(v2);"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("groupshared uint tb_mscnt;"));
    assert!(!t.hlsl.contains("GroupMemoryBarrierWithGroupSync();"));
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap();
    let mut w2 = w.clone();
    let i = w2.iter().position(|x| *x == exp(13, 0x3, false, [1, 2, 0, 0])[0]).unwrap();
    w2[i] = exp(14, 0x3, false, [1, 2, 0, 0])[0];
    let outs = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_ClipDistance", 2, 3, 1, 0x3)];
    let e = translate(&ms(&w2, mesh_info(64, 64, 64, 2), outs)).err().expect("refusal");
    assert!(e.contains("no clip/cull distance element"), "{e}");
}

#[test]
fn mesh_and_amplification_validate_thread_dimensions() {
    for dims in [[1, 0, 1], [0, 1, 1], [129, 1, 1], [65536, 65536, 1], [u32::MAX; 3]] {
        let mut info = mesh_info(1, 3, 1, 2);
        info.num_threads = dims;
        let mesh = ms(&console_ms(3, 1, 0), info, pos_param_outs());
        assert!(translate(&mesh).err().unwrap().contains("thread-group size"));
        info.stage = 14;
        let mut amp = ms(&[S_ENDPGM], info, vec![]);
        amp.stage = Stage::Amplification;
        assert!(translate(&amp).err().unwrap().contains("thread-group size"));
    }

    for dims in [[1, 1, 1], [1, 3, 1], [8, 4, 4]] {
        let mut info = mesh_info(1, 0, 0, 0);
        info.num_threads = dims;
        info.stage = 14;
        let mut amp = ms(&[S_ENDPGM], info, vec![]);
        amp.stage = Stage::Amplification;
        let t = translate(&amp).unwrap();
        let threads: u32 = dims.into_iter().product();
        assert!(t.hlsl.contains(&format!("tb_k < 4u; tb_k += {threads}u) tb_args[tb_k] = 0u;")));
        dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap();
    }
}

#[test]
fn mesh_scalar_buffer_load_uses_root_descriptor_for_aliased_register() {
    use tombox_xshader::meta::{RootParam, RootParamKind, RootSig};

    let mut words = vec![sop1(3, 0, 16), sop1(3, 28, 0), 0xf420_000e, 0xfa00_0000];
    words.extend(console_ms(3, 1, 0));
    let mut shader = ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs());
    let Err(error) = translate(&shader) else {
        panic!("a scalar buffer load requires a known root descriptor");
    };
    assert!(error.contains("missing root signature for base s28 (root s16)"), "{error}");

    shader.has_root_sig = true;
    shader.root_sig = Some(RootSig {
        version: 2,
        flags: 0,
        params: vec![RootParam {
            visibility: 0,
            kind: RootParamKind::Descriptor { kind: 2, register: 7, space: 2 },
        }],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    });
    let translated = translate(&shader).expect("explicit CBV root descriptor");
    assert!(translated.hlsl.contains("register(b7, space2)"), "{}", translated.hlsl);
}

#[test]
fn mesh_typed_buffer_load_uses_root_table_range() {
    use tombox_xshader::meta::{RootParam, RootParamKind, RootRange, RootSig};

    let mut words = vec![0xf408_0008, 0xfa00_0020, 0xe000_2000, 0x8000_0101];
    words.extend(console_ms(3, 1, 0));
    let mut shader = ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs());
    let Err(error) = translate(&shader) else {
        panic!("a typed buffer load requires a known descriptor table");
    };
    assert!(error.contains("missing root signature for descriptor table in s16"), "{error}");

    shader.has_root_sig = true;
    shader.root_sig = Some(RootSig {
        version: 2,
        flags: 0,
        params: vec![RootParam {
            visibility: 0,
            kind: RootParamKind::Table(vec![RootRange {
                range_type: 0,
                num: 1,
                base_register: 7,
                space: 2,
                offset_in_table: 1,
            }]),
        }],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    });
    let translated = translate(&shader).expect("explicit SRV descriptor range");
    assert!(translated.hlsl.contains("Buffer<"), "{}", translated.hlsl);
    assert!(translated.hlsl.contains("register(t7, space2)"), "{}", translated.hlsl);
}

#[test]
fn mesh_fssm_table_slots_follow_the_eight_system_sgprs() {
    let mut words = vec![0xf408_0008, 0xfa00_0020, 0xe000_2000, 0x8000_0101];
    words.extend(console_ms(3, 1, 0));
    let mut shader = ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs());
    let mut f = vec![0u8; 0x100];
    f[0..4].copy_from_slice(b"FSSM");
    f[0xD6] = 9;
    f[0x58..0x60].copy_from_slice(&0xB0u64.to_le_bytes());
    f[0xE8..0xF0].copy_from_slice(&0xF0u64.to_le_bytes());
    f[0xF0..0xF8].copy_from_slice(&0x104u64.to_le_bytes());
    f[0xF8..0xFC].copy_from_slice(&1u32.to_le_bytes());
    f[0xFC..0x100].copy_from_slice(&1u32.to_le_bytes());
    for w in [3u32, 0, 1, 0, 8, 0x300, 8, 7, 4, 0x0804] {
        f.extend_from_slice(&w.to_le_bytes());
    }
    shader.fssm = f;
    assert!(translate(&shader).is_err());
    let translated = tombox_xshader::translate_with_fssm_root(&shader).unwrap_or_else(|e| panic!("FSSM table translation failed: {e}"));
    assert!(translated.hlsl.contains("Buffer<"), "{}", translated.hlsl);
    assert!(translated.hlsl.contains("register(t7)"), "{}", translated.hlsl);
    assert!(dxc::compile(&translated.hlsl, &dxc::profile_for(translated.profile)).is_ok());
}

#[test]
fn mesh_root_signature_inference_binds_scalar_buffer_load() {
    let mut words = vec![sop1(3, 0, 16), sop1(3, 28, 0), 0xf420_000e, 0xfa00_0000];
    words.extend(console_ms(3, 1, 0));
    let mut shader = ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs());
    assert!(translate(&shader).is_err());
    let sig = tombox_xshader::infer_root_signature(&shader).expect("inferred root signature");
    assert_eq!(sig.params.len(), 1);
    match &sig.params[0].kind {
        tombox_xshader::meta::RootParamKind::Descriptor { kind, .. } => assert_eq!(*kind, 2),
        other => panic!("expected root descriptor, got {other:?}"),
    }
    shader.root_sig = Some(sig);
    let translated = translate(&shader).expect("translated with inferred root signature");
    assert!(translated.hlsl.contains("cbuffer CB"), "{}", translated.hlsl);
    assert!(dxc::compile(&translated.hlsl, &dxc::profile_for(translated.profile)).is_ok());
}

#[test]
fn mesh_root_signature_inference_binds_typed_buffer_table() {
    let mut words = vec![0xf408_0008, 0xfa00_0020, 0xe000_2000, 0x8000_0101];
    words.extend(console_ms(3, 1, 0));
    let shader = ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs());
    assert!(translate(&shader).is_err());
    let sig = tombox_xshader::infer_root_signature(&shader).expect("inferred root signature");
    assert_eq!(sig.params.len(), 1);
    match &sig.params[0].kind {
        tombox_xshader::meta::RootParamKind::Table(ranges) => {
            assert_eq!(ranges.len(), 1);
            assert_eq!(ranges[0].range_type, 0);
            assert_eq!(ranges[0].offset_in_table, 1);
        }
        other => panic!("expected descriptor table, got {other:?}"),
    }
    let translated = tombox_xshader::translate_with_inference(&shader).expect("translated with inference");
    assert!(translated.hlsl.contains("Buffer<"), "{}", translated.hlsl);
    assert!(dxc::compile(&translated.hlsl, &dxc::profile_for(translated.profile)).is_ok());
}

#[test]
fn mesh_root_signature_inference_matches_psv_metadata() {
    use tombox_xshader::meta::PsvResource;

    let mut words = vec![0xf408_0008, 0xfa00_0020, 0xe000_2000, 0x8000_0101];
    words.extend(console_ms(3, 1, 0));
    let mut shader = ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs());
    shader.resources = vec![PsvResource {
        res_type: 3,
        space: 4,
        lower: 9,
        upper: 9,
        kind: 0,
    }];
    let sig = tombox_xshader::infer_root_signature(&shader).expect("inferred root signature");
    match &sig.params[0].kind {
        tombox_xshader::meta::RootParamKind::Table(ranges) => {
            assert_eq!(ranges[0].base_register, 9);
            assert_eq!(ranges[0].space, 4);
        }
        other => panic!("expected descriptor table, got {other:?}"),
    }
    let translated = tombox_xshader::translate_with_inference(&shader).expect("translated with inference");
    assert!(translated.hlsl.contains("register(t9, space4)"), "{}", translated.hlsl);
    assert!(dxc::compile(&translated.hlsl, &dxc::profile_for(translated.profile)).is_ok());
}

#[test]
fn mesh_refusals_are_explicit() {
    let base = console_ms(3, 1, 0);
    let mut mi = mesh_info(128, 128, 128, 2);
    mi.prim_vectors = 1;
    assert!(translate(&ms(&base, mi, pos_param_outs())).err().expect("refusal").contains("per-primitive outputs"));
    let mut mi = mesh_info(128, 128, 128, 2);
    mi.payload_bytes = 16;
    let t = translate(&ms(&base, mi, pos_param_outs())).expect("mesh shader with an amplification payload");
    assert!(t.hlsl.contains("struct TbPayload { uint w[4]; };") && t.hlsl.contains("in payload TbPayload tb_pl"), "{}", t.hlsl);
    let mut mi = mesh_info(128, 128, 128, 2);
    mi.payload_bytes = 16388;
    assert!(translate(&ms(&base, mi, pos_param_outs())).err().expect("refusal").contains("payload of 16388 bytes exceeds the D3D12 limit 16384"));
    assert!(translate(&ms(&base, mesh_info(64, 128, 64, 2), pos_param_outs())).err().expect("refusal").contains("multi-row export"));
    assert!(translate(&ms(&base, mesh_info(128, 128, 128, 0), pos_param_outs())).err().expect("refusal").contains("topology 0"));
    let mut bad = base.clone();
    let i = bad.iter().position(|x| *x == sopp(0x10, 9)).unwrap();
    bad[i] = sopp(0x10, 2);
    assert!(translate(&ms(&bad, mesh_info(128, 128, 128, 2), pos_param_outs())).err().expect("refusal").contains("message 2 in a mesh shader"));
    let mut mask = base.clone();
    let i = mask.iter().position(|x| *x == vop3(0x365, 0, [ic(-1), ic(0), 0])[1]).unwrap();
    mask[i] = vop3(0x365, 0, [ic(3), ic(0), 0])[1];
    assert!(translate(&ms(&mask, mesh_info(128, 128, 128, 2), pos_param_outs())).err().expect("refusal").contains("mask other than all lanes"));
    let mut a = ms(&base, mesh_info(128, 128, 128, 2), pos_param_outs());
    a.stage = Stage::Amplification;
    let e = translate(&a).err().expect("refusal");
    assert!(e.contains("s_sendmsg outside a geometry shader"), "{e}");
}

#[test]
fn mesh_divergent_barrier_with_lds_is_refused() {
    let mut w = vec![];
    w.push(vopc(0xd2, ic(0), 0));
    w.push(sopp(8, 1));
    w.push(sopp(0x0a, 0));
    w.extend([0xd834_0000, 0x0000_0100]);
    w.extend([sop1(3, M0, LIT), (1 << 12) | 3]);
    w.push(sopp(0x10, 9));
    w.extend(exp(12, 0xf, true, [0, 0, 0, 0]));
    w.push(S_ENDPGM);
    let e = translate(&ms(&w, mesh_info(64, 64, 64, 2), vec![sig("SV_Position", 1, 3, 0, 0xf)])).err().expect("refusal");
    assert!(e.contains("divergent path"), "{e}");
}

fn gta_dir() -> Option<std::path::PathBuf> {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../analysis/shaders/mesh");
    p.is_dir().then_some(p)
}

#[test]
#[cfg_attr(not(tombox_fixtures_mesh), ignore = "needs analysis/shaders/mesh fixtures extracted from local title dumps")]
fn gta_mesh_programs_extract_with_mesh_runtime_info() {
    let dir = gta_dir().expect("analysis/shaders/mesh fixtures");
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().map(|x| x != "dxbc").unwrap_or(true) {
            continue;
        }
        let ex = extract(&std::fs::read(&p).unwrap()).expect("extract");
        assert_eq!(ex.stage, Stage::Mesh, "{}", p.display());
        let m = ex.gs_info.and_then(|g| g.mesh).expect("mesh info");
        assert_eq!((m.stage, m.topology, m.prim_vectors, m.payload_bytes), (13, 2, 0, 0), "{}", p.display());
        assert!(m.max_vertices as u32 <= m.num_threads[0] && m.max_primitives as u32 <= m.num_threads[0], "{}", p.display());
        let insns = rdna2::disassemble(&ex.isa);
        assert!(insns.iter().any(|i| i.mnemonic == "s_sendmsg" && i.aux.imm & 0xf == 9), "{}", p.display());
        assert!(insns.iter().any(|i| i.mnemonic == "exp" && i.aux.imm == 20), "{}", p.display());
        assert!(!ex.has_root_sig, "{}", p.display());
        let r = translate(&ex);
        if let Err(e) = &r {
            assert!(!e.contains("mesh") && !e.contains("sendmsg") && !e.contains("export"), "{}: {e}", p.display());
        }
        n += 1;
    }
    assert!(n >= 3);
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

const PS: &str = "uint4 main(float4 p : SV_Position, nointerpolation uint2 t : TEXCOORD0) : SV_Target { return uint4(t.x, t.y, 0x77u, 0x88u); }";

const IID_DEVICE2: Guid = Guid(0x30baa41e, 0xb15b, 0x475c, [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28]);
const IID_LIST6: Guid = Guid(0xc3827890, 0xe548, 0x4cfa, [0x96, 0xcf, 0x56, 0x89, 0xa9, 0x37, 0x0f, 0x80]);

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

static WARP_RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());

unsafe fn render(words: &[u32], groups: u32) -> Result<[u32; 4], String> {
    render_mi(words, groups, mesh_info(128, 128, 128, 2))
}

unsafe fn render_mi(words: &[u32], groups: u32, mi: MeshInfo) -> Result<[u32; 4], String> {
    let t = translate(&ms(words, mi, pos_param_outs()))?;
    let msb = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("{e}\n{}", t.hlsl))?;
    let psb = dxc::compile(PS, "ps_6_5")?;
    let _warp = std::env::var_os("TOMBOX_EXEC_HW").is_none().then(|| WARP_RENDER.lock().unwrap_or_else(|e| e.into_inner()));
    let (mut gpu, kind) = mesh_gpu();
    eprintln!("mesh render on {kind}");
    let d3d12 = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
        core::mem::transmute(GetProcAddress(d3d12, b"D3D12SerializeRootSignature\0".as_ptr()));
    let desc = [0u8; 40];
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    assert!(ser(desc.as_ptr(), 1, &mut blob, &mut err) >= 0);
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(gpu.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, vcall!(blob, 3, fn() -> *const u8), vcall!(blob, 4, fn() -> usize), &d3d::IID_ROOTSIG, &mut rs);
    assert!(hr >= 0);
    let mut s = Vec::new();
    sub(&mut s, 0, &(rs as u64).to_le_bytes());
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
        return Err(format!("CreatePipelineState(mesh) hr={:#010x}{}\n{}", hr as u32, gpu.messages(), t.hlsl));
    }
    let mut td = [0u8; 0x38];
    td[0..4].copy_from_slice(&3u32.to_le_bytes());
    td[0x10..0x18].copy_from_slice(&1u64.to_le_bytes());
    td[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
    td[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
    td[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
    td[0x20..0x24].copy_from_slice(&3u32.to_le_bytes());
    td[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
    td[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
    let rt = gpu.committed(&td, d3d::default_heap(), 4)?;
    let rb = gpu.buffer(256, 3, d3d::COPY_DEST, false)?;
    let (_h, rtv, _, _) = gpu.heap(2, 1, false)?;
    vcall!(gpu.device, 20, fn(*mut c_void, *const c_void, usize) -> (), rt, core::ptr::null(), rtv);
    gpu.begin();
    let l = gpu.list;
    let clear = [5.0f32, 6.0, 7.0, 8.0];
    vcall!(l, 48, fn(usize, *const f32, u32, *const c_void) -> (), rtv, clear.as_ptr(), 0, core::ptr::null());
    vcall!(l, 46, fn(u32, *const usize, i32, *const usize) -> (), 1, &rtv, 0, core::ptr::null());
    vcall!(l, 30, fn(*mut c_void) -> (), rs);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    let vp = [0f32, 0., 1., 1., 0., 1.];
    vcall!(l, 21, fn(u32, *const f32) -> (), 1, vp.as_ptr());
    let sc = [0i32, 0, 1, 1];
    vcall!(l, 22, fn(u32, *const i32) -> (), 1, sc.as_ptr());
    let mut l6: *mut c_void = core::ptr::null_mut();
    assert!(vcall!(l, 0, fn(*const Guid, *mut *mut c_void) -> i32, &IID_LIST6, &mut l6) >= 0, "ID3D12GraphicsCommandList6");
    vcall!(l6, 79, fn(u32, u32, u32) -> (), groups, 1, 1);
    d3d::release(l6);
    gpu.barrier(rt, 4, d3d::COPY_SOURCE);
    let mut dst2 = [0u8; 48];
    dst2[0..8].copy_from_slice(&(rb as u64).to_le_bytes());
    dst2[8..12].copy_from_slice(&1u32.to_le_bytes());
    dst2[24..28].copy_from_slice(&3u32.to_le_bytes());
    dst2[28..32].copy_from_slice(&1u32.to_le_bytes());
    dst2[32..36].copy_from_slice(&1u32.to_le_bytes());
    dst2[36..40].copy_from_slice(&1u32.to_le_bytes());
    dst2[40..44].copy_from_slice(&256u32.to_le_bytes());
    let mut src = [0u8; 48];
    src[0..8].copy_from_slice(&(rt as u64).to_le_bytes());
    vcall!(l, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), dst2.as_ptr(), 0, 0, 0, src.as_ptr(), core::ptr::null());
    gpu.submit()?;
    let b = Gpu::map_read(rb, 16).ok_or("map readback")?;
    d3d::release(pso);
    d3d::release(dev2);
    let mut o = [0u32; 4];
    for i in 0..4 {
        o[i] = u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    }
    Ok(o)
}

#[test]
fn gpu_mesh_renders_console_abi_triangle() {
    let got = unsafe { render(&console_ms(67, 1, TRI), 3) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [2 * 128 + 64, 1, 0x77, 0x88]);
}

#[test]
fn gpu_mesh_divergent_barrier_without_memory_writes() {
    let w = console_ms_b(67, 1, TRI, true);
    let t = translate(&ms(&w, mesh_info(128, 128, 128, 2), pos_param_outs())).expect("translate");
    assert_eq!(t.hlsl.matches("GroupMemoryBarrierWithGroupSync();").count(), 3, "{}", t.hlsl);
    let got = unsafe { render(&w, 3) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [2 * 128 + 64, 1, 0x77, 0x88]);
}

#[test]
fn gpu_mesh_uniform_entry_counts_preserve_rendered_output() {
    let mut words = console_ms(67, 1, TRI);
    let guard = words.iter().position(|&w| w == sop1(4, 10, EXEC)).unwrap();
    let barrier = words.iter().position(|&w| w == sopp(0x0a, 0)).unwrap();
    words.drain(guard..=barrier);
    words.splice(..0, [sop1(3, M0, LIT), (1 << 12) | 67, sopp(0x10, 9)]);
    let shader = translate(&ms(&words, mesh_info(128, 128, 128, 2), pos_param_outs())).unwrap();
    assert!(!shader.hlsl.contains("GroupMemoryBarrierWithGroupSync();"));
    assert_eq!(unsafe { render(&words, 1) }.unwrap(), [64, 1, 0x77, 0x88]);
}

#[test]
fn gpu_mesh_lds_write_before_reconverged_barrier() {
    let shader = |divergent: bool| {
        let mut words = console_ms(67, 1, TRI);
        let insert = words.len() - 3;
        words.splice(insert..insert, [
            vop2(0x1a, 10, ic(2), 0), // Each writer gets its own LDS word.
            sop1(4, 10, EXEC),
            vopc(0xd2, ic(11), 0), // Only lane 11 writes.
            sopp(8, if divergent { 4 } else { 3 }),
            0xd834_0000, 10, // ds_write_b32 v10, v0
            sopp(0x0c, 0), // Wait for the writes.
            sopp(0x0a, 0),
            sop1(4, EXEC, 10),
            vop1(1, 10, ic(44)),
            0xd8d8_0000, 0x0700_000a, // ds_read_b32 v7, v10
            sopp(0x0c, 0),
        ]);
        words
    };
    let divergent = ms(&shader(true), mesh_info(128, 128, 128, 2), pos_param_outs());
    let err = translate(&divergent).err().expect("divergent LDS barrier is refused");
    assert!(err.contains("writes LDS or memory"), "{err}");

    let got = unsafe { render(&shader(false), 1) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [11, 1, 0x77, 0x88]);
}

#[test]
fn gpu_mesh_lds_with_export_between_barriers() {
    let shader = |conditional_first_barrier: bool| {
        let mut words = console_ms(67, 1, TRI);
        let insert = words.len() - 3;
        let mut body = vec![
            vop2(0x1a, 10, ic(2), 0),
            sop1(4, 10, EXEC),
            vopc(0xd2, ic(11), 0),
            sopp(8, if conditional_first_barrier { 6 } else { 3 }),
            0xd834_0000, 10, // Lane 11 writes value 11 to LDS word 11.
            sopp(0x0c, 0),
        ];
        body.push(sopp(0x0a, 0));
        if !conditional_first_barrier {
            body.push(sopp(8, 2)); // Reopen the same EXEC guard for the export.
        }
        body.extend(exp(12, 0, false, [0; 4]));
        body.extend([
            sop1(4, EXEC, 10),
            sopp(0x0a, 0),
            vop1(1, 10, ic(44)),
            0xd8d8_0000, 0x0700_000a, // Read LDS word 11 into the exported parameter.
            sopp(0x0c, 0),
        ]);
        words.splice(insert..insert, body);
        words
    };
    let conditional = ms(&shader(true), mesh_info(128, 128, 128, 2), pos_param_outs());
    let err = translate(&conditional).err().expect("conditional LDS barrier is refused");
    assert!(err.contains("writes LDS or memory"), "{err}");

    let uniform = shader(false);
    let t = translate(&ms(&uniform, mesh_info(128, 128, 128, 2), pos_param_outs())).unwrap();
    assert_eq!(t.hlsl.matches("GroupMemoryBarrierWithGroupSync();").count(), 5);
    let got = unsafe { render(&uniform, 1) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [11, 1, 0x77, 0x88]);
}

#[test]
fn mesh_alloc_and_lds_refusals() {
    let mut w = console_ms(67, 1, TRI);
    let i = w.iter().position(|x| *x == sopp(0x10, 9)).unwrap();
    w[i] = sopp(0, 0);
    let e = translate(&ms(&w, mesh_info(128, 128, 128, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("without s_sendmsg GS_ALLOC_REQ"), "{e}");
    let mut mi = mesh_info(128, 128, 128, 2);
    mi.groupshared_bytes = 32768;
    let e = translate(&ms(&console_ms(67, 1, TRI), mi, pos_param_outs())).err().expect("refusal");
    assert!(e.contains("groupshared memory"), "{e}");
}

#[test]
fn mesh_export_and_entry_count_refusals_are_explicit() {
    let base = console_ms(3, 1, 0);
    let with = |extra: [u32; 2]| {
        let mut w = base.clone();
        w.extend(extra);
        w
    };
    let e = translate(&ms(&with(exp(21, 0x1, false, [0, 0, 0, 0])), mesh_info(128, 128, 128, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("mesh export target 21 unsupported"), "{e}");
    let e = translate(&ms(&with(exp(13, 0x1, false, [0, 0, 0, 0])), mesh_info(128, 128, 128, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("position export 13.x has no clip/cull distance element"), "{e}");
    let layer = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_ViewportArrayIndex", 5, 1, 1, 0x1)];
    let e = translate(&ms(&with(exp(13, 0x1, false, [0, 0, 0, 0])), mesh_info(128, 128, 128, 2), layer)).err().expect("refusal");
    assert!(e.contains("position-export misc vector unsupported"), "{e}");
    let clip = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_ClipDistance", 2, 3, 1, 0x3)];
    let t = translate(&ms(&with(exp(13, 0x3, false, [0, 0, 0, 1])), mesh_info(128, 128, 128, 2), clip)).expect("clip-distance export 13");
    assert!(t.hlsl.contains("o.o1"), "{}", t.hlsl);
    let e = translate(&ms(&base, mesh_info(128, 0, 1, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("mesh shader with max vertices 0 / primitives 1"), "{e}");
    let mut mi = mesh_info(128, 128, 128, 2);
    mi.prim_vectors = 1;
    let mut ex = ms(&base, mi, pos_param_outs());
    ex.prim_sig = vec![sig("SV_PrimitiveID", 8, 1, 0, 0x1)];
    let e = translate(&ex).err().expect("refusal");
    assert!(e.contains("per-primitive system values unsupported"), "{e}");
    let mut w = base.clone();
    let end = w.len() - 1;
    w.splice(end..end, [0xd834_0000, 0x0000_0100]);
    let e = translate(&ms(&w, mesh_info(192, 3, 1, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("mesh shader with 192 threads and LDS unsupported"), "{e}");
    let mut w = base.clone();
    let end = w.len() - 1;
    w.splice(end..end, [0xd834_ffff, 0x0000_0100]);
    let e = translate(&ms(&w, mesh_info(128, 3, 1, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("LDS offset 65551 beyond the PC groupshared limit"), "{e}");
    let bare = vec![sop1(3, M0, LIT), 0x40018, sopp(0x10, 9), S_ENDPGM];
    let e = translate(&ms(&bare, mesh_info(128, 3, 1, 2), Vec::new())).err().expect("refusal");
    assert!(e.contains("mesh shader without vertex outputs"), "{e}");
}

#[test]
fn mesh_divergent_barrier_with_memory_store_is_refused() {
    let mut w = console_ms_b(67, 1, TRI, true);
    let end = w.len() - 1;
    w.splice(end..end, [0xdc70_8000, 0x007d_0100]);
    let insns = rdna2::disassemble(&w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>());
    assert!(insns.iter().any(|i| i.enc == rdna2::Enc::Flat && i.op == 0x1c), "{:?}", insns.iter().map(|i| i.mnemonic.clone()).collect::<Vec<_>>());
    let e = translate(&ms(&w, mesh_info(128, 128, 128, 2), pos_param_outs())).err().expect("refusal");
    assert!(e.contains("writes LDS or memory"), "{e}");
}

#[test]
fn gpu_mesh_culled_and_empty_outputs_draw_nothing() {
    let clear = [5, 6, 7, 8];
    let culled = unsafe { render(&console_ms(67, 1, TRI | 0x8000_0000), 1) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(culled, clear);
    let empty = unsafe { render(&console_ms(0, 0, TRI), 1) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(empty, clear);
    let one = unsafe { render(&console_ms(67, 1, TRI), 1) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(one, [64, 1, 0x77, 0x88]);
}

#[test]
fn gpu_mesh_merged_wave_info_fields() {
    let pre = [sop2(0x27, 4, 3, LIT), 0x4001c, sop2(0x0e, 5, 3, LIT), 0xffff];
    for (wave32, threads, counts, tg) in [(false, 128, 0x4040, 2), (true, 128, 0x2020, 4), (false, 96, 0x2020, 2), (true, 80, 0x1010, 3), (false, 256, 0x4040, 4), (true, 256, 0x2020, 8)] {
        let w = ms_prog((1 << 12) | 67, TRI, false, wave32, &pre, [5, 4]);
        let mi = mesh_info(threads, threads as u16, threads as u16, 2);
        let t = translate(&ms(&w, mi, pos_param_outs())).expect("translate");
        let ws = if wave32 { 32 } else { 64 };
        let gi = if threads > 128 { "tb_gi" } else { "gi" };
        let want = format!("s3 = {:#x}u | (({gi} / {ws}u) << 24) | (min({ws}u, {threads}u - ({gi} & {:#x}u)) * 0x101u);", tg << 28, !(ws - 1u32));
        assert!(t.hlsl.contains(&want), "missing `{want}` in\n{}", t.hlsl);
        let got = unsafe { render_mi(&w, 2, mi) }.unwrap_or_else(|e| panic!("wave{ws} x {threads}: {e}"));
        assert_eq!(got, [counts, tg, 0x77, 0x88], "wave{ws} x {threads}");
    }
}

#[test]
fn gpu_mesh_alloc_req_counts_at_limit_ignore_bits_outside_fields() {
    let w = ms_prog(0xff80_0800 | (128 << 12) | 128, TRI, false, false, &[], [2, 3]);
    let t = translate(&ms(&w, mesh_info(128, 128, 128, 2), pos_param_outs())).expect("translate");
    assert!(t.hlsl.contains("uint tb_nv = tb_mscnt & 0x3ffu, tb_np = (tb_mscnt >> 12) & 0x7ffu;"), "{}", t.hlsl);
    let got = unsafe { render(&w, 3) }.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [2 * 128 + 64, 1, 0x77, 0x88]);
}

fn rts0_blob(flags: u32) -> Vec<u8> {
    let body: Vec<u8> = [1u32, 0, 24, 0, 24, flags].iter().flat_map(|x| x.to_le_bytes()).collect();
    let total = (32 + 4 + 8 + body.len()) as u32;
    let mut v = b"DXBC".to_vec();
    v.extend_from_slice(&[0u8; 16]);
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&total.to_le_bytes());
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(&36u32.to_le_bytes());
    v.extend_from_slice(b"RTS0");
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(&body);
    v
}

#[test]
fn warp_rejects_the_console_force_memory_based_abi_root_flag() {
    unsafe {
        let d = warp_device();
        let create = |flags: u32| {
            let b = rts0_blob(flags);
            let mut rs: *mut c_void = core::ptr::null_mut();
            let hr = vcall!(d, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, b.as_ptr(), b.len(), &d3d::IID_ROOTSIG, &mut rs);
            if hr >= 0 {
                vcall!(rs, 2, fn() -> u32);
            }
            hr
        };
        assert!(create(0) >= 0, "control root signature");
        let hr = create(0x0100_0000);
        eprintln!("WARP CreateRootSignature with flag 0x01000000 -> {hr:#010x}");
        assert!(hr < 0, "PC accepted the console-only root signature flag bit 24");
    }
}

#[test]
fn compile_amplification_payload_subword_loads() {
    let mut words = Vec::new();
    for (op, offset) in [(8, 15), (10, 14)] {
        words.extend([0xdc00_0000 | (op << 18) | offset, (0x7d << 16) | (4 << 24)]);
    }
    words.push(S_ENDPGM);
    let mut info = mesh_info(1, 0, 0, 0);
    info.stage = 14;
    info.payload_bytes = 16;
    let mut shader = ms(&words, info, vec![]);
    shader.stage = Stage::Amplification;
    let t = translate(&shader).unwrap();
    for width in [1, 2] {
        assert!(t.hlsl.lines().any(|line| line.contains("tb_gload(") && line.contains(&format!(", {width}u)"))), "{}", t.hlsl);
    }
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).unwrap();
}

#[test]
fn mesh_256_threads_and_per_primitive_outputs_compile_with_dxc() {
    let mut w = console_ms(200, 1, TRI);
    w.pop();
    w.extend(exp(33, 0xf, true, [7, 8, 7, 8]));
    w.push(S_ENDPGM);
    let mut mi = mesh_info(256, 256, 128, 2);
    mi.prim_vectors = 1;
    let mut s = ms(&w, mi, pos_param_outs());
    s.prim_sig = vec![SigElement { name: "TEXCOORD".into(), index: 7, sysval: 0, comp_type: 1, register: 0, mask: 0xf, rw_mask: 0 }];
    let t = translate(&s).expect("256-thread mesh + per-primitive output");
    assert_eq!(t.profile, "ms_6_5");
    assert!(t.hlsl.contains("[numthreads(128, 1, 1)]"), "{}", t.hlsl);
    assert!(t.hlsl.contains("for (uint tb_iter = 0u; tb_iter < 2u; tb_iter++)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("uint tb_gi = gi + tb_iter * 128u;"), "{}", t.hlsl);
    assert!(t.hlsl.contains("uint4 p0 : TEXCOORD7;"), "{}", t.hlsl);
    assert!(t.hlsl.contains("tb_pout.p0 ="), "{}", t.hlsl);
    assert!(t.hlsl.contains("verts[gi + 128u] = tb_vo[1];"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("tris[gi + 128u]"), "mp=128 must not index tris[gi+128]:\n{}", t.hlsl);
    dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).expect("DXC compile 256-thread MS");

    let mut bad_lds = w.clone();
    bad_lds.insert(0, 0xd834_0000);
    bad_lds.insert(1, 0x0000_0000);
    let mut s_lds = ms(&bad_lds, mi, pos_param_outs());
    s_lds.prim_sig = s.prim_sig.clone();
    let err = translate(&s_lds).err().expect("256-thread MS with LDS must refuse");
    assert!(err.contains("LDS"), "{err}");

    let mut s_nopsg = ms(&w, mi, pos_param_outs());
    let err = translate(&s_nopsg).err().expect("prim_vectors without PSG1 must refuse");
    assert!(err.contains("PSG1"), "{err}");
    let _ = &mut s_nopsg;
}
