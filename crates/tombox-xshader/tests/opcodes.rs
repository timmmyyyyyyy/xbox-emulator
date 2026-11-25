use tombox_xshader::hlsl::translate;
use tombox_xshader::meta::{RootParam, RootParamKind, RootRange, RootSig, SigElement};
use tombox_xshader::rdna2::{disassemble, Opnd};
use tombox_xshader::{ShaderExtract, Stage};

const S_ENDPGM: u32 = 0xbf81_0000;
const EXP_MRT0: [u32; 2] = [0xf800_180f, 0x0302_0100];
const LIT: u32 = 0xff;

fn v(n: u32) -> u32 {
    256 + n
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

fn vop3(op: u32, vdst: u32, srcs: [u32; 3], abs: u32, neg: u32) -> [u32; 2] {
    [0xd400_0000 | (op << 16) | (abs << 8) | vdst, srcs[0] | (srcs[1] << 9) | (srcs[2] << 18) | (neg << 29)]
}

fn sop2(op: u32, sdst: u32, s0: u32, s1: u32) -> u32 {
    0x8000_0000 | (op << 23) | (sdst << 16) | (s1 << 8) | s0
}

fn sop1(op: u32, sdst: u32, s0: u32) -> u32 {
    0xbe80_0000 | (sdst << 16) | (op << 8) | s0
}

fn sopk(op: u32, sdst: u32, imm: u16) -> u32 {
    0xb000_0000 | (op << 23) | (sdst << 16) | imm as u32
}

fn sig(name: &str, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index: 0, sysval, comp_type, register, mask, rw_mask: 0 }
}

fn table(range_type: u32, num: u32) -> RootParam {
    RootParam {
        visibility: 5,
        kind: RootParamKind::Table(vec![RootRange { range_type, num, base_register: 0, space: 0, offset_in_table: u32::MAX }]),
    }
}

fn shader(words: &[u32], root: Option<RootSig>) -> ShaderExtract {
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
        has_root_sig: root.is_some(),
        input_sig: vec![],
        output_sig: vec![sig("SV_Target", 64, 3, 0, 0xf)],
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: None,
        root_sig: root,
    }
}

fn setup() -> Vec<u32> {
    vec![
        vop1(1, 1, LIT), 0x3fc0_0000,
        vop1(1, 2, LIT), 0xc020_0000,
        vop1(1, 3, LIT), 0x1234_5678,
        sop1(3, 0, LIT), 0x8765_4321,
        sop1(3, 1, LIT), 0xf000_000f,
        sop1(3, 2, LIT), 0x0000_0024,
        sop1(3, 3, LIT), 0x0000_0003,
    ]
}

fn program(body: &[u32]) -> Vec<u32> {
    let mut w = setup();
    w.extend_from_slice(body);
    w.extend_from_slice(&EXP_MRT0);
    w.push(S_ENDPGM);
    w
}

fn check(label: &str, words: &[u32], root: Option<RootSig>, expect: &[&str]) {
    let ex = shader(words, root);
    let t = translate(&ex).unwrap_or_else(|e| panic!("{label}: translate failed: {e}"));
    for e in expect {
        assert!(t.hlsl.contains(e), "{label}: missing {e:?} in\n{}", t.hlsl);
    }
    #[cfg(windows)]
    {
        assert!(tombox_xshader::dxc::available(), "{label}: DXC (dxcompiler.dll + dxil.dll) not found");
        let profile = tombox_xshader::dxc::profile_for(t.profile);
        let code = tombox_xshader::dxc::compile(&t.hlsl, &profile).unwrap_or_else(|e| panic!("{label}: dxc: {e}\n{}", t.hlsl));
        assert!(code.len() > 4 && &code[..4] == b"DXBC", "{label}: DXC returned no container");
    }
}

fn mnemonic(words: &[u32]) -> (String, Opnd) {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let i = disassemble(&isa).into_iter().next().expect("decodes");
    (i.mnemonic, i.dst.first().copied().unwrap_or(Opnd::Null))
}

#[test]
fn vopc_gfx10_compare_table() {
    let cases: &[(u32, &str, bool)] = &[
        (0x10, "v_cmpx_f_f32", true),
        (0x1e, "v_cmpx_nlt_f32", true),
        (0x91, "v_cmpx_lt_i32", true),
        (0x97, "v_cmpx_t_i32", true),
        (0xc8, "v_cmp_f_f16", false),
        (0xcf, "v_cmp_o_f16", false),
        (0xd0, "v_cmpx_f_u32", true),
        (0xd2, "v_cmpx_eq_u32", true),
        (0xd5, "v_cmpx_ne_u32", true),
        (0xd8, "v_cmpx_f_f16", true),
        (0xdf, "v_cmpx_o_f16", true),
        (0xe8, "v_cmp_u_f16", false),
        (0xed, "v_cmp_neq_f16", false),
        (0xef, "v_cmp_tru_f16", false),
        (0xf8, "v_cmpx_u_f16", true),
        (0xff, "v_cmpx_tru_f16", true),
        (0x89, "v_cmp_lt_i16", false),
        (0x8e, "v_cmp_ge_i16", false),
        (0x8f, "v_cmp_class_f16", false),
        (0x99, "v_cmpx_lt_i16", true),
        (0x9e, "v_cmpx_ge_i16", true),
        (0x9f, "v_cmpx_class_f16", true),
        (0xa0, "v_cmp_f_i64", false),
        (0xa7, "v_cmp_t_i64", false),
        (0xa9, "v_cmp_lt_u16", false),
        (0xae, "v_cmp_ge_u16", false),
        (0xb0, "v_cmpx_f_i64", true),
        (0xb7, "v_cmpx_t_i64", true),
        (0xb9, "v_cmpx_lt_u16", true),
        (0xbe, "v_cmpx_ge_u16", true),
        (0xe0, "v_cmp_f_u64", false),
        (0xe5, "v_cmp_ne_u64", false),
        (0xf0, "v_cmpx_f_u64", true),
        (0xf7, "v_cmpx_t_u64", true),
    ];
    for &(op, name, x) in cases {
        let (m, d) = mnemonic(&[vopc(op, v(1), 2)]);
        assert_eq!(m, name, "VOPC {op:#x}");
        assert_eq!(d, if x { Opnd::ExecLo } else { Opnd::VccLo }, "VOPC {op:#x} destination");
        let w = vop3(op, 106, [v(1), v(2), 0], 0, 0);
        let (m, d) = mnemonic(&w);
        assert_eq!(m, name, "VOP3 {op:#x}");
        if x {
            assert_eq!(d, Opnd::ExecLo, "VOP3 {op:#x} destination");
        }
    }
    assert_eq!(mnemonic(&[vop2(0x06, 0, v(1), 2)]).0, "v_mac_legacy_f32");
    assert_eq!(mnemonic(&[sop1(0x14, 4, 0)]).0, "s_ff1_i32_b64");
}

