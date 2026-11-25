
use tombox_xshader::hlsl::{ps_input_interp, translate, translate_linked};
use tombox_xshader::meta::SigElement;
use tombox_xshader::{ShaderExtract, Stage};

#[cfg(windows)]
#[allow(dead_code)]
#[path = "../../tools/xreplay/src/d3d.rs"]
mod d3d;

const S_ENDPGM: u32 = 0xbf81_0000;
const S_NOP: u32 = 0xbf80_0000;

fn sig(name: &str, index: u32, sysval: u32, comp_type: u32, register: u32, mask: u8) -> SigElement {
    SigElement { name: name.into(), index, sysval, comp_type, register, mask, rw_mask: mask }
}

fn shader(stage: Stage, words: &[u32], input_sig: Vec<SigElement>, output_sig: Vec<SigElement>) -> ShaderExtract {
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
        has_root_sig: false,
        input_sig,
        output_sig,
        prim_sig: Vec::new(),
        resources: Vec::new(),
        gs_info: None,
        root_sig: None,
    }
}

fn interpolants() -> Vec<SigElement> {
    vec![
        sig("TEXCOORD", 0, 0, 3, 0, 0x3),
        sig("SV_Position", 0, 1, 3, 1, 0xf),
        sig("TEXCOORD1_centroid", 0, 0, 3, 2, 0x3),
        sig("TEXCOORD", 2, 0, 1, 3, 0x1),
    ]
}

fn pair() -> (ShaderExtract, ShaderExtract) {
    let ps_words = [
        0xc80a_0000, // v_interp_mov_f32 v2, p10, attr0.x
        0xc80e_0400, // v_interp_mov_f32 v3, p10, attr1.x
        0xc812_0800, // v_interp_mov_f32 v4, p10, attr2.x
        S_NOP,
        0xf800_180f, // exp mrt0 v2, v3, v4, v4 (done, valid mask)
        0x0404_0302,
        S_ENDPGM,
    ];
    let ps = shader(Stage::Pixel, &ps_words, interpolants(), vec![sig("SV_Target", 0, 64, 3, 0, 0xf)]);
    let vs_words = [
        0x7e10_0287, // v_mov_b32 v8, 7
        0x7e02_02f2, // v_mov_b32 v1, 1.0
        0xf800_08cf, // exp pos0 v1, v1, v1, v1 (done)
        0x0101_0101,
        0xf800_020f, // exp param0 v1, v1, v1, v1
        0x0101_0101,
        0xf800_021f, // exp param1 v1, v1, v1, v1
        0x0101_0101,
        0xf800_022f, // exp param2 v8, v8, v8, v8
        0x0808_0808,
        S_ENDPGM,
    ];
    let vs_out = interpolants().into_iter().map(|e| SigElement { rw_mask: 0, ..e }).collect();
    let vs = shader(Stage::Vertex, &vs_words, vec![], vs_out);
    (ps, vs)
}

#[test]
fn integer_vertex_selectors_read_the_requested_vertex() {
    let words = [0xc802_0002, 0xc806_0100, 0xc80a_0201, 0xc80e_0301, 0xf800_180f, 0x0302_0100, S_ENDPGM];
    let ps = shader(
        Stage::Pixel,
        &words,
        vec![sig("TEXCOORD", 0, 0, 1, 0, 0xf)],
        vec![sig("SV_Target", 0, 64, 1, 0, 0xf)],
    );
    let t = translate(&ps).expect("PS");
    assert!(t.hlsl.contains("TB_WV((uint)(i.i0.x), v0)"), "{}", t.hlsl);
    assert!(t.hlsl.contains("GetAttributeAtVertex(i.i0, 1).y"), "{}", t.hlsl);
    assert!(t.hlsl.contains("GetAttributeAtVertex(i.i0, 2).z"), "{}", t.hlsl);
    assert!(t.hlsl.contains("GetAttributeAtVertex(i.i0, 2).w"), "{}", t.hlsl);
    assert_eq!(t.profile, "ps_6_2");
}

