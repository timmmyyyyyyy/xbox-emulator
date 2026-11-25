use super::*;

fn vintrp(op: u32, vdst: u32, attr: u32, chan: u32, vsrc: u32) -> u32 {
    0xc800_0000 | (vdst << 18) | (op << 16) | (attr << 10) | (chan << 8) | vsrc
}

const VS_ATTRIBUTES: &str = "struct O { float4 p : SV_Position; float4 a : TEXCOORD0; float4 b : TEXCOORD1; nointerpolation uint4 f : TEXCOORD2; };
O main(uint id : SV_VertexID) {
    O o;
    float2 q = float2((id << 1) & 2, id & 2);
    o.p = float4(q * float2(2, -2) + float2(-1, 1), 0, 1);
    float k = id == 0 ? 1.0 : (id == 1 ? 2.0 : 3.0);
    o.a = float4(4.0, 8.0, 16.0, 32.0) * k;
    o.b = float4(1, 2, 3, 4) * (2.0 * id + 1.0);
    o.f = uint4(0x11, 0x22, 0x33, 0x44) + uint4(id, id, id, id) * 0x100;
    return o;
}";

fn element(name: &str, index: u32, comp_type: u32, register: u32) -> SigElement {
    SigElement { name: name.into(), index, sysval: 0, comp_type, register, mask: 0xf, rw_mask: 0xf }
}

fn run_interpolation(body: &[u32]) -> Result<[u32; 4], String> {
    let mut words = body.to_vec();
    words.extend_from_slice(&[0xf800_180f, 0x0302_0100, S_ENDPGM]);
    let mut ex = shader(&words);
    ex.input_sig = vec![sig("SV_Position", 1, 3, 0, 0xf), element("TEXCOORD", 0, 3, 1), element("TEXCOORD", 1, 3, 2), element("TEXCOORD", 2, 1, 3)];
    let t = translate(&ex)?;
    let ps = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let vs = dxc::compile(VS_ATTRIBUTES, "vs_6_0").map_err(|e| format!("vs: {e}"))?;
    let mut c = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() })).lock().unwrap_or_else(|e| e.into_inner());
    let rs = c.rs;
    unsafe { draw_graphics(&mut c, &ps, rs, &|_| {}, &|_| {}, Some((&vs, &[]))) }.map_err(|e| format!("{e}\n{}", t.hlsl))
}

fn pair(vdst: u32, attr: u32, chan: u32) -> [u32; 2] {
    [vintrp(0, vdst, attr, chan, 0), vintrp(1, vdst, attr, chan, 1)]
}

#[test]
fn v_interp_p1_and_p2_give_the_attribute_at_the_pixel_centre() {
    let body: Vec<u32> = (0..4u32).flat_map(|c| pair(c, 0, c)).collect();
    let got = run_interpolation(&body).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [7.0f32, 14.0, 28.0, 56.0].map(f32::to_bits));
    let body: Vec<u32> = (0..4u32).flat_map(|c| pair(c, 1, c)).collect();
    let got = run_interpolation(&body).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [2.5f32, 5.0, 7.5, 10.0].map(f32::to_bits));
    let mut body = Vec::new();
    for (dst, attr, chan) in [(0u32, 1u32, 3u32), (1, 0, 0), (2, 1, 1), (3, 0, 2)] {
        body.extend_from_slice(&pair(dst, attr, chan));
    }
    let got = run_interpolation(&body).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [10.0f32, 7.0, 5.0, 28.0].map(f32::to_bits));
}

#[test]
fn v_interp_mov_reads_the_flat_attribute_of_the_provoking_vertex() {
    let body: Vec<u32> = (0..4u32).map(|c| vintrp(2, c, 2, c, 2)).collect();
    let got = run_interpolation(&body).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, [0x11, 0x22, 0x33, 0x44]);
}