#[test]
fn cmpx_writes_exec_only() {
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("v_cmpx_eq_u32", vec![vopc(0xd2, v(1), 2)], vec!["exec_lo = (((exec_lo & 1u) != 0u) && (v1 == v2)) ? 1u : 0u;"]),
        ("v_cmpx_ne_u32", vec![vopc(0xd5, v(1), 2)], vec!["&& (v1 != v2)) ? 1u : 0u;"]),
        ("v_cmpx_gt_i32", vec![vopc(0x94, v(1), 2)], vec!["(asint(v1) > asint(v2))"]),
        ("v_cmpx_lt_f32", vec![vopc(0x11, v(1), 2)], vec!["(asfloat(v1) < asfloat(v2))"]),
        ("v_cmpx_lg_f32", vec![vopc(0x15, v(1), 2)], vec!["((asfloat(v1) < asfloat(v2)) || (asfloat(v1) > asfloat(v2)))"]),
        ("v_cmpx_nlg_f32", vec![vopc(0x1a, v(1), 2)], vec!["|| (asfloat(v1) == asfloat(v2)))"]),
        ("v_cmpx_neq_f32", vec![vopc(0x1d, v(1), 2)], vec!["|| ((asfloat(v1) < asfloat(v2)) || (asfloat(v1) > asfloat(v2))))"]),
        ("v_cmpx_gt_f16", vec![vopc(0xdc, v(1), 2)], vec!["(f16tof32((v1) & 0xffffu) > f16tof32((v2) & 0xffffu))"]),
        ("v_cmpx_nlt_f16", vec![vopc(0xfe, v(1), 2)], vec!["|| (f16tof32((v1) & 0xffffu) >= f16tof32((v2) & 0xffffu)))"]),
        ("v_cmpx_gt_f32 vop3 abs/neg", vop3(0x14, 0, [v(1), v(2), 0], 1, 2).to_vec(), vec!["(abs(asfloat(v1)) > (-asfloat(v2)))"]),
        ("v_cmpx_lt_i16", vec![vopc(0x99, v(1), 2)], vec!["((asint(((v1) & 0xffffu) << 16) >> 16) < (asint(((v2) & 0xffffu) << 16) >> 16))"]),
        ("v_cmpx_ge_u16", vec![vopc(0xbe, v(1), 2)], vec!["(((v1) & 0xffffu) >= ((v2) & 0xffffu))"]),
        ("v_cmpx_lt_i64", vec![vopc(0xb1, v(1), 4)], vec!["((asint(v2) < asint(v5)) || ((v2 == v5) && (v1 < v4)))"]),
        ("v_cmpx_ne_u64", vec![vopc(0xf5, v(1), 4)], vec!["((v2 != v5) || (v1 != v4))"]),
        ("v_cmpx_class_f16", vec![vopc(0x9f, v(1), 2)], vec!["tb_cmp_class16(((v1) & 0xffffu), v2)"]),
    ];
    for (label, body, expect) in cases {
        let mut b = body.clone();
        b.push(vop1(1, 0, v(3)));
        let t = translate(&shader(&program(&b), None)).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(!t.hlsl.contains("vcc_lo = (((exec_lo"), "{label}: cmpx must not write VCC\n{}", t.hlsl);
        check(label, &program(&b), None, &expect);
    }
}

#[test]
fn vop3_compare_applies_source_modifiers() {
    let b = vop3(0x04, 106, [v(1), v(2), 0], 2, 1);
    check("v_cmp_gt_f32 vop3", &program(&[b[0], b[1], vop1(1, 0, v(3))]), None, &["((-asfloat(v1)) > abs(asfloat(v2)))"]);
    let b = [vopc(0x05, v(1), 2), vop1(1, 0, v(3))];
    check("v_cmp_lg_f32", &program(&b), None, &["((asfloat(v1) < asfloat(v2)) || (asfloat(v1) > asfloat(v2)))"]);
    let b = [vopc(0x85, v(1), 2), vop1(1, 0, v(3))];
    check("v_cmp_ne_i32", &program(&b), None, &["(asint(v1) != asint(v2))"]);
    let b = vop3(0xa1, 106, [v(1), v(4), 0], 1, 0);
    let err = translate(&shader(&program(&[b[0], b[1], vop1(1, 0, v(3))]), None)).err().unwrap_or_default();
    assert!(err.contains("abs/neg on an integer compare"), "v_cmp_lt_i64 abs must be refused: {err}");
}

#[test]
fn new_vector_ops() {
    let lerp = vop3(0x14d, 0, [v(1), v(2), v(3)], 0, 0);
    let align = vop3(0x14f, 0, [v(1), v(2), v(3)], 0, 0);
    let ldexp = vop3(0x362, 0, [v(1), v(3), 0], 0, 0);
    let mac = vop3(0x106, 0, [v(1), v(2), 0], 0, 0);
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("v_mac_legacy_f32", vec![vop2(0x06, 0, v(1), 2)], vec!["tb_madlz(asfloat(v1), asfloat(v2), asfloat(v0))"]),
        ("v_mac_legacy_f32 vop3", mac.to_vec(), vec!["tb_madlz(asfloat(v1), asfloat(v2), asfloat(v0))"]),
        ("v_mul_hi_u32_u24", vec![vop2(0x0c, 0, v(1), 3)], vec!["tb_mulhi_u32(v1 & 0xffffffu, v3 & 0xffffffu)"]),
        ("v_mul_hi_i32_i24", vec![vop2(0x0a, 0, v(1), 3)], vec!["tb_mulhi_i32(asuint(asint(v1 << 8) >> 8), asuint(asint(v3 << 8) >> 8))"]),
        ("v_ffbh_i32", vec![vop1(0x3b, 0, v(3))], vec!["(31u - (uint)firstbithigh(asint(v3)))"]),
        ("v_cvt_off_f32_i4", vec![vop1(0x0e, 0, v(3))], vec!["((float)(asint(v3 << 28) >> 28) * 0.0625)"]),
        ("v_lerp_u8", lerp.to_vec(), vec!["((((v1 >> 24u) & 0xffu) + ((v2 >> 24u) & 0xffu) + ((v3 >> 24u) & 1u)) >> 1) << 24u)"]),
        ("v_alignbyte_b32", align.to_vec(), vec!["(((v3) & 3u) == 0u ? v2 : ((v2 >> (((v3) & 3u) * 8u)) | (v1 << (32u - ((v3) & 3u) * 8u))))"]),
        ("v_ldexp_f32", ldexp.to_vec(), vec!["tb_ldexp32(asfloat(v1), asint(v3))"]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), None, &expect);
    }
}

#[test]
fn new_scalar_ops() {
    let to_v0 = vop1(1, 0, 4);
    let to_v1 = vop1(1, 1, 5);
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("s_lshr_b64", vec![sop2(0x21, 4, 0, 2), to_v0, to_v1], vec!["((s0 >> ((s2 & 63u) & 31u)) | (s1 << (32u - ((s2 & 63u) & 31u))))", "scc = ((t"]),
        ("s_ashr_i64", vec![sop2(0x23, 4, 0, 2), to_v0, to_v1], vec!["asuint(asint(s1) >> 31)", "asuint(asint(s1) >> ((s2 & 63u) - 32u))"]),
        ("s_absdiff_i32", vec![sop2(0x2c, 4, 0, 1), to_v0], vec!["asuint(abs(asint(s0) - asint(s1)))", "scc = (t"]),
        ("s_pack_ll_b32_b16", vec![sop2(0x32, 4, 0, 1), to_v0], vec!["((s0 & 0xffffu) | (s1 << 16))"]),
        ("s_not_b64", vec![sop1(0x08, 4, 0), to_v0, to_v1], vec!["= ~s0;", "= ~s1;"]),
        ("s_cmov_b64", vec![sopc_eq(2, 3), sop1(0x06, 4, 0), to_v0, to_v1], vec!["(scc != 0u) ? s0 : s4;", "(scc != 0u) ? s1 : s5;"]),
        ("s_flbit_i32_b32", vec![sop1(0x15, 4, 0), to_v0], vec!["((s0) == 0u ? 0xffffffffu : (31u - firstbithigh(s0)))"]),
        ("s_ff1_i32_b64", vec![sop1(0x14, 4, 0), to_v0], vec!["((s0) != 0u ? firstbitlow(s0) : ((s1) != 0u ? 32u + firstbitlow(s1) : 0xffffffffu))"]),
        ("s_cmovk_i32", vec![sopc_eq(2, 3), sopk(0x02, 4, 0xfff0), to_v0], vec!["((scc != 0u) ? 0xfffffff0u : s4)"]),
        ("s_version", vec![sopk(0x01, 0, 0x0004), to_v0], vec![]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), None, &expect);
    }
}