#[test]
fn stage_feeding_a_pixel_shader_gets_its_interpolation_modifiers() {
    let (ps, vs) = pair();
    let ps_t = translate(&ps).expect("PS");
    assert!(ps_t.hlsl.contains("    TB_CEN_Q float2 i2 : TEXCOORD1_centroid0;"), "{}", ps_t.hlsl);
    assert!(ps_t.hlsl.contains("    nointerpolation uint i3 : TEXCOORD2;"), "{}", ps_t.hlsl);
    let link = ps_input_interp(&ps);
    assert_eq!(link.0, [("TEXCOORD1_centroid0".to_string(), "TB_CEN_Q".to_string())]);
    assert!(ps_input_interp(&vs).0.is_empty());
    let unlinked = translate(&vs).unwrap().hlsl;
    assert_eq!(translate_linked(&vs, Some(&link)).unwrap().hlsl, unlinked);

    let vs_t = translate_linked(&vs, Some(&link.clone().pc_centroid())).expect("linked VS");
    assert!(vs_t.hlsl.contains("    centroid float2 o2 : TEXCOORD1_centroid0;"), "{}", vs_t.hlsl);
    assert!(vs_t.hlsl.contains("    float2 o0 : TEXCOORD0;"), "{}", vs_t.hlsl);
    assert!(vs_t.hlsl.contains("    uint o3 : TEXCOORD2;"), "{}", vs_t.hlsl);
    assert!(vs_t.hlsl.contains("    precise float4 o1 : SV_Position0;"), "{}", vs_t.hlsl);
    assert_eq!(unlinked, translate_linked(&vs, None).unwrap().hlsl);
    assert!(!unlinked.contains("centroid float2"), "{unlinked}");
    assert_eq!(translate_linked(&ps, Some(&link)).unwrap().hlsl, ps_t.hlsl);
}

#[test]
fn integer_output_keeps_no_float_modifier() {
    let (_, mut vs) = pair();
    vs.output_sig[2].comp_type = 1;
    let link = tombox_xshader::hlsl::InputInterp(vec![("TEXCOORD1_centroid0".into(), "centroid".into())]);
    let t = translate_linked(&vs, Some(&link)).expect("linked VS");
    assert!(t.hlsl.contains("    uint2 o2 : TEXCOORD1_centroid0;"), "{}", t.hlsl);
}

#[cfg(windows)]
mod d3d12 {
    use super::*;
    use crate::d3d::{self, Guid};
    use core::ffi::c_void;
    use tombox_xshader::{dxc, meta, Dxbc};

    extern "system" {
        fn LoadLibraryA(n: *const u8) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
    }

    fn compile(t: &tombox_xshader::hlsl::Translated) -> Vec<u8> {
        compile_src(&t.hlsl, t)
    }

    fn compile_src(src: &str, t: &tombox_xshader::hlsl::Translated) -> Vec<u8> {
        dxc::compile(src, &dxc::profile_for(t.profile)).unwrap_or_else(|e| panic!("{e}\n{src}"))
    }

    fn signature(code: &[u8], cc: &[u8; 4]) -> Vec<SigElement> {
        let c = Dxbc::parse(code).expect("DXIL container");
        let ch = c.chunk(cc).expect("signature chunk");
        meta::parse_signature(cc, &code[ch.offset..ch.offset + ch.size]).expect("signature")
    }

    fn rows(vs: &[u8], ps: &[u8]) -> Vec<(String, u32, u8, u32, u8)> {
        let outs = signature(vs, b"OSG1");
        signature(ps, b"ISG1")
            .into_iter()
            .map(|i| {
                let o = outs.iter().find(|o| o.name == i.name && o.index == i.index).expect("output for every input");
                (format!("{}{}", i.name, i.index), i.register, i.mask, o.register, o.mask)
            })
            .collect()
    }

