use tombox_xshader::hlsl::translate;
use tombox_xshader::meta::SigElement;
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

#[test]
fn environment_switch_opts_plain_translation_into_the_table() {
    let ex = pixel_shader(&program(), table_with_buffer_and_texture());
    std::env::remove_var("TOMBOX_FSSM_ROOT_SIG");
    assert!(translate(&ex).is_err());
    std::env::set_var("TOMBOX_FSSM_ROOT_SIG", "0");
    assert!(translate(&ex).is_err());
    std::env::set_var("TOMBOX_FSSM_ROOT_SIG", "1");
    let t = translate(&ex);
    std::env::remove_var("TOMBOX_FSSM_ROOT_SIG");
    let t = t.unwrap_or_else(|e| panic!("opted-in translation failed: {e}"));
    let direct = translate_with_fssm_root(&ex).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(t.hlsl, direct.hlsl);
}