#[test]
fn second_batch_decodes() {
    let v1: &[(u32, &str)] = &[
        (0x1b, "v_pipeflush"), (0x3f, "v_frexp_exp_i32_f32"), (0x40, "v_frexp_mant_f32"), (0x41, "v_clrexcp"),
        (0x57, "v_log_f16"), (0x58, "v_exp_f16"), (0x59, "v_frexp_mant_f16"), (0x5a, "v_frexp_exp_i16_f16"),
        (0x60, "v_sin_f16"), (0x61, "v_cos_f16"), (0x62, "v_sat_pk_u8_i16"), (0x63, "v_cvt_norm_i16_f16"),
        (0x64, "v_cvt_norm_u16_f16"),
    ];
    for &(op, n) in v1 {
        assert_eq!(mnemonic(&[vop1(op, 0, v(1))]).0, n, "VOP1 {op:#x}");
    }
    assert_eq!(mnemonic(&[vop2(0x3b, 0, v(1), 2)]).0, "v_ldexp_f16");
    assert_eq!(mnemonic(&[vop2(0x3c, 0, v(1), 2)]).0, "v_pk_fmac_f16");
    let v3: &[(u32, &str)] = &[
        (0x15a, "v_sad_u8"), (0x15d, "v_sad_u32"), (0x15e, "v_cvt_pk_u8_f32"), (0x368, "v_cvt_pknorm_i16_f32"),
        (0x36b, "v_cvt_pk_i16_i32"), (0x373, "v_mad_u32_u16"), (0x375, "v_mad_i32_i16"), (0x376, "v_sub_nc_i32"),
        (0x37f, "v_add_nc_i32"), (0x303, "v_add_nc_u16"), (0x314, "v_lshlrev_b16"), (0x311, "v_pack_b32_f16"),
        (0x340, "v_mad_u16"), (0x35e, "v_mad_i16"), (0x359, "v_med3_u16"),
    ];
    for &(op, n) in v3 {
        assert_eq!(mnemonic(&vop3(op, 0, [v(1), v(2), v(3)], 0, 0)).0, n, "VOP3 {op:#x}");
    }
    assert_eq!(mnemonic(&[sop2(0x33, 4, 0, 1)]).0, "s_pack_lh_b32_b16");
    assert_eq!(mnemonic(&[sop1(0x17, 4, 0)]).0, "s_flbit_i32");
    assert_eq!(mnemonic(&[sop1(0x0c, 4, 0)]).0, "s_brev_b64");
    assert_eq!(mnemonic(&[0xbf8e_0000]).0, "s_sleep");
}

