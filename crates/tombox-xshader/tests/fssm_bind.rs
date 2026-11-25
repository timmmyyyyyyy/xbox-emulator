use tombox_xshader::hlsl::translate;
use tombox_xshader::meta::{RootParam, RootParamKind, RootRange, RootSig, SigElement, StaticSampler};
use tombox_xshader::{translate_with_fssm_root, ShaderExtract, Stage};

const HEAD: [u32; 4] = [0xf408_0104, 0xfa00_0000, 0xf40c_0304, 0xfa00_0020];
const BUF_LOAD_V4: [u32; 2] = [0xe030_1000, 0x8001_0401];
const IMG_LOAD_V8: [u32; 2] = [0xf000_0f08, 0x0003_0801];
const TAIL: [u32; 4] = [0x7e00_0304, 0xf800_180f, 0x0302_0100, 0xbf81_0000];

fn fssm(user_sgprs: u8, l1: &[[u32; 5]], l2: &[[u32; 5]]) -> Vec<u8> {
    let mut f = vec![0u8; 0x100];
    f[0..4].copy_from_slice(b"FSSM");
    f[0xD6] = user_sgprs;
    f[0x40..0x48].copy_from_slice(&0xB0u64.to_le_bytes());
    let p1 = f.len() as u64 - 0x10;
    f[0xE8..0xF0].copy_from_slice(&p1.to_le_bytes());
    f[0xF8..0xFC].copy_from_slice(&(l1.len() as u32).to_le_bytes());
    for w in l1.iter().flatten() {
        f.extend_from_slice(&w.to_le_bytes());
    }
    let p2 = f.len() as u64 - 0x10;
    f[0xF0..0xF8].copy_from_slice(&p2.to_le_bytes());
    f[0xFC..0x100].copy_from_slice(&(l2.len() as u32).to_le_bytes());
    for w in l2.iter().flatten() {
        f.extend_from_slice(&w.to_le_bytes());
    }
    f
}

fn table_with_buffer_and_texture() -> Vec<u8> {
    fssm(9, &[[3, 0, 1, 0, 8]], &[[0x300, 0, 0, 4, 0x0804], [0x300, 8, 1, 4, 0x0808]])
}

fn pixel_shader(words: &[u32], fssm: Vec<u8>) -> ShaderExtract {
    let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    ShaderExtract {
        stage: Stage::Pixel,
        shader_model: (6, 0),
        bitcode_stripped: true,
        fssm,
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
        output_sig: vec![SigElement { name: "SV_Target".into(), index: 0, sysval: 64, comp_type: 3, register: 0, mask: 0xf, rw_mask: 0 }],
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: None,
        root_sig: None,
    }
}

fn program() -> Vec<u32> {
    [&HEAD[..], &BUF_LOAD_V4, &IMG_LOAD_V8, &TAIL].concat()
}

fn engine_srv_table() -> RootSig {
    RootSig {
        version: 2,
        flags: 0,
        params: vec![RootParam { visibility: 5, kind: RootParamKind::Table(vec![RootRange { range_type: 0, num: 64, base_register: 0, space: 0, offset_in_table: u32::MAX }]) }],
        num_static_samplers: 0,
        static_samplers: Vec::new(),
    }
}

fn compile(label: &str, hlsl: &str, profile: &str) {
    #[cfg(windows)]
    {
        assert!(tombox_xshader::dxc::available(), "{label}: DXC (dxcompiler.dll + dxil.dll) not found");
        let profile = tombox_xshader::dxc::profile_for(profile);
        let code = tombox_xshader::dxc::compile(hlsl, &profile).unwrap_or_else(|e| panic!("{label}: dxc: {e}\n{hlsl}"));
        assert!(code.len() > 4 && &code[..4] == b"DXBC", "{label}: DXC returned no container");
    }
    #[cfg(not(windows))]
    let _ = (label, hlsl, profile);
}

