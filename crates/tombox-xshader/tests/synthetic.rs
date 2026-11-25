//! Translator regression tests on hand-assembled RDNA2 programs.
//!
//! These cover behaviour that was found wrong while running the title, without needing a
//! game shader as a fixture: each program is a few instructions written out as raw dwords,
//! wrapped in a `ShaderExtract` with a made-up signature.

use tombox_xshader::hlsl::translate;
use tombox_xshader::meta::{GsInfo, MeshInfo, PsvResource, RootParam, RootParamKind, RootRange, RootSig, SigElement, StaticSampler};
use tombox_xshader::{parse_root_signature, ShaderExtract, Stage};

const S_ENDPGM: u32 = 0xbf81_0000;
const S_NOP: u32 = 0xbf80_0000;
/// `v_cvt_pkrtz_f16_f32 v0, v2, v2` / `v1, v2, v2`: the default colour-export packing.
const PACK_V0_V2_V2: u32 = 0x5e00_0502;
const PACK_V1_V2_V2: u32 = 0x5e02_0502;
/// `exp mrt0 v0, v1` compressed, done, valid-mask.
const EXP_MRT0_COMPR: [u32; 2] = [0xf800_1c0f, 0x0000_0100];

fn sig(name: &str, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index: 0, sysval, comp_type, register, mask, rw_mask: 0 }
}

fn table(range_type: u32, num: u32, visibility: u32) -> RootParam {
    RootParam {
        visibility,
        kind: RootParamKind::Table(vec![RootRange { range_type, num, base_register: 0, space: 0, offset_in_table: u32::MAX }]),
    }
}

fn shader(stage: Stage, words: &[u32], input_sig: Vec<SigElement>, output_sig: Vec<SigElement>, root: Option<RootSig>) -> ShaderExtract {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderExtract {
        stage,
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
        input_sig,
        output_sig,
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: None,
        root_sig: root,
    }
}

#[test]
fn external_root_signature_binds_a_stripped_mesh_shader() {
    let words = [
        0xf420_0108,
        0xfa00_0000,
        0xbefc_0380,
        0xbf90_0009,
        S_ENDPGM,
    ];
    let output = vec![sig("SV_Position", 1, 3, 0, 0xf)];
    let mut ex = shader(Stage::Mesh, &words, vec![], output, None);
    ex.gs_info = Some(GsInfo {
        input_primitive: 0,
        output_topology: 0,
        max_vertex_count: 0,
        mesh: Some(MeshInfo {
            stage: 13,
            groupshared_bytes: 0,
            payload_bytes: 0,
            max_vertices: 3,
            max_primitives: 1,
            prim_vectors: 0,
            topology: 2,
            num_threads: [32, 1, 1],
        }),
        tess: None,
    });
    let Err(error) = translate(&ex) else {
        panic!("translation unexpectedly accepted the missing root descriptor");
    };
    assert!(error.contains("missing root signature for base s16 (root s16)"), "{error}");

    let body = [
        2u32, 1, 24, 0, 0, 0,
        2, 0, 36,
        5, 2, 0,
    ]
    .map(u32::to_le_bytes)
    .concat();
    let mut wrapped = b"DXBC".to_vec();
    wrapped.extend_from_slice(&[0; 16]);
    for value in [1u32, 44 + body.len() as u32, 1, 36] {
        wrapped.extend_from_slice(&value.to_le_bytes());
    }
    wrapped.extend_from_slice(b"RTS0");
    wrapped.extend_from_slice(&(body.len() as u32).to_le_bytes());
    wrapped.extend_from_slice(&body);
    for external in [&body, &wrapped] {
        ex.root_sig = Some(parse_root_signature(external).expect("parse external root signature"));
        assert!(!ex.has_root_sig);
        let translated = translate(&ex).expect("translate with external root signature");
        assert_eq!(translated.profile, "ms_6_5");
        assert!(translated.hlsl.contains("register(b5, space2)"), "{}", translated.hlsl);
    }
    assert!(parse_root_signature(&wrapped[..wrapped.len() - 1]).is_err());
    wrapped[36..40].copy_from_slice(b"TEST");
    assert!(parse_root_signature(&wrapped).is_err());
}