#[test]
fn second_batch_vector_ops() {
    let o = |op: u32, srcs: [u32; 3]| vop3(op, 0, srcs, 0, 0).to_vec();
    let mut opsel_hi = vop3(0x30a, 0, [v(1), v(2), 0], 0, 0);
    opsel_hi[0] |= (1 << 11) | (1 << 14);
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("v_frexp_exp_i32_f32", vec![vop1(0x3f, 0, v(1))], vec!["(t", "- 126u"]),
        ("v_frexp_mant_f32", vec![vop1(0x40, 0, v(1))], vec!["0x3f000000u"]),
        ("v_frexp_exp_i16_f16", vec![vop1(0x5a, 0, v(1))], vec!["- 14u"]),
        ("v_frexp_mant_f16", vec![vop1(0x59, 0, v(1))], vec!["0x3800u"]),
        ("v_log_f16", vec![vop1(0x57, 0, v(1))], vec!["tb_t16(3u, ((v1) & 0xffffu))"]),
        ("v_exp_f16", vec![vop1(0x58, 0, v(1))], vec!["tb_t16(4u, "]),
        ("v_sin_f16", vec![vop1(0x60, 0, v(1))], vec!["tb_t16(5u, ((v1) & 0xffffu))"]),
        ("v_cos_f16", vec![vop1(0x61, 0, v(1))], vec!["tb_t16(6u, "]),
        ("v_sat_pk_u8_i16", vec![vop1(0x62, 0, v(3))], vec!["clamp(asint(v3) >> 16, 0, 255) << 8"]),
        ("v_cvt_norm_i16_f16", vec![vop1(0x63, 0, v(1))], vec!["32767u)"]),
        ("v_cvt_norm_u16_f16", vec![vop1(0x64, 0, v(1))], vec!["65535u)"]),
        ("v_pipeflush", vec![vop1(0x1b, 0, 0), vop1(1, 0, v(3))], vec![]),
        ("v_ldexp_f16", vec![vop2(0x3b, 0, v(1), 3)], vec!["ldexp(f16tof32((v1) & 0xffffu), (float)clamp(asint((v3) << 16) >> 16, -64, 64))"]),
        ("v_pk_fmac_f16", vec![vop2(0x3c, 0, v(1), 2)], vec!["f16tof32((v1) >> 16) * f16tof32((v2) >> 16) + f16tof32((v0) >> 16)"]),
        ("v_sad_u8", o(0x15a, [v(1), v(2), v(3)]), vec!["(max(((v1 >> 24u) & 0xffu), ((v2 >> 24u) & 0xffu)) - min("]),
        ("v_sad_hi_u8", o(0x15b, [v(1), v(2), v(3)]), vec!["<< 16) + v3)"]),
        ("v_sad_u16", o(0x15c, [v(1), v(2), v(3)]), vec!["((v1 >> 16u) & 0xffffu)"]),
        ("v_sad_u32", o(0x15d, [v(1), v(2), v(3)]), vec!["(((max(v1, v2) - min(v1, v2))) + v3)"]),
        ("v_cvt_pk_u8_f32", o(0x15e, [v(1), 0x81, v(3)]), vec!["(uint)clamp(tb_rne(t"]),
        ("v_cvt_pknorm_i16_f32", o(0x368, [v(1), v(2), 0]), vec!["clamp(t", "32767u)"]),
        ("v_cvt_pknorm_u16_f32", o(0x369, [v(1), v(2), 0]), vec!["saturate(t"]),
        ("v_cvt_pk_u16_u32", o(0x36a, [v(1), v(3), 0]), vec!["(min(v1, 0xffffu) | (min(v3, 0xffffu) << 16))"]),
        ("v_cvt_pk_i16_i32", o(0x36b, [v(1), v(3), 0]), vec!["clamp(asint(v3), -32768, 32767)) << 16"]),
        ("v_mad_u32_u16", o(0x373, [v(1), v(2), v(3)]), vec!["(((v1) & 0xffffu) * ((v2) & 0xffffu) + v3)"]),
        ("v_mad_i32_i16", o(0x375, [v(1), v(2), v(3)]), vec!["asuint((asint((v1) << 16) >> 16) * (asint((v2) << 16) >> 16) + asint(v3))"]),
        ("v_add_nc_i32", o(0x37f, [v(1), v(3), 0]), vec!["(v1 + v3)"]),
        ("v_sub_nc_i32", o(0x376, [v(1), v(3), 0]), vec!["(v1 - v3)"]),
        ("v_add_nc_u16", o(0x303, [v(1), v(3), 0]), vec!["(((v1) & 0xffffu) + ((v3) & 0xffffu))", ")) & 0xffffu)"]),
        ("v_sub_nc_u16", o(0x304, [v(1), v(3), 0]), vec!["(((v1) & 0xffffu) - ((v3) & 0xffffu))"]),
        ("v_mul_lo_u16", o(0x305, [v(1), v(3), 0]), vec!["(((v1) & 0xffffu) * ((v3) & 0xffffu))"]),
        ("v_lshrrev_b16", o(0x307, [v(1), v(3), 0]), vec!["(((v3) & 0xffffu) >> (((v1) & 0xffffu) & 15u))"]),
        ("v_ashrrev_i16", o(0x308, [v(1), v(3), 0]), vec!["asuint((asint((v3) << 16) >> 16) >> (((v1) & 0xffffu) & 15u))"]),
        ("v_lshlrev_b16", o(0x314, [v(1), v(3), 0]), vec!["(((v3) & 0xffffu) << (((v1) & 0xffffu) & 15u))"]),
        ("v_max_u16", o(0x309, [v(1), v(3), 0]), vec!["max(((v1) & 0xffffu), ((v3) & 0xffffu))"]),
        ("v_max_i16 opsel", opsel_hi.to_vec(), vec!["asuint(max((asint(v1) >> 16), (asint((v2) << 16) >> 16)))", "((v0 & 0xffffu) | ((("]),
        ("v_min_u16", o(0x30b, [v(1), v(3), 0]), vec!["min(("]),
        ("v_min_i16", o(0x30c, [v(1), v(3), 0]), vec!["asuint(min(("]),
        ("v_add_nc_i16", o(0x30d, [v(1), v(3), 0]), vec!["asuint(((asint((v1) << 16) >> 16) + (asint((v3) << 16) >> 16)))"]),
        ("v_sub_nc_i16", o(0x30e, [v(1), v(3), 0]), vec!["asuint(((asint((v1) << 16) >> 16) - (asint((v3) << 16) >> 16)))"]),
        ("v_mad_u16", o(0x340, [v(1), v(2), v(3)]), vec!["(((v1) & 0xffffu) * ((v2) & 0xffffu) + ((v3) & 0xffffu))"]),
        ("v_mad_i16", o(0x35e, [v(1), v(2), v(3)]), vec!["asuint(((asint((v1) << 16) >> 16) * (asint((v2) << 16) >> 16) + (asint((v3) << 16) >> 16)))"]),
        ("v_min3_i16", o(0x352, [v(1), v(2), v(3)]), vec!["asuint(min(min("]),
        ("v_min3_u16", o(0x353, [v(1), v(2), v(3)]), vec!["min(min("]),
        ("v_max3_i16", o(0x355, [v(1), v(2), v(3)]), vec!["asuint(max(max("]),
        ("v_max3_u16", o(0x356, [v(1), v(2), v(3)]), vec!["max(max("]),
        ("v_med3_i16", o(0x358, [v(1), v(2), v(3)]), vec!["asuint(max(min("]),
        ("v_med3_u16", o(0x359, [v(1), v(2), v(3)]), vec!["max(min("]),
        ("v_pack_b32_f16", vop3(0x311, 0, [v(1), v(2), 0], 0, 2).to_vec(), vec!["(((v1) & 0xffffu) | ((((v2) & 0xffffu) ^ 0x8000u) << 16))"]),
        ("v_cvt_pknorm_i16_f16", o(0x312, [v(1), v(2), 0]), vec!["f16tof32(((v1) & 0xffffu))", "32767u)"]),
        ("v_cvt_pknorm_u16_f16", o(0x313, [v(1), v(2), 0]), vec!["65535u)"]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), None, &expect);
    }
    let mut clamp = vop3(0x37f, 0, [v(1), v(3), 0], 0, 0);
    clamp[0] |= 1 << 15;
    check("v_add_nc_i32 clamp", &program(&clamp), None, &["0x80000000u : 0x7fffffffu"]);
}

#[test]
fn second_batch_scalar_ops() {
    let to_v0 = vop1(1, 0, 4);
    let to_v1 = vop1(1, 1, 5);
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("s_pack_lh_b32_b16", vec![sop2(0x33, 4, 0, 1), to_v0], vec!["((s0 & 0xffffu) | (s1 & 0xffff0000u))"]),
        ("s_pack_hh_b32_b16", vec![sop2(0x34, 4, 0, 1), to_v0], vec!["((s0 >> 16) | (s1 & 0xffff0000u))"]),
        ("s_bfe_u64", vec![sop2(0x29, 4, 0, 2), to_v0, to_v1], vec!["min((s2 >> 16) & 127u, 64u)"]),
        ("s_bfe_i64", vec![sop2(0x2a, 4, 0, 2), to_v0, to_v1], vec!["(64u - t"]),
        ("s_brev_b64", vec![sop1(0x0c, 4, 0), to_v0, to_v1], vec!["reversebits(s1)", "reversebits(s0)"]),
        ("s_bcnt0_i32_b32", vec![sop1(0x0d, 4, 0), to_v0], vec!["(32u - countbits(s0))"]),
        ("s_bcnt0_i32_b64", vec![sop1(0x0e, 4, 0), to_v0], vec!["(64u - countbits(s0) - countbits(s1))"]),
        ("s_ff0_i32_b32", vec![sop1(0x11, 4, 0), to_v0], vec!["firstbitlow(~(s0))"]),
        ("s_ff0_i32_b64", vec![sop1(0x12, 4, 0), to_v0], vec!["32u + firstbitlow(~(s1))"]),
        ("s_flbit_i32_b64", vec![sop1(0x16, 4, 0), to_v0], vec!["(63u - firstbithigh(s0))"]),
        ("s_flbit_i32", vec![sop1(0x17, 4, 0), to_v0], vec!["(31u - (uint)firstbithigh(asint(s0)))"]),
        ("s_flbit_i32_i64", vec![sop1(0x18, 4, 0), to_v0], vec!["? 32u :"]),
        ("s_bitset0_b64", vec![sop1(0x1c, 4, 2), to_v0, to_v1], vec!["= s4 & ~("]),
        ("s_bitset1_b64", vec![sop1(0x1e, 4, 2), to_v0, to_v1], vec!["= s5 | ("]),
        ("s_orn1_saveexec_b64", vec![sop1(0x38, 4, 0), to_v0], vec!["(~s0 | exec_lo)"]),
        ("s_orn2_saveexec_b32", vec![sop1(0x40, 4, 0), to_v0], vec!["(s0 | ~exec_lo)"]),
        ("s_nand_saveexec_b32", vec![sop1(0x41, 4, 0), to_v0], vec!["(~(s0 & exec_lo)) & 1u"]),
        ("s_xnor_saveexec_b64", vec![sop1(0x2b, 4, 0), to_v0, to_v1], vec!["(~(s0 ^ exec_lo)) & 1u", "s5 = 0u"]),
        ("s_andn1_wrexec_b32", vec![sop1(0x46, 4, 0), to_v0], vec!["(~s0 & exec_lo) & 1u", "exec_lo = t"]),
        ("s_andn2_wrexec_b64", vec![sop1(0x3a, 4, 0), to_v0, to_v1], vec!["(s0 & ~exec_lo) & 1u", "s5 = 0u"]),
        ("s_bitcmp1_b64", vec![0xbf0f_0200, to_v0], vec!["(s2) & 63u) >= 32u ? (s1 >>"]),
        ("s_bitcmp0_b64", vec![0xbf0e_0200, to_v0], vec!["& 1u) == 0u)"]),
        ("s_sleep / s_setprio / s_icache_inv", vec![0xbf8e_0001, 0xbf8f_0000, 0xbf93_0000, 0xbf8b_0000, 0xbf94_0000, 0xbf95_0000, to_v0], vec![]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), None, &expect);
    }
}

