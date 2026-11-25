use super::*;

fn fetch(link: u32, value: u32) -> Result<[u32; 4], String> {
    let input = |name, format, offset| VertexElement { semantic: name, semantic_index: 0, format, slot: 0, offset, input_class: 0, step_rate: 0 };
    let layout = [input(b"POSITION\0".as_ptr(), 16, 0), input(b"NORMAL\0".as_ptr(), 2, 8), input(b"TEXCOORD\0".as_ptr(), 42, 24)];
    let swap = 0xbe80_2100 | (link << 16) | link;
    let mut vs = shader(&[swap, 0xf800_00cf, 0x0706_0504, 0xf800_0a0f, 0x0b0a_0908, S_ENDPGM]);
    vs.stage = Stage::Vertex;
    vs.input_sig = vec![sig("POSITION", 0, 3, 0, 3), sig("NORMAL", 0, 3, 1, 0xf), sig("TEXCOORD", 0, 1, 3, 4)];
    vs.input_sig[0].rw_mask = 3;
    vs.input_sig[2].rw_mask = 4;
    vs.output_sig = vec![sig("SV_Position", 1, 3, 0, 0xf), sig("TEXCOORD", 0, 1, 1, 0xf)];
    let vs = translate(&vs)?;
    let vertex_code = dxc::compile(&vs.hlsl, &dxc::profile_for(vs.profile))?;
    let pixel_code = dxc::compile("uint4 main(float4 p : SV_Position, nointerpolation uint4 v : TEXCOORD0) : SV_Target { return v ^ uint4(uint2(p.xy), 0, 0); }", "ps_6_0")?;
    let vertices: Vec<u8> = [(-1.0f32, 1.0f32), (3.0, 1.0), (-1.0, -3.0)]
        .into_iter()
        .flat_map(|(x, y)| [x.to_bits(), y.to_bits(), 0x41a8_0000, 0, 0, 0, value])
        .flat_map(u32::to_le_bytes)
        .collect();
    let mut c = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() })).lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vb = c.gpu.buffer(vertices.len() as u64, 2, 0xac3, false)?;
        assert!(Gpu::map_write(vb, &vertices));
        let view = [Gpu::gpu_va(vb), ((28u64) << 32) | vertices.len() as u64];
        let rs = c.rs;
        let result = draw_graphics(&mut c, &pixel_code, rs, &|list| {
            vcall!(list, 44, fn(u32, u32, *const u64) -> (), 0, 1, view.as_ptr());
        }, &|_| {}, Some((&vertex_code, &layout)));
        d3d::release(vb);
        result.map_err(|e| format!("{e}\n{}", vs.hlsl))
    }
}

#[test]
fn s_swappc_b64_vertex_fetch_call_delivers_every_u32_value() {
    let mut bad = Vec::new();
    for link in [4u32, 12] {
        for value in [0u32, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0xdead_beef, 0x0100_0001] {
            match fetch(link, value) {
                Ok(got) if got == [0, 0, value, 0x3f80_0000] => {}
                other => bad.push(format!("s_swappc_b64 s{link}, s{link}, value {value:08x}: {other:08x?}")),
            }
        }
    }
    let all = std::collections::BTreeSet::from(["s_swappc_b64"]);
    let failed = if bad.is_empty() { Default::default() } else { all.clone() };
    verified("vertex_fetch_call", &all, &failed);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