    fn agree(rows: &[(String, u32, u8, u32, u8)]) -> bool {
        rows.iter().all(|(_, pr, pm, vr, vm)| pr == vr && pm & !vm == 0)
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

    unsafe fn root_signature(device: *mut c_void) -> *mut c_void {
        let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
        let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
            core::mem::transmute(GetProcAddress(d3d, b"D3D12SerializeRootSignature\0".as_ptr()));
        let desc = [0u8; 40];
        let mut blob: *mut c_void = core::ptr::null_mut();
        let mut err: *mut c_void = core::ptr::null_mut();
        assert!(ser(desc.as_ptr(), 1, &mut blob, &mut err) >= 0, "serialize root signature");
        let p = vcall!(blob, 3, fn() -> *const u8);
        let n = vcall!(blob, 4, fn() -> usize);
        let mut rs: *mut c_void = core::ptr::null_mut();
        let hr = vcall!(device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, p, n, &d3d::IID_ROOTSIG, &mut rs);
        assert!(hr >= 0, "CreateRootSignature {hr:#x}");
        rs
    }

    unsafe fn create_pso(device: *mut c_void, rs: *mut c_void, vs: &[u8], ps: &[u8]) -> i32 {
        let mut d = vec![0u8; 656];
        let put32 = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let put64 = |d: &mut Vec<u8>, o: usize, v: u64| d[o..o + 8].copy_from_slice(&v.to_le_bytes());
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
        put32(&mut d, 448, 0xffff_ffff); // SampleMask
        put32(&mut d, 452, 3); // FillMode solid
        put32(&mut d, 456, 1); // CullMode none
        put32(&mut d, 476, 1); // DepthClipEnable
        put32(&mut d, 504, 8);
        for f in [516usize, 532] {
            put32(&mut d, f, 1);
            put32(&mut d, f + 4, 1);
            put32(&mut d, f + 8, 1);
            put32(&mut d, f + 12, 8);
        }
        put32(&mut d, 572, 3); // triangles
        put32(&mut d, 576, 1); // one render target
        put32(&mut d, 580, 2); // R32G32B32A32_FLOAT
        put32(&mut d, 616, 1); // one sample
        let mut pso: *mut c_void = core::ptr::null_mut();
        let hr = vcall!(device, 10, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
        d3d::release(pso);
        hr
    }

    #[test]
    fn linked_pair_compiles_to_matching_signatures_and_creates_a_pipeline() {
        if !dxc::available() {
            eprintln!("SKIPPED: DXC not available");
            return;
        }
        let (ps, vs) = pair();
        let ps_t = translate(&ps).expect("PS");
        let ps_code = compile_src(&format!("#define {}\n{}", tombox_xshader::centroid_abi::TB_CENTROID_PC_DEFINE, ps_t.hlsl), &ps_t);
        let unlinked = compile(&translate(&vs).expect("VS"));
        let linked = compile(&translate_linked(&vs, Some(&ps_input_interp(&ps).pc_centroid())).expect("linked VS"));

        let before = rows(&unlinked, &ps_code);
        assert!(!agree(&before), "the unlinked pair should reproduce the mismatch: {before:?}");
        let after = rows(&linked, &ps_code);
        assert_eq!(after.len(), 4, "{after:?}");
        assert!(agree(&after), "linked pair must agree on registers: {after:?}");

        unsafe {
            let device = warp_device();
            let rs = root_signature(device);
            let hr = create_pso(device, rs, &unlinked, &ps_code);
            assert!(hr < 0, "D3D12 accepted the mismatched pair ({hr:#010x})");
            let hr = create_pso(device, rs, &linked, &ps_code);
            assert!(hr >= 0, "CreateGraphicsPipelineState(linked) hr={:#010x}", hr as u32);
            d3d::release(rs);
            d3d::release(device);
        }
    }
}