#[test]
fn buffer_sub_dword_and_signed_atomics() {
    let root = || Some(RootSig { version: 2, flags: 1, params: vec![table(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() });
    let load_desc = [0xf408_0004u32, 0xfa00_0000];
    let cases: Vec<(&str, [u32; 2], Vec<&str>)> = vec![
        ("buffer_load_ubyte", [0xe020_1005, 0x8000_0401], vec!["& 0xffu)"]),
        ("buffer_load_sbyte", [0xe024_1005, 0x8000_0401], vec![">> 24)"]),
        ("buffer_load_ushort", [0xe028_1006, 0x8000_0401], vec!["& 0xffffu)"]),
        ("buffer_load_sshort", [0xe02c_1006, 0x8000_0401], vec![">> 16)"]),
        ("buffer_store_byte", [0xe060_1005, 0x8000_0301], vec!["InterlockedAnd(ubuf0[", "InterlockedOr(ubuf0["]),
        ("buffer_store_short", [0xe068_1006, 0x8000_0301], vec!["0xffffu) << t"]),
        ("buffer_atomic_smax", [0xe0dc_4000, 0x8000_0300], vec!["asuint(max(asint(t", "InterlockedCompareExchange("]),
        ("buffer_atomic_smin", [0xe0d4_4000, 0x8000_0300], vec!["asuint(min(asint(t"]),
        ("buffer_atomic_cmpswap", [0xe0c4_4000, 0x8000_0200], vec!["InterlockedCompareExchange(utb0[0u], v3, v2, t"]),
        ("buffer_atomic_inc", [0xe0f0_4000, 0x8000_0300], vec![">= v3) ? 0u : ("]),
        ("buffer_atomic_dec", [0xe0f4_4000, 0x8000_0300], vec!["== 0u) || ("]),
    ];
    for (label, op, expect) in cases {
        let body = [load_desc[0], load_desc[1], op[0], op[1], vop1(1, 0, v(4))];
        check(label, &program(&body), root(), &expect);
    }
}

#[test]
fn image_get_resinfo_queries_dimensions() {
    let root = Some(RootSig { version: 2, flags: 1, params: vec![table(0, 64)], num_static_samplers: 0, static_samplers: Vec::new() });
    let body = [0xf40c_0004, 0xfa00_0000, 0xf038_0f08, 0x0000_0401, vop1(1, 0, v(4)), vop1(1, 1, v(7))];
    check("image_get_resinfo", &program(&body), root.clone(), &["tex0.GetDimensions(v1, t"]);
    let cube = [0xf40c_0004, 0xfa00_0000, 0xf038_0f18, 0x0000_0401, vop1(1, 0, v(4)), vop1(1, 1, v(6))];
    check("image_get_resinfo cube", &program(&cube), root, &["TextureCube<float4> tex0", "z = 6u;"]);
}

#[test]
fn or_saveexec_integer_wave_sum() {
    let body = [
        0xbe84_25c1, 0xd501_0002, 0x0012_0280,
        0x4a04_04e9, 0xde54_c102, 0x4a04_04e9, 0x9360_1202, 0x4a04_04e9, 0x0009_2402, 0x4a04_04fa, 0xff09_4002,
        0xd778_0003, 0x0201_0102, 0x4a04_0503, 0xd760_0000, 0x0001_4102, 0x4a04_0400, 0xd760_0000, 0x0001_0102,
        0xbefe_0404, vop1(1, 0, 0),
    ];
    check("wave sum", &program(&body), None, &["asuint(WaveActiveSum(((s4 & 1u) != 0u) ? (v1) : (0x0u)))"]);
}

#[test]
fn exclusive_wave_prefix_sum() {
    let body = [
        0xbe84_25c1, 0xd501_0002, 0x0012_0280, 0x7e06_0302,
        0x4a06_06fa, 0xff09_1103, 0x4a06_06fa, 0xff01_1203, 0x4a06_06fa, 0xff01_1403, 0x4a06_06fa, 0xff01_1803,
        0xbefe_03ff, 0xffff_0000, 0xbeff_03ff, 0xffff_0000, 0xd778_4804, 0x0305_8303, 0x4a06_0704, 0xd760_0003, 0x0001_3f03,
        0xbefe_0380, 0xbeff_03c1, 0x4a06_0603, 0xbefe_03c1, 0xbeff_03c1, 0x4c06_0503, 0xbefe_0404, vop1(1, 0, v(3)),
    ];
    check("prefix sum", &program(&body), None, &["= WavePrefixSum(v2);"]);
}

#[test]
fn permlanes_read_other_lanes() {
    for (op, row) in [(0x377u32, "(t0l & ~15u)"), (0x378, "((t0l & ~15u) ^ 16u)")] {
        let mut body = vop3(op, 4, [v(1), v(2), v(3)], 0, 0).to_vec();
        body.push(vop1(1, 0, v(4)));
        assert_eq!(mnemonic(&vop3(op, 4, [v(1), v(2), v(3)], 0, 0)).0, if op == 0x377 { "v_permlane16_b32" } else { "v_permlanex16_b32" });
        check("permlane", &program(&body), None, &[row, "WaveReadLaneAt(v1, t0s)", "(t0a ? t0v : v4)"]);
    }
}

#[test]
fn double_precision_ops() {
    let o = |op: u32, srcs: [u32; 3]| vop3(op, 4, srcs, 0, 0).to_vec();
    let vcc = |op: u32, srcs: [u32; 3]| {
        let mut w = vop3(op, 4, srcs, 0, 0);
        w[0] |= 106 << 8;
        w.to_vec()
    };
    let out = vop1(1, 0, v(4));
    let cases: Vec<(&str, Vec<u32>, &str)> = vec![
        ("v_add_f64", o(0x164, [v(0), v(2), 0]), "(asdouble(v0, v1) + asdouble(v2, v3))"),
        ("v_mul_f64", o(0x165, [v(0), v(2), 0]), "(asdouble(v0, v1) * asdouble(v2, v3))"),
        ("v_fma_f64", o(0x14c, [v(0), v(2), v(0)]), "fma(asdouble(v0, v1), asdouble(v2, v3), asdouble(v0, v1))"),
        ("v_min_f64", o(0x166, [v(0), 0xf2, 0]), "min(asdouble(v0, v1), (1.0L))"),
        ("v_max_f64", o(0x167, [v(0), 0x81, 0]), "max(asdouble(v0, v1), asdouble(0x1u, 0x0u))"),
        ("v_ldexp_f64", o(0x168, [v(0), 0x82, 0]), "tb_ldexp64(asdouble(v0, v1), asint(0x2u))"),
        ("v_div_scale_f64", vcc(0x16e, [v(0), v(2), v(0)]), "tb_div_scale64(asdouble(v0, v1), asdouble(v2, v3), asdouble(v0, v1), t"),
        ("v_div_fmas_f64", o(0x170, [v(0), v(2), v(0)]), "tb_div_fmas64(asdouble(v0, v1), asdouble(v2, v3), asdouble(v0, v1), vcc_lo)"),
        ("v_div_fixup_f64", o(0x160, [v(0), v(2), v(0)]), "tb_div_fixup64("),
        ("v_trig_preop_f64", o(0x174, [v(0), 0x81, 0]), "tb_trig_preop64(asdouble(v0, v1), 0x1u)"),
        ("v_mad_u64_u32", vcc(0x176, [v(1), v(2), v(0)]), "tb_mulhi_u32(v1, v2)"),
        ("v_mad_i64_i32", vcc(0x177, [v(1), v(2), v(0)]), "tb_mulhi_i32(v1, v2)"),
        ("v_sqrt_f64", vec![vop1(0x34, 4, v(0))], "tb_t64(2u, "),
        ("v_rsq_f64", vec![vop1(0x31, 4, v(0))], "tb_t64(1u, "),
        ("v_rcp_f64", vec![vop1(0x2f, 4, v(0))], "tb_t64(0u, "),
        ("v_floor_f64", vec![vop1(0x1a, 4, v(0))], "tb_floor64("),
        ("v_ceil_f64", vec![vop1(0x18, 4, v(0))], "tb_ceil64("),
        ("v_trunc_f64", vec![vop1(0x17, 4, v(0))], "tb_trunc64("),
        ("v_rndne_f64", vec![vop1(0x19, 4, v(0))], "tb_rndne64("),
        ("v_fract_f64", vec![vop1(0x3e, 4, v(0))], "tb_fract64("),
        ("v_frexp_mant_f64", vec![vop1(0x3d, 4, v(0))], "tb_frexp_mant64("),
        ("v_frexp_exp_i32_f64", vec![vop1(0x3c, 4, v(0))], "asuint(tb_frexp_exp64("),
        ("v_cvt_f64_f32", vec![vop1(0x10, 4, v(1))], "(double)(asfloat(v1))"),
        ("v_cvt_f32_f64", vec![vop1(0x0f, 4, v(0))], "(float)t"),
        ("v_cvt_i32_f64", vec![vop1(0x03, 4, v(0))], "-2147483648.0L"),
        ("v_cvt_u32_f64", vec![vop1(0x15, 4, v(0))], "4294967295.0L"),
        ("v_cvt_f64_i32", vec![vop1(0x04, 4, v(3))], "(double)asint(v3)"),
        ("v_cvt_f64_u32", vec![vop1(0x16, 4, v(3))], "(double)(v3)"),
        ("v_cmp_lt_f64", vec![vopc(0x21, v(0), 2), vop1(1, 4, 106)], "(asdouble(v0, v1) < asdouble(v2, v3))"),
        ("v_cmpx_lt_f64", vec![vopc(0x31, v(0), 2), vop1(1, 4, v(1))], "exec_lo = (((exec_lo & 1u) != 0u) && ((asdouble(v0, v1) < asdouble(v2, v3)) && (!"),
        ("v_cmp_class_f64", vec![vopc(0xa8, v(0), 3), vop1(1, 4, 106)], "tb_cmp_class64(v0, v1, v3)"),
        ("v_cmpx_class_f32", vec![vopc(0x98, v(1), 3), vop1(1, 4, v(1))], "tb_cmp_class(v1, v3)"),
        ("v_msad_u8", o(0x171, [v(1), v(2), v(3)]), "((((v2 >> 0u) & 0xffu) == 0u) ? 0u :"),
        ("v_qsad_pk_u16_u8", o(0x172, [v(0), v(1), v(2)]), "0xffffu) | (("),
        ("v_mqsad_pk_u16_u8", o(0x173, [v(0), v(1), v(2)]), "== 0u) ? 0u : (max("),
        ("v_mqsad_u32_u8", o(0x175, [v(0), v(1), v(2)]), "s3 = v5;"),
        ("v_mullit_f32", o(0x150, [v(1), v(2), v(3)]), "-3.402823466e+38"),
    ];
    for (label, mut body, expect) in cases {
        assert_eq!(mnemonic(&body).0, label.split(' ').next().unwrap(), "{label}");
        body.push(out);
        check(label, &program(&body), None, &[expect]);
    }
}

fn snowdrop_root() -> Option<RootSig> {
    let unb = |range_type: u32, space: u32| RootRange { range_type, num: u32::MAX, base_register: 0, space, offset_in_table: 0 };
    let one = |range_type: u32, base_register: u32, offset_in_table: u32| RootRange { range_type, num: 1, base_register, space: 0, offset_in_table };
    Some(RootSig {
        version: 2,
        flags: 0x1d,
        params: vec![
            RootParam { visibility: 0, kind: RootParamKind::Descriptor { kind: 2, register: 0, space: 2 } },
            RootParam { visibility: 0, kind: RootParamKind::Table(vec![unb(0, 540), unb(0, 541), unb(1, 2300)]) },
            RootParam { visibility: 0, kind: RootParamKind::Table(vec![one(2, 1760, 0), one(2, 1780, 1)]) },
            RootParam { visibility: 0, kind: RootParamKind::Descriptor { kind: 3, register: 2060, space: 0 } },
        ],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    })
}

#[test]
fn snowdrop_root_cbv_descriptor() {
    let body = [sop1(3, 0, 8), sop2(0x10, 1, 9, LIT), 0x0010_0000, sop1(3, 2, 0xc1), 0xf420_0100, 0xfa00_0010, vop1(1, 0, 4)];
    check("root cbv", &program(&body), snowdrop_root(), &["register(b0, space2)", "cb131072[1].x"]);
}

#[test]
fn snowdrop_root_srv_descriptor() {
    let body = [sop1(3, 4, 12), sop2(0x10, 5, 13, LIT), 0x0001_0000, sop1(3, 6, 0xc1), sop1(3, 7, 3), 0xe030_1000, 0x8001_0401, vop1(1, 0, v(4))];
    check("root srv", &program(&body), snowdrop_root(), &["StructuredBuffer<TbElem_sbuf2060> sbuf2060 : register(t2060);"]);
}

#[test]
fn snowdrop_multi_range_cbv_table() {
    let body = [sop1(3, 2, 11), sop1(3, 3, 0x80), 0xf408_0501, 0xfa00_0020, 0xf420_060a, 0xfa00_0000, vop1(1, 0, 24)];
    check("cbv table", &program(&body), snowdrop_root(), &["cbuffer CB1780 : register(b1780)"]);
}

#[test]
fn snowdrop_bindless_texture() {
    let body = [sop1(3, 2, 10), sop1(3, 3, 0x80), 0xf40c_0601, 0x0a00_0000, 0xf038_0f08, 0x0006_0401, vop1(1, 0, v(4))];
    check(
        "bindless",
        &program(&body),
        snowdrop_root(),
        &["Texture2D<float4> tb_bl10_0[] : register(t0, space540);", "tb_bi0 = s5 + 0x0u;", "tb_bl10_0[tb_bi0 / 32u - 0u]"],
    );
}

#[test]
fn hardware_register_access() {
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("s_getreg_b32 MODE", vec![sopk(0x12, 4, 0xf801), vop1(1, 0, 4)], vec!["static uint tb_hw1 = 0xf0u;", "tb_hw1"]),
        ("s_setreg_b32 MODE[7:4]", vec![sopk(0x13, 0, 0x1901), sopk(0x12, 4, 0xf801), vop1(1, 0, 4)], vec!["tb_hw1 = (tb_hw1 & 0xffffff0fu) | ((s0 << 4u) & 0xf0u);"]),
        ("s_setreg_imm32_b32", vec![sopk(0x15, 0, 0x1901), 0x0000_0005, sopk(0x12, 4, 0xf801), vop1(1, 0, 4)], vec!["(0x5u << 4u)"]),
        ("s_getreg_b32 STATUS", vec![sopk(0x12, 4, 0xf802), vop1(1, 0, 4)], vec!["WaveActiveBallot((exec_lo & 1u) != 0u)", "WaveActiveBallot((vcc_lo & 1u) != 0u)"]),
        ("s_getreg_b32 SHADER_CYCLES", vec![sopk(0x12, 4, 0x981d), vop1(1, 0, 4)], vec!["tb_hw29 = (tb_hw29 + 4u) & 0xfffffu;"]),
        ("s_sleep advances SHADER_CYCLES", vec![0xbf8e_0002, sopk(0x12, 4, 0x981d), vop1(1, 0, 4)], vec!["tb_hw29 = (tb_hw29 + 128u) & 0xfffffu;"]),
        ("s_setprio sets STATUS.USER_PRIO", vec![0xbf8f_0003, sopk(0x12, 4, 0xf802), vop1(1, 0, 4)], vec!["tb_hw2 = (tb_hw2 & ~0x18u) | 0x18u;", "(tb_hw2 & 0x18u)"]),
        ("v_clrexcp", vec![0x7e00_8200, sopk(0x12, 4, 0xf803), vop1(1, 0, 4)], vec!["tb_hw3 &= ~0x1ffu;"]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), None, &expect);
    }
}