#[test]
fn table_binds_a_shader_without_root_signature() {
    let ex = pixel_shader(&program(), table_with_buffer_and_texture());
    let Err(e) = translate(&ex) else { panic!("a table load without any root signature must be refused") };
    assert!(e.contains("missing root signature"), "{e}");
    let t = translate_with_fssm_root(&ex).unwrap_or_else(|e| panic!("FSSM table translation failed: {e}"));
    assert!(t.hlsl.contains("register(t0)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("Texture2D<float4> tex1 : register(t1);"), "{}", t.hlsl);
    compile("FSSM table", &t.hlsl, t.profile);
}

#[test]
fn table_and_equivalent_root_signature_give_the_same_hlsl() {
    let bare = pixel_shader(&program(), table_with_buffer_and_texture());
    let mut with_rs = bare.clone();
    with_rs.has_root_sig = true;
    with_rs.root_sig = Some(engine_srv_table());
    let from_rs = translate(&with_rs).unwrap_or_else(|e| panic!("root signature translation failed: {e}"));
    let from_table = translate_with_fssm_root(&bare).unwrap_or_else(|e| panic!("FSSM table translation failed: {e}"));
    assert_eq!(from_rs.hlsl, from_table.hlsl);
    let b = tombox_xshader::fssm_bind::parse(&bare.fssm).unwrap().unwrap();
    assert!(b.disagreements(&engine_srv_table()).is_empty());
}

#[test]
fn shaders_with_a_root_signature_ignore_the_table() {
    let mut ex = pixel_shader(&program(), fssm(9, &[[3, 0, 1, 0, 8]], &[[0x300, 0, 5, 4, 0x0804], [0x300, 8, 6, 4, 0x0808]]));
    ex.has_root_sig = true;
    ex.root_sig = Some(engine_srv_table());
    let a = translate(&ex).unwrap_or_else(|e| panic!("{e}"));
    let b = translate_with_fssm_root(&ex).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(a.hlsl, b.hlsl);
    assert!(!a.hlsl.contains("register(t5)"), "{}", a.hlsl);
}

#[test]
fn unreadable_or_missing_tables_are_refused() {
    let mut f = table_with_buffer_and_texture();
    f[0xE8..0xF0].copy_from_slice(&0xDEAD_BEEF_DEAD_BEEFu64.to_le_bytes());
    let Err(e) = translate_with_fssm_root(&pixel_shader(&program(), f)) else { panic!("no table must be refused") };
    assert!(e.contains("no FSSM binding table"), "{e}");
    let Err(e) = translate_with_fssm_root(&pixel_shader(&program(), fssm(10, &[[3, 0, 1, 0, 8]], &[]))) else { panic!("a bad table must be refused") };
    assert!(e.contains("user SGPR count"), "{e}");
    let mut f = table_with_buffer_and_texture();
    f[0x40..0x48].copy_from_slice(&0u64.to_le_bytes());
    f[0x38..0x40].copy_from_slice(&0xB0u64.to_le_bytes());
    let Err(e) = translate_with_fssm_root(&pixel_shader(&program(), f)) else { panic!("a vertex-stage table on a pixel shader must be refused") };
    assert!(e.contains("hardware stage slot"), "{e}");
}

#[test]
fn table_types_the_descriptor_as_written_or_read() {
    let store = [0xe070_1000u32, 0x8001_0801];
    let words: Vec<u32> = [&HEAD[..], &store, &IMG_LOAD_V8, &TAIL].concat();
    let ex = pixel_shader(&words, fssm(9, &[[3, 0, 1, 0, 8]], &[[0x303, 0, 2, 4, 0x0804], [0x300, 8, 1, 4, 0x0808]]));
    let t = translate_with_fssm_root(&ex).unwrap_or_else(|e| panic!("FSSM table translation failed: {e}"));
    assert!(t.hlsl.contains("register(u2)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("register(t1)"), "{}", t.hlsl);
    compile("FSSM UAV + SRV", &t.hlsl, t.profile);
}

const TEX_AT_DWORD_8: [u32; 2] = [0xf40c_0304, 0xfa00_0020];
const SAMPLER_S20_CLAMP_TRILINEAR: [u32; 7] = [0xbe94_03ff, 0x0000_0092, 0xbe95_03ff, 0x00c0_0000, 0xbe96_03ff, 0x1850_0000, 0xbe97_0380];
const COORDS: [u32; 2] = [0x7e04_0280, 0x7e06_0280];
const SAMPLE_S12_S20: [u32; 2] = [0xf080_0f08, 0x00a3_0402];
const EXPORT_V4: [u32; 4] = [0xbf8c_3f70, 0xf800_180f, 0x0706_0504, 0xbf81_0000];
const WAIT: u32 = 0xbf8c_c07f;

fn literal_sampler_program(between: &[u32]) -> Vec<u32> {
    [&COORDS[..], &TEX_AT_DWORD_8, &SAMPLER_S20_CLAMP_TRILINEAR, between, &[WAIT], &SAMPLE_S12_S20, &EXPORT_V4].concat()
}

fn static_sampler_table(static_registers: &[u32]) -> Vec<u8> {
    let mut l1 = vec![[3u32, 0, 1, 0, 8]];
    let mut l2 = vec![[0x300u32, 8, 1, 4, 0x0808]];
    for (i, r) in static_registers.iter().enumerate() {
        l1.push([4, 0, 0, *r, (i as u32) << 8]);
        l2.push([0x401, 0, *r, 0, ((i as u32) << 16) | 4]);
    }
    fssm(9, &l1, &l2)
}

fn sampled_shader(between: &[u32], static_registers: &[u32]) -> ShaderExtract {
    let mut ex = pixel_shader(&literal_sampler_program(between), static_sampler_table(static_registers));
    ex.has_psv0 = true;
    ex.resources = vec![
        tombox_xshader::meta::PsvResource { res_type: 3, space: 0, lower: 1, upper: 1, kind: 2 },
        tombox_xshader::meta::PsvResource { res_type: 1, space: 1, lower: 5, upper: 7, kind: 14 },
    ];
    ex
}

fn static_sampler(register: u32, address: u32, min_lod: f32) -> StaticSampler {
    StaticSampler {
        filter: 0x15,
        address_u: address,
        address_v: address,
        address_w: address,
        max_anisotropy: 1,
        comparison_func: 1,
        border_color: 0,
        min_lod_bits: min_lod.to_bits(),
        max_lod_bits: f32::MAX.to_bits(),
        register,
        space: 1,
        visibility: 0,
    }
}

fn runtime_root_signature(samplers: Vec<StaticSampler>) -> RootSig {
    let mut rs = engine_srv_table();
    rs.num_static_samplers = samplers.len() as u32;
    rs.static_samplers = samplers;
    rs
}

#[test]
fn table_static_sampler_binds_a_literal_sampler_without_root_signature() {
    let ex = sampled_shader(&[], &[5]);
    let Err(e) = translate(&ex) else { panic!("a literal sampler without any root signature must be refused") };
    assert!(e.contains("s20 not a sampler descriptor"), "{e}");
    let t = translate_with_fssm_root(&ex).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("register(s5, space1)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("register(t1)"), "{}", t.hlsl);
    compile("FSSM static sampler", &t.hlsl, t.profile);
}

#[test]
fn a_literal_sampler_set_before_a_branch_is_still_proven() {
    let skip_one_vector_move = [0xbf88_0001u32, 0x7e04_02f2];
    let t = translate_with_fssm_root(&sampled_shader(&skip_one_vector_move, &[5])).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("register(s5, space1)"), "{}", t.hlsl);
    compile("FSSM static sampler after a branch", &t.hlsl, t.profile);
}

#[test]
fn several_table_static_samplers_without_state_are_refused() {
    let Err(e) = translate_with_fssm_root(&sampled_shader(&[], &[5, 7])) else { panic!("two candidate static samplers and no state must be refused") };
    assert!(e.contains("lists 2 static samplers (s5, s7)") && e.contains("no sampler state"), "{e}");
}

#[test]
fn a_clobbered_sampler_word_is_not_a_literal_sampler() {
    let carry_out_into_s22 = [0xd70f_1600u32, 0x0002_0101];
    let Err(e) = translate_with_fssm_root(&sampled_shader(&carry_out_into_s22, &[5])) else { panic!("an overwritten sampler word must be refused") };
    assert!(e.contains("s20 not a sampler descriptor"), "{e}");
}

#[test]
fn table_narrows_root_signature_static_samplers_to_the_used_register() {
    let mut ex = sampled_shader(&[], &[5]);
    ex.root_sig = Some(runtime_root_signature(vec![static_sampler(2, 3, 0.0), static_sampler(5, 3, 1.0)]));
    let t = translate(&ex).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("register(s5, space1)"), "{}", t.hlsl);
    assert!(!t.hlsl.contains("register(s2"), "{}", t.hlsl);
    compile("FSSM-narrowed root signature static sampler", &t.hlsl, t.profile);
}

#[test]
fn root_signature_static_sampler_with_other_state_is_refused() {
    let mut ex = sampled_shader(&[], &[5]);
    ex.root_sig = Some(runtime_root_signature(vec![static_sampler(2, 3, 0.0), static_sampler(5, 1, 0.0)]));
    let Err(e) = translate(&ex) else { panic!("a wrap static sampler must not bind a clamp literal") };
    assert!(e.contains("matches no static sampler") && e.contains("s5, space1: address modes [1, 1, 1], literal [3, 3, 3]"), "{e}");
    let mut filtered = runtime_root_signature(vec![static_sampler(5, 3, 0.0)]);
    filtered.static_samplers[0].filter = 0x14;
    ex.root_sig = Some(filtered);
    let Err(e) = translate(&ex) else { panic!("a point-mip static sampler must not bind a trilinear literal") };
    assert!(e.contains("filter 0x14, literal 0x15"), "{e}");
}

const MERGED_ES_SLOT: usize = 0x28;
const HW_VS_SLOT: usize = 0x38;
const LOAD_TABLE_AT_S16: [u32; 2] = [0xf408_0108, 0xfa00_0000];
const POSITION_EXPORT: [u32; 4] = [0xbf8c_3f70, 0xf800_08cf, 0x0706_0504, 0xbf81_0000];

fn vertex_with_stage_slot(slot: Option<usize>) -> ShaderExtract {
    let words: Vec<u32> = [&LOAD_TABLE_AT_S16[..], &[0xbf8c_c07f], &BUF_LOAD_V4, &POSITION_EXPORT].concat();
    let mut table = fssm(9, &[[3, 0, 1, 0, 8]], &[[0x300, 0, 0, 4, 0x0804]]);
    table[0x40..0x48].copy_from_slice(&0u64.to_le_bytes());
    if let Some(at) = slot {
        table[at..at + 8].copy_from_slice(&0xB0u64.to_le_bytes());
    }
    let mut ex = pixel_shader(&words, table);
    ex.stage = Stage::Vertex;
    ex.output_sig = vec![SigElement { name: "SV_Position".into(), index: 0, sysval: 1, comp_type: 3, register: 0, mask: 0xf, rw_mask: 0 }];
    ex.has_root_sig = true;
    ex.root_sig = Some(engine_srv_table());
    ex
}

#[test]
fn merged_es_vertex_shader_reads_user_data_where_the_stage_slot_puts_it() {
    let t = translate(&vertex_with_stage_slot(Some(MERGED_ES_SLOT))).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("register(t0)"), "{}", t.hlsl);
    compile("merged ES vertex shader", &t.hlsl, t.profile);
}

#[test]
fn without_an_agreeing_table_the_static_guess_stays() {
    let Err(e) = translate(&vertex_with_stage_slot(Some(HW_VS_SLOT))) else { panic!("a hardware VS reads user data from s8, so s16 is no root table") };
    assert!(e.contains("buffer in table of kind None"), "{e}");
    let mut none = vertex_with_stage_slot(Some(MERGED_ES_SLOT));
    none.fssm[0xE8..0xF0].copy_from_slice(&0xDEAD_BEEF_DEAD_BEEFu64.to_le_bytes());
    assert!(translate(&none).err().is_some_and(|e| e.contains("buffer in table of kind None")));
    let mut disagree = vertex_with_stage_slot(Some(MERGED_ES_SLOT));
    let mut rs = engine_srv_table();
    rs.params.push(rs.params[0].clone());
    disagree.root_sig = Some(rs);
    assert!(translate(&disagree).err().is_some_and(|e| e.contains("buffer in table of kind None")));
}

#[test]
fn buffer_elements_take_the_fssm_stride() {
    let f = fssm(9, &[[3, 0, 1, 0, 8]], &[[0x300, 0, 0, 44, 0x0804], [0x300, 8, 1, 0, 0x0808]]);
    let t = translate_with_fssm_root(&pixel_shader(&program(), f)).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("struct TbElem_sbuf0 { uint w[11]; };"), "{}", t.hlsl);
    assert!(t.hlsl.contains("/ 11u].w["), "{}", t.hlsl);
    compile("stride 44", &t.hlsl, t.profile);
}

#[test]
fn conflicting_fssm_strides_keep_the_extent_heuristic() {
    let f = fssm(9, &[[3, 0, 1, 0, 8]], &[[0x300, 0, 0, 44, 0x0804], [0x300, 0, 0, 32, 0x0804], [0x300, 8, 1, 0, 0x0808]]);
    let t = translate_with_fssm_root(&pixel_shader(&program(), f)).unwrap_or_else(|e| panic!("{e}"));
    assert!(t.hlsl.contains("struct TbElem_sbuf0 { uint w[1]; };"), "{}", t.hlsl);
}