/// A pixel shader that writes the integer 5 to a `uint4` render target through the default
/// pack-and-export tail. Converting the integer through float-to-half (what the tail does
/// literally) would give 0; the integer itself must be exported.
#[test]
fn integer_colour_export_keeps_the_integer() {
    let words = [
        0x7e04_0285, // v_mov_b32 v2, 5
        S_NOP,
        S_NOP,
        PACK_V0_V2_V2,
        PACK_V1_V2_V2,
        EXP_MRT0_COMPR[0],
        EXP_MRT0_COMPR[1],
        S_ENDPGM,
    ];
    let ex = shader(Stage::Pixel, &words, vec![], vec![sig("SV_Target", 64, 1, 0, 0xf)], None);
    let t = translate(&ex).expect("translate");
    assert!(t.hlsl.contains("uint4 o0 : SV_Target0"), "{}", t.hlsl);
    assert!(t.hlsl.contains("o.o0 = (uint4)(uint4(tb_pk0a, tb_pk0b, tb_pk1a, tb_pk1b).xyzw);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("tb_pk0a = v2;"), "{}", t.hlsl);
    let body = t.hlsl.split("#define TB_RTFMT7 0u\n#endif").nth(1).unwrap_or(&t.hlsl);
    assert!(!body.contains("f16tof32"), "integer export must not go through half floats:\n{}", t.hlsl);
}

/// The same tail feeding a float render target exports the unpacked floats (the target's own
/// format conversion applies, not a fixed half-float one).
#[test]
fn float_colour_export_is_not_squeezed_through_half() {
    let words = [
        0x7e04_02f2, // v_mov_b32 v2, 1.0
        S_NOP,
        S_NOP,
        PACK_V0_V2_V2,
        PACK_V1_V2_V2,
        EXP_MRT0_COMPR[0],
        EXP_MRT0_COMPR[1],
        S_ENDPGM,
    ];
    let ex = shader(Stage::Pixel, &words, vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], None);
    let t = translate(&ex).expect("translate");
    assert!(
        t.hlsl.contains("o.o0 = (float4)(float4(tb_rtfmt(asfloat(tb_pk0a), TB_RTFMT0, 0u), tb_rtfmt(asfloat(tb_pk0b), TB_RTFMT0, 1u), tb_rtfmt(asfloat(tb_pk1a), TB_RTFMT0, 2u), tb_rtfmt(asfloat(tb_pk1b), TB_RTFMT0, 3u)).xyzw);"),
        "{}",
        t.hlsl
    );
}

#[test]
fn float_colour_export_follows_the_target_format() {
    let words = [
        0x7e04_02ff, 0x4974_2400,
        S_NOP,
        S_NOP,
        PACK_V0_V2_V2,
        PACK_V1_V2_V2,
        EXP_MRT0_COMPR[0],
        EXP_MRT0_COMPR[1],
        S_ENDPGM,
    ];
    let mut out = sig("SV_Target", 64, 3, 0, 0xf);
    out.index = 0;
    let ex = shader(Stage::Pixel, &words, vec![], vec![out], None);
    let t = translate(&ex).expect("translate");
    assert!(t.hlsl.contains("#ifndef TB_RTFMT0\n#define TB_RTFMT0 0u\n#endif"), "{}", t.hlsl);
    assert!(t.hlsl.contains("uint tb_f16rtz(float x)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("float tb_rtfmt(float x, uint k, uint c)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("tb_rtfmt(asfloat(tb_pk0a), TB_RTFMT0, 0u)"), "{}", t.hlsl);
    #[cfg(windows)]
    if tombox_xshader::dxc::available() {
        let profile = tombox_xshader::dxc::profile_for(t.profile);
        for defs in ["", "#define TB_RTFMT0 1u\n", "#define TB_RTFMT0 2u\n"] {
            let src = format!("{defs}{}", t.hlsl);
            tombox_xshader::dxc::compile(&src, &profile).unwrap_or_else(|e| panic!("dxc ({defs:?}): {e}\n{src}"));
        }
    }
}

#[test]
fn pkrtz_rounds_toward_zero() {
    let words = [
        0x7e04_02f2,
        0x5e06_0502,
        0x7e00_0303,
        0x7e02_0303,
        EXP_MRT0_COMPR[0],
        EXP_MRT0_COMPR[1],
        S_ENDPGM,
    ];
    let ex = shader(Stage::Pixel, &words, vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], None);
    let t = translate(&ex).expect("translate");
    assert!(t.hlsl.contains("(tb_f16rtz(asfloat(v2)) | (tb_f16rtz(asfloat(v2)) << 16))"), "{}", t.hlsl);
}

/// A packed export whose operands do not come from the pack instructions directly before it
/// keeps the literal behaviour (unpack the halves).
#[test]
fn packed_export_without_the_tail_is_left_alone() {
    let words = [
        0x7e00_0280, // v_mov_b32 v0, 0
        0x7e02_0280, // v_mov_b32 v1, 0
        EXP_MRT0_COMPR[0],
        EXP_MRT0_COMPR[1],
        S_ENDPGM,
    ];
    let ex = shader(Stage::Pixel, &words, vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], None);
    let t = translate(&ex).expect("translate");
    assert!(t.hlsl.contains("f16tof32"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("tb_pk"), "{}", t.hlsl);
    // ...and an integer element cannot be produced from it: that has to fail loudly.
    let ex = shader(Stage::Pixel, &words, vec![], vec![sig("SV_Target", 64, 1, 0, 0xf)], None);
    assert!(translate(&ex).is_err());
}

/// A vertex shader exporting an integer parameter (the primitive id handed to the pixel
/// shader) must pass the register bits through, not convert them as if they were a float.
#[test]
fn integer_parameter_export_keeps_the_integer() {
    let words = [
        0x7e10_0287, // v_mov_b32 v8, 7
        0x7e02_02f2, // v_mov_b32 v1, 1.0
        0xf800_08cf, // exp pos0 v1, v1, v1, v1 (done)
        0x0101_0101,
        0xf800_020f, // exp param0 v8, v8, v8, v8
        0x0808_0808,
        S_ENDPGM,
    ];
    let out = vec![sig("PRIMITIVE_ID", 0, 1, 0, 0x1), sig("SV_Position", 1, 3, 1, 0xf)];
    let ex = shader(Stage::Vertex, &words, vec![], out, None);
    let t = translate(&ex).expect("translate");
    assert!(t.hlsl.contains("o.o0 = (uint)(uint4(v8, v8, v8, v8).x);"), "{}", t.hlsl);
}

/// Texel type of a texture read with texel fetches: integer when the loaded value is used as
/// an integer, float when it is used as a float. (The lighting-channel mask is an R16_UINT
/// texture; declared as float its bits never matched the light's channel mask.)
#[test]
fn texel_type_follows_the_use_of_the_loaded_value() {
    let root = || Some(RootSig { version: 2, flags: 1, params: vec![table(0, 64, 5), table(2, 16, 5)], num_static_samplers: 0, static_samplers: Vec::new() });
    let program = |use_op: u32| {
        vec![
            0xf40c_0004, // s_load_dwordx8 s0, s8 [off=0]: texture descriptor t0
            0xfa00_0000,
            0x7e00_0280, // v_mov_b32 v0, 0
            0x7e02_0280, // v_mov_b32 v1, 0
            0xf000_1108, // image_load v0 <- 2D texture, coords v0..
            0x0000_0000,
            use_op,
            S_NOP,
            S_NOP,
            PACK_V0_V2_V2,
            PACK_V1_V2_V2,
            EXP_MRT0_COMPR[0],
            EXP_MRT0_COMPR[1],
            S_ENDPGM,
        ]
    };
    // v_and_b32 v2, 1, v0
    let ex = shader(Stage::Pixel, &program(0x3604_0081), vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], root());
    let t = translate(&ex).expect("translate (integer use)");
    assert!(t.hlsl.contains("Texture2D<uint4> tex0 : register(t0);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("uint4 t0 = tex0.Load("), "{}", t.hlsl);
    // v_add_f32 v2, v0, v0
    let ex = shader(Stage::Pixel, &program(0x0604_0100), vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], root());
    let t = translate(&ex).expect("translate (float use)");
    assert!(t.hlsl.contains("Texture2D<float4> tex0 : register(t0);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("float4 t0 = tex0.Load("), "{}", t.hlsl);
}

#[test]
fn loops_translate_deterministically() {
    let mut words = Vec::new();
    for s in 0..6u32 {
        words.extend([
            0xbe80_0384 | s << 16,
            0x8080_8100 | s << 16 | s,
            0xbf07_8000 | s,
            0xbf85_fffd,
        ]);
    }
    words.push(S_ENDPGM);
    let mut ex = shader(Stage::Compute, &words, vec![], vec![], None);
    ex.isa_preamble = [0, 1, 1, 1];
    (ex.has_input_sig, ex.has_output_sig) = (false, false);
    let first = translate(&ex).expect("translate").hlsl;
    assert_eq!(first.matches("do {").count(), 6, "{first}");
    for _ in 0..16 {
        assert!(translate(&ex).expect("translate").hlsl == first, "translation differs between runs");
    }
}

#[test]
fn vertex_fetch_packs_read_attributes_into_consecutive_vgprs() {
    let words = [
        0xbe84_2104,
        0xf800_00cf, 0x0706_0504,
        0xf800_0a0f, 0x0b0a_0908,
        S_ENDPGM,
    ];
    let mut position = sig("POSITION", 0, 3, 0, 0x3);
    position.rw_mask = 0x3;
    let unused = sig("NORMAL", 0, 3, 1, 0xf);
    let mut attribute = sig("TEXCOORD", 0, 1, 3, 0x4);
    attribute.rw_mask = 0x4;
    let ex = shader(
        Stage::Vertex,
        &words,
        vec![position, unused, attribute],
        vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)],
        None,
    );
    let t = translate(&ex).expect("vertex fetch translates");
    for assignment in [
        "v4 = asuint(i.i0.x);",
        "v5 = asuint(i.i0.y);",
        "v6 = 0u;",
        "v7 = 0x3f800000u;",
        "v8 = 0u;",
        "v9 = 0u;",
        "v10 = (uint)(i.i2.x);",
        "v11 = 0x3f800000u;",
    ] {
        assert!(t.hlsl.contains(assignment), "missing {assignment}\n{}", t.hlsl);
    }
    assert!(!t.hlsl.contains("i.i1."), "{}", t.hlsl);
    #[cfg(windows)]
    tombox_xshader::dxc::compile(&t.hlsl, &tombox_xshader::dxc::profile_for(t.profile)).unwrap();
}

#[test]
fn vertex_misc_vector_export_feeds_the_render_target_array_index() {
    let words = [0xf800_00cf, 0x0403_0200, 0xf800_08df, 0x0200_0101, S_ENDPGM];
    let ex = shader(
        Stage::Vertex,
        &words,
        vec![sig("SV_VertexID", 6, 1, 0, 1)],
        vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_RenderTargetArrayIndex", 4, 1, 1, 1)],
        None,
    );
    let t = translate(&ex).expect("POS1 misc vector export translates");
    let line = t.hlsl.lines().find(|l| l.contains("o.o1 =")).unwrap_or_else(|| panic!("no layer write:\n{}", t.hlsl));
    assert!(line.contains("uint4(") && line.trim_end().ends_with(".z);"), "{line}");
}

#[test]
fn rdna2_viewport_index_comes_from_layer_bits_19_16() {
    let words = [0xf800_00cf, 0x0403_0200, 0xf800_08df, 0x0200_0101, S_ENDPGM];
    let inputs = vec![sig("SV_VertexID", 6, 1, 0, 1)];
    let ex = shader(
        Stage::Vertex,
        &words,
        inputs.clone(),
        vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_RenderTargetArrayIndex", 4, 1, 1, 1), sig("SV_ViewportArrayIndex", 5, 1, 2, 1)],
        None,
    );
    let t = translate(&ex).expect("viewport export translates");
    let layer = t.hlsl.lines().find(|l| l.contains("o.o1 =")).unwrap_or_else(|| panic!("no layer write:\n{}", t.hlsl));
    let view = t.hlsl.lines().find(|l| l.contains("o.o2 =")).unwrap_or_else(|| panic!("no viewport write:\n{}", t.hlsl));
    assert!(layer.contains("(v0) & 0x7ffu"), "{layer}");
    assert!(view.contains("((v0) >> 16) & 0xfu"), "{view}");
    assert!(!view.contains(".w"), "{view}");
    let view_only = shader(
        Stage::Vertex,
        &words,
        inputs.clone(),
        vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_ViewportArrayIndex", 5, 1, 1, 1)],
        None,
    );
    let only = translate(&view_only).expect("viewport without a layer translates");
    let line = only.hlsl.lines().find(|l| l.contains("o.o1 =")).unwrap_or_else(|| panic!("no viewport write:\n{}", only.hlsl));
    assert!(line.contains("((v0) >> 16) & 0xfu"), "{line}");
    let mut no_z = words;
    no_z[2] = 0xf800_08db;
    let e = translate(&shader(
        Stage::Vertex,
        &no_z,
        inputs,
        vec![sig("SV_Position", 1, 3, 0, 0xf), sig("SV_ViewportArrayIndex", 5, 1, 1, 1)],
        None,
    ))
    .err()
    .expect("missing z lane translated");
    assert!(e.contains('z') && e.contains("SV_ViewportArrayIndex"), "{e}");
    #[cfg(windows)]
    tombox_xshader::dxc::compile(&t.hlsl, &tombox_xshader::dxc::profile_for(t.profile)).unwrap();
}

const EXP_POS0_V0_V3: [u32; 2] = [0xf800_08cf, 0x0300_0300];

#[test]
fn vertex_shader_receives_vertex_and_instance_index_in_v0_and_v3() {
    let words = [EXP_POS0_V0_V3[0], EXP_POS0_V0_V3[1], S_ENDPGM];
    let position = || vec![sig("SV_Position", 1, 3, 0, 0xf)];
    let ex = shader(Stage::Vertex, &words, vec![sig("SV_VertexID", 6, 1, 0, 1), sig("SV_InstanceID", 8, 1, 1, 1)], position(), None);
    let t = translate(&ex).expect("translate (vertex and instance index)");
    assert!(t.hlsl.contains("    v0 = (uint)(i.i0);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("    v3 = (uint)(i.i1);"), "{}", t.hlsl);
    let ex = shader(Stage::Vertex, &words, vec![sig("SV_VertexID", 6, 1, 0, 1)], position(), None);
    let t = translate(&ex).expect("translate (vertex index only)");
    assert!(t.hlsl.contains("    v0 = (uint)(i.i0);"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("v3 = (uint)(i."), "{}", t.hlsl);
    let mut attribute = sig("ATTRIBUTE", 0, 3, 0, 0xf);
    attribute.rw_mask = 0xf;
    let exp_pos0_v4_v5_v6_v7 = [0xf800_08cf, 0x0706_0504, S_ENDPGM];
    let ex = shader(Stage::Vertex, &exp_pos0_v4_v5_v6_v7, vec![attribute, sig("SV_VertexID", 6, 1, 1, 1), sig("SV_InstanceID", 8, 1, 2, 1)], position(), None);
    let t = translate(&ex).expect("translate (index declared, not read)");
    assert!(!t.hlsl.contains("v0 = (uint)(i.") && !t.hlsl.contains("v3 = (uint)(i."), "{}", t.hlsl);
    assert!(t.hlsl.contains("    v4 = asuint(i.i0.x);"), "{}", t.hlsl);
}

fn vintrp_word(op: u32, vdst: u32, attr: u32, chan: u32, vsrc: u32) -> u32 {
    0xc800_0000 | (vdst << 18) | (op << 16) | (attr << 10) | (chan << 8) | vsrc
}

fn vop3_interp_words(op: u32, vdst: u32, attr: u32, chan: u32, vsrc: u32) -> [u32; 2] {
    [0xd400_0000 | ((0x200 + op) << 16) | vdst, attr | (chan << 6) | ((256 + vsrc) << 9)]
}

fn interp_signature() -> (Vec<SigElement>, Vec<SigElement>) {
    let mut pos = sig("SV_Position", 1, 3, 0, 0xf);
    pos.rw_mask = 3;
    let mut tex = sig("TEXCOORD", 0, 3, 1, 0xf);
    tex.rw_mask = 0xf;
    (vec![pos, tex], vec![sig("SV_Target", 64, 3, 0, 1)])
}

#[test]
fn vop3_interp_forms_match_the_vintrp_lowering() {
    let (ins, outs) = interp_signature();
    let exp = [0xf800_1c01, 0x0000_0000, S_ENDPGM];
    let mut vintrp = vec![vintrp_word(0, 0, 0, 2, 0), vintrp_word(1, 0, 0, 2, 1), vintrp_word(2, 0, 0, 2, 0)];
    vintrp.extend_from_slice(&exp);
    let mut vop3 = Vec::new();
    for (op, vsrc) in [(0, 0), (1, 1), (2, 0)] {
        vop3.extend_from_slice(&vop3_interp_words(op, 0, 0, 2, vsrc));
    }
    vop3.extend_from_slice(&exp);
    let a = translate(&shader(Stage::Pixel, &vintrp, ins.clone(), outs.clone(), None)).expect("VINTRP interp translates");
    let b = translate(&shader(Stage::Pixel, &vop3, ins, outs, None)).expect("VOP3 interp translates");
    let body = |h: &str| h.lines().filter(|l| !l.trim_start().starts_with("// 0x")).map(str::to_string).collect::<Vec<_>>().join("\n");
    assert_eq!(body(&a.hlsl), body(&b.hlsl));
    assert!(b.hlsl.contains("v0 = TB_WV(asuint(i.i1.z), v0);"), "{}", b.hlsl);
}

#[test]
fn vop3_interp_with_modifiers_is_refused() {
    let (ins, outs) = interp_signature();
    let mut words: Vec<u32> = vop3_interp_words(0, 0, 0, 0, 0).to_vec();
    words[1] |= 1 << 30;
    words.push(S_ENDPGM);
    let err = match translate(&shader(Stage::Pixel, &words, ins, outs, None)) {
        Ok(t) => panic!("{}", t.hlsl),
        Err(e) => e,
    };
    assert!(err.contains("v_interp_p1_f32 VOP3 form with abs/neg/clamp/omod/opsel modifiers is not supported"), "{err}");
}

fn vop3_interp16_words(op: u32, vdst: u32, attr: u32, chan: u32, high: u32, vsrc: u32, vsrc2: u32) -> [u32; 2] {
    [0xd400_0000 | (op << 16) | vdst, attr | (chan << 6) | (high << 8) | ((256 + vsrc) << 9) | ((256 + vsrc2) << 18)]
}

#[test]
fn vop3_interp_f16_forms_decode_attr_chan_and_operands() {
    use tombox_xshader::rdna2::{disassemble, Opnd};
    let cases = [
        (0x342, "v_interp_p1ll_f16", vec![Opnd::Vgpr(1)]),
        (0x343, "v_interp_p1lv_f16", vec![Opnd::Vgpr(1), Opnd::Vgpr(5)]),
        (0x35a, "v_interp_p2_f16", vec![Opnd::Vgpr(1), Opnd::Vgpr(5)]),
    ];
    for (op, name, src) in cases {
        let isa: Vec<u8> = vop3_interp16_words(op, 4, 3, 2, 1, 1, 5).iter().flat_map(|w| w.to_le_bytes()).collect();
        let ins = disassemble(&isa).into_iter().next().unwrap();
        assert_eq!(ins.mnemonic, name);
        assert_eq!(ins.dwords, 2, "{name}");
        assert_eq!(ins.dst, vec![Opnd::Vgpr(4)], "{name}");
        assert_eq!(ins.src, src, "{name}");
        assert_eq!((ins.aux.imm, ins.aux.imm2), (3, 2), "{name} attr/chan");
    }
}

#[test]
fn vop3_interp_f16_forms_are_refused_with_attr_chan_and_half() {
    let cases = [
        (0x342, 0, 0, 0, "v_interp_p1ll_f16 attr0.x low half"),
        (0x342, 0, 1, 1, "v_interp_p1ll_f16 attr0.y high half"),
        (0x343, 0, 2, 0, "v_interp_p1lv_f16 attr0.z low half"),
        (0x35a, 0, 3, 1, "v_interp_p2_f16 attr0.w high half"),
    ];
    for (op, attr, chan, high, want) in cases {
        let (ins, outs) = interp_signature();
        let mut words = vop3_interp16_words(op, 0, attr, chan, high, 0, 1).to_vec();
        words.extend_from_slice(&[0xf800_1c01, 0x0000_0000, S_ENDPGM]);
        let err = match translate(&shader(Stage::Pixel, &words, ins, outs, None)) {
            Ok(t) => panic!("{want}: translated\n{}", t.hlsl),
            Err(e) => e,
        };
        assert!(
            err.contains(&format!("{want}: the packing of 16-bit attributes into LDS dword halves is not established")),
            "{err}"
        );
    }
}

#[test]
fn pixel_attribute_past_the_interpolants_reads_the_layer_sysval() {
    let words = [0xc802_0002, 0xf800_1c03, 0x0000_0000, S_ENDPGM];
    let mut layer = sig("SV_RenderTargetArrayIndex", 4, 1, 1, 1);
    layer.rw_mask = 1;
    let mut pos = sig("SV_Position", 1, 3, 0, 0xf);
    pos.rw_mask = 3;
    let ex = shader(Stage::Pixel, &words, vec![pos, layer], vec![sig("SV_Target", 64, 3, 0, 1)], None);
    let t = translate(&ex).expect("v_interp_mov of the layer attribute translates");
    assert!(t.hlsl.contains("(uint)(i.i1.x)"), "{}", t.hlsl);
}

#[test]
fn image_ops_with_status_or_r128_bits_are_refused() {
    let root = || Some(RootSig { version: 2, flags: 1, params: vec![table(0, 64, 5), table(2, 16, 5)], num_static_samplers: 0, static_samplers: Vec::new() });
    let program = |load: u32| vec![0xf40c_0004, 0xfa00_0000, 0x7e00_0280, 0x7e02_0280, load, 0x0000_0000, 0x3604_0081, S_NOP, S_NOP, PACK_V0_V2_V2, PACK_V1_V2_V2, EXP_MRT0_COMPR[0], EXP_MRT0_COMPR[1], S_ENDPGM];
    let out = || vec![sig("SV_Target", 64, 3, 0, 0xf)];
    translate(&shader(Stage::Pixel, &program(0xf000_1108), vec![], out(), root())).expect("plain image_load");
    for (bit, name) in [(15, "R128"), (16, "TFE"), (17, "LWE")] {
        let e = translate(&shader(Stage::Pixel, &program(0xf000_1108 | 1 << bit), vec![], out(), root())).err().unwrap_or_else(|| panic!("{name} translated"));
        assert!(e.contains(name), "{name}: {e}");
    }
}

#[test]
fn durango_vertex_fetch_call_synthesizes_attributes_from_input_signature() {
    let words = [0xbe80_2100, 0xf800_00cf, 0x0706_0504, S_ENDPGM];
    let pos_in = sig("POSITION", 0, 3, 0, 0xf);
    let color_in = sig("COLOR", 0, 3, 1, 0xf);
    let pos_out = sig("SV_Position", 1, 3, 0, 0xf);
    let mut ex = shader(Stage::Vertex, &words, vec![pos_in, color_in], vec![pos_out], None);

    let mut durango_blob = vec![0u8; 72];
    durango_blob[0..8].copy_from_slice(&72u64.to_le_bytes());
    durango_blob[0x18..0x1c].copy_from_slice(&5u32.to_le_bytes());
    durango_blob[0x28..0x30].copy_from_slice(&64u64.to_le_bytes());
    ex.fssm = durango_blob;

    let t = translate(&ex).expect("Durango vertex shader with s_swappc translates");
    assert!(t.hlsl.contains("struct SIn {"), "Missing SIn:\n{}", t.hlsl);
    assert!(t.hlsl.contains("float4 i0 : POSITION0;"), "Missing POSITION0:\n{}", t.hlsl);
    assert!(t.hlsl.contains("float4 i1 : COLOR0;"), "Missing COLOR0:\n{}", t.hlsl);
    assert!(t.hlsl.contains("v4 = asuint(i.i0.x);"), "Missing v4 init:\n{}", t.hlsl);
    assert!(t.hlsl.contains("v7 = asuint(i.i0.w);"), "Missing v7 init:\n{}", t.hlsl);
    assert!(t.hlsl.contains("v8 = asuint(i.i1.x);"), "Missing v8 init:\n{}", t.hlsl);
    assert!(t.hlsl.contains("v11 = asuint(i.i1.w);"), "Missing v11 init:\n{}", t.hlsl);
}

#[test]
fn amplification_dispatch_uses_the_y_group_count_and_not_the_payload_pointer() {
    let mut ex = shader(Stage::Amplification, &[S_ENDPGM], vec![], vec![], None);
    ex.gs_info = Some(GsInfo {
        input_primitive: 0,
        output_topology: 0,
        max_vertex_count: 0,
        mesh: Some(MeshInfo {
            stage: 14,
            groupshared_bytes: 0,
            payload_bytes: 16,
            max_vertices: 0,
            max_primitives: 0,
            prim_vectors: 0,
            topology: 0,
            num_threads: [32, 1, 1],
        }),
        tess: None,
    });
    let t = translate(&ex).expect("amplification shader translates");
    assert!(
        t.hlsl.contains("DispatchMesh(tb_args[0], max(tb_args[1], 1u), 1u, tb_pl);"),
        "{}",
        t.hlsl
    );
}

fn geometry(prim: u32, topo: u32) -> Result<tombox_xshader::hlsl::Translated, String> {
    let mut ex = shader(
        Stage::Geometry,
        &[S_ENDPGM],
        vec![sig("POSITION", 0, 3, 0, 0xf)],
        vec![sig("SV_Position", 1, 3, 0, 0xf)],
        None,
    );
    ex.gs_info = Some(GsInfo { input_primitive: prim, output_topology: topo, max_vertex_count: 1, mesh: None, tess: None });
    translate(&ex)
}

#[test]
fn geometry_adjacency_primitives_translate() {
    let tri = geometry(3, 5).expect("triangle");
    assert!(tri.hlsl.contains("triangle SIn i[3]"), "{}", tri.hlsl);
    assert!(tri.hlsl.contains("v0 = 0x04000000u; v1 = 0x800u;"), "{}", tri.hlsl);
    assert!(!tri.hlsl.contains("v2 ="), "{}", tri.hlsl);

    let line = geometry(6, 3).expect("lineadj");
    assert_eq!(line.profile, "gs_5_0");
    assert!(line.hlsl.contains("lineadj SIn i[4]"), "{}", line.hlsl);
    assert!(line.hlsl.contains("v1 = 0xc000800u;"), "{}", line.hlsl);
    assert!(!line.hlsl.contains("v2 ="), "{}", line.hlsl);

    let adj = geometry(7, 5).expect("triangleadj");
    assert!(adj.hlsl.contains("triangleadj SIn i[6]"), "{}", adj.hlsl);
    assert!(adj.hlsl.contains("v2 = 0x14001000u;"), "{}", adj.hlsl);

    let err = match geometry(4, 5) {
        Err(e) => e,
        Ok(_) => panic!("primitive 4 is not a GS input"),
    };
    assert!(err.contains("input primitive"), "{err}");
}

fn geometry_sendmsg(simm16: u32) -> Result<tombox_xshader::hlsl::Translated, String> {
    let mut ex = shader(
        Stage::Geometry,
        &[0xbf90_0000 | simm16, S_ENDPGM],
        vec![sig("POSITION", 0, 3, 0, 0xf)],
        vec![sig("SV_Position", 1, 3, 0, 0xf)],
        None,
    );
    ex.gs_info = Some(GsInfo { input_primitive: 1, output_topology: 1, max_vertex_count: 1, mesh: None, tess: None });
    translate(&ex)
}

#[test]
fn geometry_emit_and_cut_on_streams_1_to_3_are_refused() {
    for stream in 1..4u32 {
        for op in [1u32, 2, 3] {
            let err = match geometry_sendmsg(2 | (op << 4) | (stream << 8)) {
                Err(e) => e,
                Ok(_) => panic!("GS op {op} on stream {stream} must not translate"),
            };
            assert!(err.contains("GS output stream != 0 unsupported"), "stream {stream} op {op}: {err}");
        }
    }
    geometry_sendmsg(3).expect("GS_DONE translates");
}

fn compute_threads(threads: Option<[u32; 3]>, preamble: [u32; 4]) -> String {
    let mut ex = shader(Stage::Compute, &[S_ENDPGM], vec![], vec![], None);
    ex.psv_numthreads = threads;
    ex.isa_preamble = preamble;
    translate(&ex).expect("compute shader translates").hlsl
}

#[test]
fn compute_numthreads_come_from_psv_when_the_preamble_is_zero() {
    let h = compute_threads(Some([4, 2, 1]), [0; 4]);
    assert!(h.contains("[numthreads(4, 2, 1)]"), "{h}");
    let preamble = compute_threads(Some([4, 2, 1]), [0, 8, 1, 1]);
    assert!(preamble.contains("[numthreads(8, 1, 1)]"), "{preamble}");
    let missing = compute_threads(None, [0; 4]);
    assert!(missing.contains("[numthreads(1, 1, 1)]"), "{missing}");
}

fn compute_with(words: &[u32], preamble: [u32; 4]) -> String {
    let mut ex = shader(Stage::Compute, words, vec![], vec![], None);
    ex.isa_preamble = preamble;
    translate(&ex).expect("compute shader translates").hlsl
}

fn compute_refusal(words: &[u32]) -> String {
    let ex = shader(Stage::Compute, words, vec![], vec![], None);
    match translate(&ex) {
        Err(e) => e,
        Ok(t) => panic!("must not translate:
{}", t.hlsl),
    }
}

#[test]
fn sendmsghalt_and_ordered_count_are_refused() {
    let halt = compute_refusal(&[0xbf91_0000, S_ENDPGM]);
    assert!(halt.contains("s_sendmsghalt halts the wave after the message"), "{halt}");
    let ordered = compute_refusal(&[0xd8fe_0000, 0x0000_0101, S_ENDPGM]);
    assert!(ordered.contains("ds_ordered_count: dispatch-ordered GDS append is not lowered"), "{ordered}");
}

fn consecutive_lds() -> [u32; 7] {
    [0xd834_0000, 0x0000_0909, 0xd834_0000, 0x0000_0a09, 0xd8d8_0000, 0x0700_0007, S_ENDPGM]
}

fn consecutive_reads() -> [u32; 5] {
    [0xd8d8_0000, 0x0700_0007, 0xd8d8_0000, 0x0800_0008, S_ENDPGM]
}

fn barriers_between_first_lds(h: &str) -> usize {
    let body = h.split("void main").nth(1).expect("compute entry");
    let first = body.find("tb_lds[").expect("first LDS access");
    let second = body[first + 1..].find("tb_lds[").map(|p| first + 1 + p).expect("second LDS access");
    body[first..second].matches("GroupMemoryBarrierWithGroupSync();").count()
}

#[test]
fn one_wave_compute_separates_consecutive_lds_writes() {
    let h = compute_with(&consecutive_lds(), [0, 8, 8, 1]);
    assert!(h.contains("[numthreads(8, 8, 1)]"), "{h}");
    assert_eq!(barriers_between_first_lds(&h), 1, "{h}");

    let big = compute_with(&consecutive_lds(), [0, 16, 16, 1]);
    assert!(big.contains("[numthreads(16, 16, 1)]"), "{big}");
    assert_eq!(barriers_between_first_lds(&big), 0, "{big}");

    let reads = compute_with(&consecutive_reads(), [0, 8, 8, 1]);
    assert_eq!(barriers_between_first_lds(&reads), 0, "{reads}");

    let mut barred = consecutive_lds().to_vec();
    barred.insert(2, 0xbf8a_0000);
    let barred = compute_with(&barred, [0, 8, 8, 1]);
    assert_eq!(barriers_between_first_lds(&barred), 1, "{barred}");

    let unknown = compute_with(&consecutive_lds(), [0; 4]);
    assert_eq!(barriers_between_first_lds(&unknown), 0, "{unknown}");
}

#[test]
fn d16_texel_type_looks_through_half_packing() {
    let root = || Some(RootSig {
        version: 2,
        flags: 1,
        params: vec![table(0, 64, 5), table(1, 8, 5)],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    });
    let cs = |words: &[u32]| {
        let mut ex = shader(Stage::Compute, words, vec![], vec![], root());
        ex.isa_preamble = [0, 64, 1, 1];
        (ex.has_input_sig, ex.has_output_sig) = (false, false);
        ex
    };
    let root2 = || Some(RootSig {
        version: 2,
        flags: 1,
        params: vec![table(0, 64, 5), table(3, 16, 5), table(1, 8, 5)],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    });
    let cs2 = |words: &[u32]| {
        let mut ex = shader(Stage::Compute, words, vec![], vec![], root2());
        ex.isa_preamble = [0, 64, 1, 1];
        (ex.has_input_sig, ex.has_output_sig) = (false, false);
        ex
    };
    let _ = cs;
    const S_WAITCNT_0: u32 = 0xbf8c_0000;
    let uav = [0xf40c_0503, 0xfa00_0000, S_WAITCNT_0];
    let srv = [0xf40c_0202, 0xfa00_0000, S_WAITCNT_0];
    let store = [0xf020_1f08, 0x8005_0800];
    let load = [0xf000_1f08, 0x8002_0400];

    let mut w = uav.to_vec();
    w.extend([
        0x7e14_1508,
        0x7e16_1509,
        0x3614_14ff, 0x0000_ffff,
        0xd76f_0008, 0x0429_210b,
        0x7e12_0308,
    ]);
    w.extend(store);
    w.push(S_ENDPGM);
    let h = translate(&cs2(&w)).expect("f16 packed store").hlsl;
    assert!(h.contains("RWTexture2D<float4> uav0"), "{h}");

    let mut w = uav.to_vec();
    w.extend([
        0x2814_1508,
        0x3614_14ff, 0x0000_ffff,
        0xd76f_0008, 0x0429_2109,
        0x7e12_0308,
    ]);
    w.extend(store);
    w.push(S_ENDPGM);
    let h = translate(&cs2(&w)).expect("uint packed store").hlsl;
    assert!(h.contains("RWTexture2D<uint4> uav0"), "{h}");
    assert!(h.contains("& 0xffffu)") && h.contains(">> 16u)"), "{h}");

    let mut w = uav.to_vec();
    w.extend([
        0x2414_1508,
        0x3614_14ff, 0x0000_ffff,
        0xd76f_0008, 0x0429_2109,
        0x7e12_0308,
    ]);
    w.extend(store);
    w.push(S_ENDPGM);
    let h = translate(&cs2(&w)).expect("sint packed store").hlsl;
    assert!(h.contains("RWTexture2D<int4> uav0"), "{h}");
    assert!(h.contains("asint(v8 << 16u) >> 16") && h.contains("asint(v8) >> 16"), "{h}");

    let mut w = srv.to_vec();
    w.extend(load);
    w.extend([
        0x2c10_0890,
        0x7e10_1708,
    ]);
    w.push(S_ENDPGM);
    let h = translate(&cs2(&w)).expect("f16 packed load").hlsl;
    assert!(h.contains("Texture2D<float4> tex0"), "{h}");

    let mut w = uav.to_vec();
    w.extend([
        0xf044_1108, 0x0005_0400,
        0xf000_1108, 0x8005_0400,
        S_ENDPGM,
    ]);
    let err = match translate(&cs2(&w)) {
        Ok(t) => panic!("expected refusal, got:\n{}", t.hlsl),
        Err(e) => e,
    };
    assert!(err.contains("atomic"), "{err}");
}

#[test]
fn raw_uav_buffer_atomics_share_ubuf_instead_of_overlapping_utb() {
    let words = [
        0xf408_0002, 0xfa00_0000,
        0x7e02_0284,
        0x7e06_0287,
        0xe070_1000, 0x8000_0301,
        0xe0e8_1004, 0x8000_0301,
        S_ENDPGM,
    ];
    let root = RootSig {
        version: 2,
        flags: 0,
        params: vec![table(1, 1, 0)],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    };
    let mut ex = shader(Stage::Compute, &words, vec![], vec![], Some(root));
    ex.isa_preamble = [0, 64, 1, 1];
    (ex.has_input_sig, ex.has_output_sig) = (false, false);
    ex.has_psv0 = true;
    ex.resources.push(PsvResource { res_type: 7, space: 0, lower: 0, upper: 0, kind: 1 });
    let t = translate(&ex).expect("raw UAV buffer store + atomic");
    assert!(t.hlsl.contains("ubuf0"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("utb0"), "overlapping utb0 declaration:\n{}", t.hlsl);
    assert!(t.hlsl.contains("RWByteAddressBuffer ubuf0"), "{}", t.hlsl);
    assert!(t.hlsl.contains("ubuf0.InterlockedOr("), "{}", t.hlsl);
    assert!(t.hlsl.contains("ubuf0.Store("), "{}", t.hlsl);
    if tombox_xshader::dxc::available() {
        let profile = tombox_xshader::dxc::profile_for(t.profile);
        tombox_xshader::dxc::compile(&t.hlsl, &profile).expect("DXC compile");
    }
}

fn raw_buffer_shader(res_type: u32, range_type: u32, op_word: u32) -> ShaderExtract {
    let words = [
        0xf408_0002, 0xfa00_0000,
        0x7e02_0284,
        0x7e06_0287,
        op_word, 0x8000_0301,
        S_ENDPGM,
    ];
    let root = RootSig {
        version: 2,
        flags: 0,
        params: vec![table(range_type, 1, 0)],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    };
    let mut ex = shader(Stage::Compute, &words, vec![], vec![], Some(root));
    ex.isa_preamble = [0, 64, 1, 1];
    (ex.has_input_sig, ex.has_output_sig) = (false, false);
    ex.has_psv0 = true;
    ex.resources.push(PsvResource { res_type, space: 0, lower: 0, upper: 0, kind: 1 });
    ex
}

#[test]
fn raw_srv_buffer_is_a_byte_address_buffer() {
    let t = translate(&raw_buffer_shader(4, 0, 0xe030_1014)).expect("raw SRV buffer_load_dword");
    assert!(t.hlsl.contains("ByteAddressBuffer sbuf0"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("TbElem_sbuf0"), "{}", t.hlsl);
    assert!(t.hlsl.contains("= sbuf0.Load(((t"), "{}", t.hlsl);
    if tombox_xshader::dxc::available() {
        let profile = tombox_xshader::dxc::profile_for(t.profile);
        tombox_xshader::dxc::compile(&t.hlsl, &profile).expect("DXC compile");
    }
}

#[test]
fn structured_srv_buffer_keeps_its_element_struct() {
    let t = translate(&raw_buffer_shader(5, 0, 0xe030_1014)).expect("structured SRV buffer_load_dword");
    assert!(t.hlsl.contains("StructuredBuffer<TbElem_sbuf0> sbuf0"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("ByteAddressBuffer"), "{}", t.hlsl);
}

#[test]
fn raw_buffer_with_idxen_is_refused() {
    let Err(e) = translate(&raw_buffer_shader(7, 1, 0xe0e8_2000)) else { panic!("raw UAV atomic with IDXEN translated") };
    assert!(e.contains("raw buffer ubuf0 addressed with IDXEN"), "{e}");
    let Err(e) = translate(&raw_buffer_shader(7, 1, 0xe070_2000)) else { panic!("raw UAV store with IDXEN translated") };
    assert!(e.contains("raw buffer ubuf0 addressed with IDXEN"), "{e}");
}

#[test]
fn unmapped_position_export_without_clip_or_cull_distance_is_ignored() {
    let words = [
        EXP_POS0_V0_V3[0], EXP_POS0_V0_V3[1],
        0xf800_08df, 0x0302_0100,
        S_ENDPGM,
    ];
    let position = || vec![sig("SV_Position", 1, 3, 0, 0xf)];
    let ex = shader(Stage::Vertex, &words, vec![], position(), None);
    let t = translate(&ex).expect("translate position export 13 without clip or cull distance");
    assert!(t.hlsl.contains("o.o0 ="), "{}", t.hlsl);
    if tombox_xshader::dxc::available() {
        let profile = tombox_xshader::dxc::profile_for(t.profile);
        tombox_xshader::dxc::compile(&t.hlsl, &profile).expect("DXC compile");
    }
}

fn exp(target: u32) -> [u32; 2] {
    [0xf800_0000 | (target << 4) | 0xf | (1 << 10), 0x0302_0100]
}

#[test]
fn ps_export_target_above_mrt7_is_refused() {
    let ok = shader(Stage::Pixel, &[exp(0)[0], exp(0)[1], S_ENDPGM], vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], None);
    translate(&ok).expect("mrt0 export translates");
    let ex = shader(Stage::Pixel, &[exp(10)[0], exp(10)[1], S_ENDPGM], vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], None);
    let err = match translate(&ex) {
        Ok(t) => panic!("expected refusal, got:\n{}", t.hlsl),
        Err(e) => e,
    };
    assert!(err.starts_with("PS export target 10 unsupported"), "{err}");
}

#[test]
fn non_pixel_export_target_outside_position_and_parameters_is_refused() {
    let out = || vec![sig("PRIMITIVE_ID", 0, 1, 0, 0x1)];
    let ok = shader(Stage::Vertex, &[0xf800_0207, 0x0000_0000, S_ENDPGM], vec![], out(), None);
    translate(&ok).expect("param0 export translates");
    let ex = shader(Stage::Vertex, &[exp(10)[0], exp(10)[1], S_ENDPGM], vec![], out(), None);
    let err = match translate(&ex) {
        Ok(t) => panic!("expected refusal, got:\n{}", t.hlsl),
        Err(e) => e,
    };
    assert!(err.starts_with("export target 10 unsupported"), "{err}");
}

#[test]
fn image_atomic_64bit_emits_uint64_uav_and_sm66() {
    let words = [
        0xf40c_0004, 0xfa00_0000,
        0x7e02_0284,
        0x7e04_0285,
        0x7e06_0286,
        0x7e08_0287,
        0xf05c_1308, 0x0000_0301,
        0xf800_1890, 0x0000_0000,
        S_ENDPGM,
    ];
    let root = RootSig {
        version: 2,
        flags: 0,
        params: vec![table(1, 16, 0)],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    };
    let ex = shader(Stage::Pixel, &words, vec![], vec![], Some(root));
    let t = translate(&ex).expect("64-bit image_atomic_umax");
    assert_eq!(t.profile, "ps_6_6");
    assert!(t.uses_typed_atomic64);
    assert!(t.hlsl.contains("RWTexture2D<uint64_t> uav0"), "{}", t.hlsl);
    assert!(t.hlsl.contains("InterlockedMax(uav0[uint2(v1, v2)], (((uint64_t)v4 << 32) | (uint64_t)v3)"), "{}", t.hlsl);
    if tombox_xshader::dxc::available() {
        let profile = tombox_xshader::dxc::profile_for(t.profile);
        tombox_xshader::dxc::compile(&t.hlsl, &profile).expect("DXC compile ps_6_6");
    }
}

fn ds_swizzle_op(offset: u32, addr: u32, vdst: u32) -> [u32; 2] {
    [0xd800_0000 | (0x35 << 18) | (offset & 0xffff), addr | (vdst << 24)]
}

fn ps_out() -> Vec<SigElement> {
    vec![sig("SV_Target", 64, 3, 0, 0xf)]
}

#[test]
fn ds_swizzle_refusals_are_explicit() {
    let words = [ds_swizzle_op(0x8000 | 0xe4, 2, 3)[0], ds_swizzle_op(0x8000 | 0xe4, 2, 3)[1], S_ENDPGM];
    let vertex_out = || vec![sig("SV_Position", 1, 3, 0, 0xf)];
    let err = match translate(&shader(Stage::Vertex, &words, vec![], vertex_out(), None)) {
        Ok(t) => panic!("ds_swizzle in a vertex shader translated:\n{}", t.hlsl),
        Err(e) => e,
    };
    assert!(err.contains("ds_swizzle outside a pixel shader"), "{err}");

    let plain = ds_swizzle_op(0x001f, 2, 3);
    let err = match translate(&shader(Stage::Pixel, &[plain[0], plain[1], S_ENDPGM], vec![], ps_out(), None)) {
        Ok(t) => panic!("ds_swizzle without QDMode translated:\n{}", t.hlsl),
        Err(e) => e,
    };
    assert!(err.contains("ds_swizzle without QDMode unsupported"), "{err}");

    let quad = ds_swizzle_op(0x8000 | 0xe4, 2, 3);
    let t = translate(&shader(Stage::Pixel, &[quad[0], quad[1], S_ENDPGM], vec![], ps_out(), None))
        .expect("QDMode ds_swizzle translates");
    assert!(t.hlsl.contains("uint tb_quad = ((uint)i.tb_pos.x & 1u) | (((uint)i.tb_pos.y & 1u) << 1);"), "{}", t.hlsl);
    assert!(t.hlsl.contains("QuadReadLaneAt(v2, 0u)"), "{}", t.hlsl);
}

fn literal_sampler_shader(address: u32, last_sampler_word: u32) -> ShaderExtract {
    let words = [
        0x7e04_0280, 0x7e06_0280,
        0xf40c_0200, 0xfa00_0000, 0xbf8c_c07f,
        0xbe90_03ff, 0x0000_0092,
        0xbe91_03ff, 0x00c0_0000,
        0xbe92_0380, last_sampler_word,
        0xf080_0f08, 0x0082_0402, 0xbf8c_3f70,
        0xf800_180f, 0x0706_0504, S_ENDPGM,
    ];
    let root = RootSig {
        version: 1,
        flags: 0,
        params: vec![table(0, 1, 5)],
        num_static_samplers: 1,
        static_samplers: vec![StaticSampler {
            filter: 0,
            address_u: address,
            address_v: address,
            address_w: address,
            comparison_func: 1,
            max_anisotropy: 1,
            border_color: 0,
            min_lod_bits: 0,
            max_lod_bits: f32::MAX.to_bits(),
            register: 0,
            space: 0,
            visibility: 5,
        }],
    };
    let mut ex = shader(
        Stage::Pixel, &words, vec![], vec![sig("SV_Target", 64, 3, 0, 0xf)], Some(root),
    );
    ex.has_psv0 = true;
    ex.resources = vec![
        PsvResource { res_type: 3, space: 0, lower: 0, upper: 0, kind: 2 },
        PsvResource { res_type: 1, space: 0, lower: 0, upper: 0, kind: 14 },
    ];
    ex
}

#[test]
fn rdna2_literal_sampler_uses_the_static_root_binding() {
    let ex = literal_sampler_shader(3, 0xbe93_0380);
    let translated = translate(&ex).expect("translate a clamp sampler");
    assert!(translated.hlsl.contains("register(s0)"));
    assert!(translated.hlsl.contains("register(t0)"));
}

#[test]
fn rdna2_literal_sampler_rejects_incompatible_or_unproven_bindings() {
    let wrong_address = translate(&literal_sampler_shader(1, 0xbe93_0380))
        .err().expect("reject a wrap sampler for a clamp descriptor");
    assert!(wrong_address.contains("no static sampler has the decoded address modes"), "{wrong_address}");

    let nonliteral_word = translate(&literal_sampler_shader(3, 0xbe93_0314))
        .err().expect("reject a nonliteral sampler word");
    assert!(nonliteral_word.contains("not a sampler descriptor"), "{nonliteral_word}");
}