#[test]
fn lane_numbering_follows_guest_wave() {
    let readlane = vop3(0x360, 4, [v(1), 2, 0], 0, 0);
    let writelane = vop3(0x361, 5, [3, 2, 0], 0, 0);
    let dpp = [vop1(1, 6, 250), 1 | (0x111 << 8) | (0xf << 24) | (0x2 << 28)];
    let body = |wave64: bool| {
        let mut b = if wave64 { vec![sop1(4, 126, 0xc1)] } else { vec![] };
        b.extend_from_slice(&readlane);
        b.extend_from_slice(&writelane);
        b.extend_from_slice(&dpp);
        b.extend_from_slice(&[sopk(0x12, 7, 0xf802), vop1(1, 0, 4), vop1(1, 1, v(5)), vop1(1, 2, v(6)), vop1(1, 3, 7)]);
        program(&b)
    };
    assert_eq!(mnemonic(&readlane).0, "v_readlane_b32");
    assert_eq!(mnemonic(&writelane).0, "v_writelane_b32");
    check(
        "wave32 lanes",
        &body(false),
        None,
        &[
            "WaveReadLaneAt(v1, ((WaveGetLaneIndex() & ~31u) | ((s2) & 31u)))",
            "if (WaveGetLaneIndex() == ((WaveGetLaneIndex() & ~31u) | ((s2) & 31u))) v5 = t",
            "(0x2u >> ((t",
            "l >> 4) & 1u)) & 1u) != 0u",
            "WaveActiveBallot((exec_lo & 1u) != 0u)",
            "WaveGetLaneIndex() >> 5;",
        ],
    );
    check(
        "wave64 lanes",
        &body(true),
        None,
        &["WaveReadLaneAt(v1, ((s2) & 31u))", "if (WaveGetLaneIndex() == ((s2) & 31u)) v5 = t", "(0x2u >> (t", "l >> 4)) & 1u) != 0u", "WaveActiveAnyTrue((exec_lo & 1u) != 0u)"],
    );
    let w64 = translate(&shader(&body(true), None)).expect("wave64 translates").hlsl;
    assert!(!w64.contains("& ~31u") && !w64.contains("WaveActiveBallot"), "wave64 lowering changed:\n{w64}");
}

#[test]
fn mip_level_uav_access() {
    let root = || Some(RootSig { version: 2, flags: 1, params: vec![table(1, 64)], num_static_samplers: 0, static_samplers: Vec::new() });
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("image_store_mip", vec![0xf40c_0004, 0xfa00_0000, 0xf024_0f08, 0x0000_0401], vec!["uav0_m[16] : register(u0, space4000000);", "uav0_m[NonUniformResourceIndex(min("]),
        ("image_load_mip uav", vec![0xf40c_0004, 0xfa00_0000, 0xf004_0f08, 0x0000_0401, vop1(1, 0, v(4))], vec!["uav0_m[NonUniformResourceIndex(min("]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), root(), &expect);
    }
}

#[test]
fn bvh_intersect_ray_reads_guest_bvh() {
    let cases: Vec<(&str, Vec<u32>, Vec<&str>)> = vec![
        ("image_bvh_intersect_ray", vec![0xf198_0f01, 0x0000_1401, vop1(1, 0, v(20)), vop1(1, 1, v(23))], vec!["tb_bvh_intersect(uint4(s0, s1, s2, s3), (uint64_t)v1, asfloat(v2), asfloat(uint3(v3, v4, v5)), asfloat(uint3(v6, v7, v8)), asfloat(uint3(v9, v10, v11)));", "ByteAddressBuffer tb_gmem : register(t0, space4000001);"]),
        ("image_bvh64_intersect_ray", vec![0xf19c_0f01, 0x0000_1401, vop1(1, 0, v(20))], vec!["(((uint64_t)v2) << 32) | (uint64_t)v1, asfloat(v3)"]),
        ("image_bvh_intersect_ray a16", vec![0xf198_0f01, 0x4000_1401, vop1(1, 0, v(20))], vec!["float3(f16tof32(v6), f16tof32(v6 >> 16), f16tof32(v7))", "float3(f16tof32(v7 >> 16), f16tof32(v8), f16tof32(v8 >> 16))"]),
    ];
    for (label, body, expect) in cases {
        check(label, &program(&body), None, &expect);
    }
}

fn sopc_eq(a: u32, b: u32) -> u32 {
    0xbf00_0000 | (0x06 << 16) | (b << 8) | a
}

#[test]
fn image_get_lod_reads_both_lods() {
    let root = Some(RootSig { version: 2, flags: 1, params: vec![table(0, 64), table(3, 16)], num_static_samplers: 0, static_samplers: Vec::new() });
    let body = [
        0xf40c_0004, 0xfa00_0000,
        sop1(3, 10, 9),
        0xf408_0305, 0xfa00_0000,
        0xf180_0308, 0x0060_0401,
        vop1(1, 0, v(4)),
        vop1(1, 1, v(5)),
    ];
    check(
        "image_get_lod",
        &program(&body),
        root,
        &["tex0.CalculateLevelOfDetail(samp0, float2(asfloat(v1), asfloat(v2)))", "tex0.CalculateLevelOfDetailUnclamped(samp0,"],
    );
}

fn flat_dword0(op: u32, seg: u32, dlc: u32, glc: u32, slc: u32, offset: i32) -> u32 {
    0xdc00_0000 | (op << 18) | (seg << 14) | (dlc << 12) | (glc << 16) | (slc << 17) | (offset as u32 & 0xfff)
}

fn decode_flat(dword0: u32) -> (String, i64, u8) {
    let isa: Vec<u8> = [dword0, 0x7f01_0102u32].iter().flat_map(|w| w.to_le_bytes()).collect();
    let ins = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().expect("one instruction");
    assert_eq!(ins.dwords, 2, "{dword0:#010x}");
    (ins.mnemonic, ins.aux.imm, ins.aux.mask)
}

#[test]
fn flat_global_and_scratch_offsets_do_not_take_the_dlc_bit() {
    for dlc in [0u32, 1] {
        for (glc, slc) in [(0u32, 0u32), (1, 1)] {
            for off in [0i32, 4, 0x7ff, -1, -4, -0x800] {
                let (m, imm, seg) = decode_flat(flat_dword0(0x0c, 2, dlc, glc, slc, off));
                assert_eq!((m.as_str(), imm, seg), ("flat_load_dword", off as i64, 2), "global dlc={dlc} glc={glc} slc={slc} offset {off}");
                let (m, imm, seg) = decode_flat(flat_dword0(0x0c, 1, dlc, glc, slc, off));
                assert_eq!((m.as_str(), imm, seg), ("flat_load_dword", off as i64, 1), "scratch dlc={dlc} glc={glc} slc={slc} offset {off}");
            }
            for off in [0i32, 4, 0x7ff] {
                let (m, imm, seg) = decode_flat(flat_dword0(0x0c, 0, dlc, glc, slc, off));
                assert_eq!((m.as_str(), imm, seg), ("flat_load_dword", off as i64, 0), "flat dlc={dlc} glc={glc} slc={slc} offset {off}");
            }
        }
    }
}

fn vop3p(op: u32, vdst: u32, srcs: [u32; 3], opsel_hi: u32, neg: u32, clamp: bool) -> [u32; 2] {
    [
        0xcc00_0000 | (op << 16) | ((clamp as u32) << 15) | (((opsel_hi >> 2) & 1) << 14) | vdst,
        srcs[0] | (srcs[1] << 9) | (srcs[2] << 18) | ((opsel_hi & 3) << 27) | (neg << 29),
    ]
}

#[test]
fn vop3p_int16_names_follow_the_rdna2_opcode_table() {
    let names = [
        "v_pk_mad_i16", "v_pk_mul_lo_u16", "v_pk_add_i16", "v_pk_sub_i16", "v_pk_lshlrev_b16", "v_pk_lshrrev_b16", "v_pk_ashrrev_i16",
        "v_pk_max_i16", "v_pk_min_i16", "v_pk_mad_u16", "v_pk_add_u16", "v_pk_sub_u16", "v_pk_max_u16", "v_pk_min_u16",
    ];
    for (op, want) in names.iter().enumerate() {
        let w = vop3p(op as u32, 4, [v(1), v(2), v(3)], 7, 0, false);
        assert_eq!(mnemonic(&w).0, *want, "VOP3P opcode {op:#x}");
        check(want, &program(&vop3p(op as u32, 0, [v(1), v(2), v(3)], 7, 0, false)), None, &[]);
    }
}

#[test]
fn vop3p_int16_refuses_modifiers_without_defined_integer_semantics() {
    let err = |w: [u32; 2]| translate(&shader(&program(&w), None)).err().unwrap_or_default();
    assert!(err(vop3p(0x0a, 0, [v(1), v(2), 0], 7, 0b010, false)).contains("NEG/NEG_HI on an integer packed op"));
    assert!(err(vop3p(0x01, 0, [v(1), v(2), 0], 7, 0, true)).contains("CLAMP on an integer packed op"));
    assert!(err(vop3p(0x04, 0, [v(1), v(2), 0], 7, 0, true)).contains("CLAMP on an integer packed op"));
    check("v_pk_add_u16", &program(&vop3p(0x0a, 0, [v(1), 242, 0], 7, 0, false)), None, &[]);
    for op in [0x00, 0x09] {
        check("clamp", &program(&vop3p(op, 0, [v(1), v(2), v(3)], 7, 0, true)), None, &[]);
    }
    for op in [0x02, 0x03, 0x07, 0x0a, 0x0b, 0x0d] {
        check("clamp", &program(&vop3p(op, 0, [v(1), v(2), 0], 7, 0, true)), None, &[]);
    }
}

#[test]
fn packed_multi_sad_refuses_clamp() {
    for op in [0x172u32, 0x173] {
        let mut w = vop3(op, 0, [v(1), v(2), v(4)], 0, 0);
        w[0] |= 1 << 15;
        let err = translate(&shader(&program(&w), None)).err().unwrap_or_default();
        assert!(err.contains("CLAMP on a packed 16-bit multi-SAD is not lowered"), "op {op:#x}: {err}");
    }
    let mut w = vop3(0x175, 0, [v(1), v(2), v(4)], 0, 0);
    w[0] |= 1 << 15;
    check("v_mqsad_u32_u8 clamp", &program(&w), None, &[]);
}

#[test]
fn f32_div_helpers() {
    let fixup = vop3(0x15f, 0, [v(1), v(2), v(3)], 0, 0).to_vec();
    let fmas = vop3(0x16f, 0, [v(1), v(2), v(3)], 0, 0).to_vec();
    let mut scale = vop3(0x16d, 0, [v(1), v(2), v(3)], 0, 0);
    scale[0] |= 106 << 8;
    check("v_div_fixup_f32", &program(&fixup), None, &["tb_div_fixup32(asfloat(v1), asfloat(v2), asfloat(v3))"]);
    check("v_div_fmas_f32", &program(&fmas), None, &["tb_div_fmas32(asfloat(v1), asfloat(v2), asfloat(v3), vcc_lo)"]);
    check("v_div_scale_f32", &program(&scale), None, &["tb_div_scale32(asfloat(v1), asfloat(v2), asfloat(v3), t", "vcc_lo = (((exec_lo & 1u) != 0u) ? t"]);
}

#[test]
fn f32_division_helpers_refuse_a_shader_written_mode() {
    let set_round = [0xba80_1801u32, 1];
    for op in [0x16du32, 0x16f, 0x15f] {
        let mut body = set_round.to_vec();
        body.extend_from_slice(&vop3(op, 0, [v(1), v(2), v(3)], 0, 0));
        let err = translate(&shader(&program(&body), None)).err().unwrap_or_default();
        assert!(err.contains("under a shader-written single-precision MODE is not lowered"), "op {op:#x}: {err}");
        check("default MODE", &program(&vop3(op, 0, [v(1), v(2), v(3)], 0, 0)), None, &[]);
    }
}
