#![allow(dead_code)]

use compat_common::record;
use core::ffi::c_void;
use std::sync::Arc;

pub const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
pub const SUB_ROOT_SIGNATURE: u32 = 0;
pub const SUB_VS: u32 = 1;
pub const SUB_PS: u32 = 2;
pub const SUB_DS: u32 = 3;
pub const SUB_HS: u32 = 4;
pub const SUB_GS: u32 = 5;
pub const SUB_CS: u32 = 6;
pub const SUB_PRIMITIVE_TOPOLOGY: u32 = 14;
pub const SUB_AS: u32 = 24;
pub const SUB_MS: u32 = 25;
pub const XBOX_SUB_AS: u32 = 23;
pub const XBOX_SUB_MS: u32 = 24;
pub const XBOX_SUB_MAX: u32 = 30;
pub const PC_DEVICE2_CREATE_PIPELINE_STATE: usize = 47;
pub const PC_LIST6_DISPATCH_MESH: usize = 79;

#[repr(C)]
pub struct Guid(pub u32, pub u16, pub u16, pub [u8; 8]);
pub const IID_DEVICE2: Guid = Guid(0x30baa41e, 0xb15b, 0x475c, [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28]);
pub const IID_LIST6: Guid = Guid(0xc3827890, 0xe548, 0x4cfa, [0x96, 0xcf, 0x56, 0x89, 0xa9, 0x37, 0x0f, 0x80]);

pub fn inner_layout(ty: u32) -> Option<(usize, usize)> {
    Some(match ty {
        0 => (8, 8),
        1..=6 | 24 | 25 => (8, 16),
        7 => (8, 32),
        8 => (4, 328),
        9 | 13 | 14 | 16 | 18 | 20 => (4, 4),
        10 => (4, 44),
        11 => (4, 52),
        21 => (4, 56),
        12 | 19 => (8, 16),
        15 => (4, 36),
        17 => (4, 8),
        22 => (8, 24),
        26 => (4, 60),
        27 => (4, 44),
        28 => (4, 40),
        29 => (8, 16),
        _ => return None,
    })
}

fn align(x: usize, a: usize) -> usize {
    (x + a - 1) & !(a - 1)
}

pub const CONSOLE_RECORD_SIZE: [usize; 31] = [16, 24, 24, 24, 24, 24, 24, 40, 336, 8, 48, 56, 24, 8, 8, 40, 8, 16, 8, 24, 8, 64, 32, 24, 24, 40, 32, 64, 16, 32, 16];

pub const CONSOLE_EXT: [(u32, &str, usize, usize); 6] = [
    (25, "D3D12XBOX_PIXEL_SHADER_OPTIONS_DESC", 4, 32),
    (26, "DEPTH_STENCIL_OPTIONS", 4, 24),
    (27, "D3D12XBOX_GRAPHICS_SHADER_LIMITS_DESC", 4, 60),
    (28, "D3D12XBOX_PIPELINE_TESSELLATION_PARAMETERS_DESC", 4, 12),
    (29, "D3D12XBOX_COMPUTE_SHADER_LIMITS_DESC", 4, 24),
    (30, "pointer subobject", 8, 8),
];

pub fn pc_record_size(ty: u32) -> Option<usize> {
    inner_layout(ty).map(|(a, n)| align(align(4, a) + n, 8))
}

pub fn console_record_size(ty: u32) -> Option<usize> {
    CONSOLE_RECORD_SIZE.get(ty as usize).copied()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbering {
    Pc,
    Console,
}

type Rec = (u32, usize, usize);

fn word(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn hex_words(p: &[u8]) -> String {
    p.chunks(4)
        .map(|c| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            format!("{:#x}", u32::from_le_bytes(b))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn scan(stream: &[u8], pc: bool) -> Result<Vec<Rec>, String> {
    let size_of = if pc { pc_record_size } else { console_record_size };
    let slot = |t: u32| if t == 21 || (pc && t == 26) { 11 } else { t };
    let mut recs: Vec<Rec> = Vec::new();
    let mut at = 0usize;
    while at < stream.len() {
        let ty = u32::from_le_bytes(stream.get(at..at + 4).ok_or("truncated subobject type")?.try_into().unwrap());
        let Some(sz) = size_of(ty) else {
            let tail = &stream[at..stream.len().min(at + 96)];
            let words: Vec<String> = tail.chunks(4).map(|c| c.iter().rev().map(|b| format!("{b:02x}")).collect()).collect();
            return Err(format!("pipeline subobject type {ty} at {at:#x} unsupported (stream {} bytes; next words {})", stream.len(), words.join(" ")));
        };
        if at + sz > stream.len() {
            return Err(format!("pipeline subobject type {ty} at {at:#x} runs past the stream end"));
        }
        if recs.iter().any(|r| slot(r.0) == slot(ty)) {
            return Err(format!("pipeline subobject type {ty} appears twice"));
        }
        recs.push((ty, at, sz));
        at += sz;
    }
    Ok(recs)
}

fn psv0_mesh_info(dxbc: &[u8]) -> Option<tombox_xshader::meta::MeshInfo> {
    if dxbc.len() < 32 || &dxbc[0..4] != b"DXBC" {
        return None;
    }
    let count = u32::from_le_bytes(dxbc[28..32].try_into().ok()?) as usize;
    if 32usize.checked_add(count.checked_mul(4)?)? > dxbc.len() {
        return None;
    }
    for i in 0..count {
        let off = u32::from_le_bytes(dxbc[32 + i * 4..36 + i * 4].try_into().ok()?) as usize;
        if off.checked_add(8)? > dxbc.len() || &dxbc[off..off + 4] != b"PSV0" {
            continue;
        }
        let sz = u32::from_le_bytes(dxbc[off + 4..off + 8].try_into().ok()?) as usize;
        let body = dxbc.get(off + 8..off + 8 + sz)?;
        let info_sz = u32::from_le_bytes(body.get(0..4)?.try_into().ok()?) as usize;
        return tombox_xshader::meta::parse_psv0_mesh(body.get(4..4 + info_sz)?);
    }
    None
}

fn record_stage(stream: &[u8], at: usize) -> Option<u8> {
    let (p, n) = bytecode(stream, at + 8);
    if p == 0 || n == 0 {
        return None;
    }
    psv0_mesh_info(unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) }).map(|m| m.stage)
}

pub fn classify(stream: &[u8]) -> Result<(Numbering, Vec<Rec>), String> {
    match (scan(stream, true), scan(stream, false)) {
        (Ok(p), Err(_)) => Ok((Numbering::Pc, p)),
        (Err(_), Ok(c)) => Ok((Numbering::Console, c)),
        (Err(_), Err(e)) => Err(e),
        (Ok(p), Ok(c)) => {
            let types: Vec<u32> = c.iter().map(|r| r.0).collect();
            if p == c && types.iter().all(|&t| t <= 22) {
                return Ok((Numbering::Pc, p));
            }
            if p == c && types.iter().all(|&t| t <= XBOX_SUB_MS) {
                let at = c.iter().find(|r| r.0 == XBOX_SUB_MS).map(|r| r.1).unwrap();
                match record_stage(stream, at) {
                    Some(13) => return Ok((Numbering::Console, c)),
                    Some(14) => return Ok((Numbering::Pc, p)),
                    _ => {}
                }
            }
            Err(format!("ambiguous pipeline stream: subobjects {types:?} parse with both the PC and the console record sizes and neither a record size nor the type-24 shader stage tells them apart"))
        }
    }
}

fn console_ds_options_default(p: &[u8]) -> bool {
    p[0..20].iter().all(|&b| b == 0) && p[20] == 1 && p[21] == 1
}

fn ds_options(p: &[u8]) -> crate::pso_ext::DsExt {
    crate::pso_ext::DsExt {
        backface_disable: word(p, 0),
        depth_bounds_disable: word(p, 4),
        color_on_depth_fail: word(p, 8),
        no_color_on_depth_pass: word(p, 0xc),
        back_read: p[0x10],
        back_write: p[0x11],
        front_ref: p[0x12],
        back_ref: p[0x13],
        front_op: p[0x14],
        back_op: p[0x15],
    }
}

const PC_DEFAULT_DEPTH_STENCIL: [u32; 13] = [1, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8];

pub fn apply_ds_options(ds: &mut [u8; 52], e: &crate::pso_ext::DsExt, per_face_ok: bool) -> Result<(Option<(u8, u8)>, Vec<&'static str>), String> {
    if e.color_on_depth_fail != 0 || e.no_color_on_depth_pass != 0 {
        return Err("ENABLE_COLOR_WRITES_ON_DEPTH_FAIL / DISABLE_COLOR_WRITES_ON_DEPTH_PASS have no PC equivalent".into());
    }
    let mut did = Vec::new();
    if word(ds, 12) == 0 {
        return Ok((None, did));
    }
    if e.front_ref != 0 || e.back_ref != 0 {
        return Err(format!("StencilTestRefValueFront/Back {:#x}/{:#x} are combined with OMSetStencilRef on the console; no PC equivalent", e.front_ref, e.back_ref));
    }
    let back_op = if e.backface_disable != 0 {
        let front: [u8; 16] = ds[20..36].try_into().unwrap();
        ds[36..52].copy_from_slice(&front);
        did.push("BACKFACE_ENABLE=0: back faces use the front-face stencil state");
        e.front_op
    } else {
        e.back_op
    };
    for (face, op) in [(20usize, e.front_op), (36, back_op)] {
        match op {
            1 => {}
            0 => {
                for k in 0..3 {
                    let o = face + 4 * k;
                    if matches!(word(ds, o), 4 | 5 | 7 | 8) {
                        ds[o..o + 4].copy_from_slice(&1u32.to_le_bytes());
                        if !did.contains(&"STENCILOPVAL=0: INCR/DECR stencil ops add 0, mapped to KEEP") {
                            did.push("STENCILOPVAL=0: INCR/DECR stencil ops add 0, mapped to KEEP");
                        }
                    }
                }
            }
            v => return Err(format!("STENCILOPVAL {v} (stencil INCR/DECR step) has no PC equivalent")),
        }
    }
    let front = (ds[16], ds[17]);
    let back = if e.backface_disable != 0 { front } else { (e.back_read, e.back_write) };
    if back == front {
        return Ok((None, did));
    }
    if !per_face_ok {
        return Err(format!("back-face stencil masks {:#x}/{:#x} differ from the front-face masks {:#x}/{:#x} and the device lacks IndependentFrontAndBackStencilRefMaskSupported", back.0, back.1, front.0, front.1));
    }
    did.push("back-face stencil read/write masks: DEPTH_STENCIL2");
    Ok((Some(back), did))
}

fn ds_record(src_ty: u32, ds: &[u8; 52], bounds: u32, back: Option<(u8, u8)>) -> Vec<u8> {
    let mut r = Vec::with_capacity(64);
    match back {
        Some((br, bw)) => {
            r.extend_from_slice(&26u32.to_le_bytes());
            r.extend_from_slice(&ds[0..16]);
            r.extend_from_slice(&ds[20..36]);
            r.extend_from_slice(&[ds[16], ds[17], 0, 0]);
            r.extend_from_slice(&ds[36..52]);
            r.extend_from_slice(&[br, bw, 0, 0]);
            r.extend_from_slice(&bounds.to_le_bytes());
        }
        None => {
            r.extend_from_slice(&src_ty.to_le_bytes());
            r.extend_from_slice(ds);
            if src_ty == 21 {
                r.extend_from_slice(&bounds.to_le_bytes());
            }
        }
    }
    while r.len() % 8 != 0 {
        r.push(0);
    }
    r
}

pub fn console_to_pc(stream: &[u8], recs: &[Rec], per_face_ok: &dyn Fn() -> bool) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut notes = Vec::new();
    let ds_rec = recs.iter().find(|r| r.0 == 11 || r.0 == 21).copied();
    let mut ds_out: Option<Vec<u8>> = None;
    for &(ty, at, sz) in recs.iter().filter(|r| r.0 >= 25) {
        let (_, name, off, len) = *CONSOLE_EXT.iter().find(|e| e.0 == ty).unwrap();
        let p = &stream[at + off..at + off + len];
        let handler = match ty { 25 => 0x8a540u32, 26 => 0x8a5f0, 27 => 0x8a6d0, 28 => 0x8a7f0, 29 => 0x8a870, _ => 0x8ecb0 };
        let tag = format!("console subobject {ty} ({name}, {sz}-byte record, payload [{}], xgs12_10d parser 0x8e100 handler {handler:#x})", hex_words(p));
        match ty {
            25 if p.iter().all(|&b| b == 0) => notes.push(format!("{tag}: all fields default, so the console stream parser leaves the extension unset; dropped")),
            25 => return Err(format!("{tag}: non-default pixel-shader options {:?} are not emulated in a pipeline stream", crate::pso_ext::Block::PsOptions.non_default(p))),
            26 if console_ds_options_default(p) => notes.push(format!("{tag}: console default, so the console stream parser leaves the extension unset; dropped")),
            26 => {
                let e = ds_options(p);
                let mut ds = [0u8; 52];
                let mut bounds = 0u32;
                match ds_rec {
                    Some((dty, dat, _)) => {
                        ds.copy_from_slice(&stream[dat + 4..dat + 56]);
                        if dty == 21 {
                            bounds = word(stream, dat + 56);
                        }
                    }
                    None => {
                        for (k, v) in PC_DEFAULT_DEPTH_STENCIL.iter().enumerate() {
                            ds[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                }
                let (back, did) = apply_ds_options(&mut ds, &e, per_face_ok()).map_err(|m| format!("{tag}: {m}"))?;
                if let Some((dty, _, _)) = ds_rec {
                    ds_out = Some(ds_record(dty, &ds, bounds, back));
                }
                let change = if did.is_empty() { "no PC-visible change".to_string() } else { did.join("; ") };
                notes.push(format!("{tag}: mapped ({change}); its DepthBoundsDisable word is overwritten by DepthBoundsTestEnable of the depth-stencil subobject"));
            }
            27 => notes.push(format!("{tag}: CU and wave scheduling limits with no D3D12 PC API, rendered results are unaffected; dropped")),
            28 if word(p, 0) == 0 && f32::from_bits(word(p, 4)) == 0.0 && word(p, 8) == 0 => notes.push(format!("{tag}: all fields default, so the console stream parser leaves the extension unset; dropped")),
            28 => return Err(format!("{tag}: non-default tessellation parameters {:?} are not emulated", crate::pso_ext::Block::Tessellation.non_default(p))),
            29 => notes.push(format!("{tag}: compute CU and wave scheduling limits, read only for compute pipelines and with no D3D12 PC API; dropped")),
            _ => notes.push(format!("{tag}: the console stream parser stores the pointer and the pipeline build never reads it; dropped")),
        }
    }
    let mut out = Vec::with_capacity(stream.len());
    for &(ty, at, sz) in recs {
        match ty {
            11 | 21 if ds_out.is_some() => out.extend_from_slice(ds_out.as_deref().unwrap()),
            0..=22 => out.extend_from_slice(&stream[at..at + sz]),
            XBOX_SUB_AS | XBOX_SUB_MS => {
                let (p, n) = bytecode(stream, at + 8);
                if p != 0 || n != 0 {
                    let pc_ty = if ty == XBOX_SUB_AS { SUB_AS } else { SUB_MS };
                    out.extend_from_slice(&pc_ty.to_le_bytes());
                    out.extend_from_slice(&0u32.to_le_bytes());
                    out.extend_from_slice(&stream[at + 8..at + 24]);
                }
            }
            _ => {}
        }
    }
    Ok((out, notes))
}

pub struct PcStream {
    pub stream: Vec<u8>,
    pub numbering: Numbering,
    pub types: Vec<u32>,
    pub notes: Vec<String>,
}

pub fn xbox_to_pc_stream(stream: &[u8], per_face_ok: &dyn Fn() -> bool) -> Result<PcStream, String> {
    let (numbering, recs) = classify(stream)?;
    let types = recs.iter().map(|r| r.0).collect();
    if numbering == Numbering::Pc {
        return Ok(PcStream { stream: stream.to_vec(), numbering, types, notes: Vec::new() });
    }
    let (out, notes) = console_to_pc(stream, &recs, per_face_ok)?;
    Ok(PcStream { stream: out, numbering, types, notes })
}

fn note_console_stream(types: &[u32], notes: &[String]) {
    use std::sync::Mutex;
    static LOGGED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let n = LOGGED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if n < 8 {
        record("d3d12_xs", "CreatePipelineState", &format!("console-numbered stream PSO #{} raw subobjects {types:?}", n + 1));
    }
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    for m in notes {
        if g.len() < 64 && !g.contains(m) {
            g.push(m.clone());
            record("d3d12_xs", "CreatePipelineState", m);
        }
    }
}

pub fn parse(stream: &[u8]) -> Result<Vec<(u32, usize)>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < stream.len() {
        let ty = u32::from_le_bytes(stream.get(at..at + 4).ok_or("truncated subobject type")?.try_into().unwrap());
        let (a, n) = inner_layout(ty).ok_or_else(|| format!("pipeline subobject type {ty} at {at:#x} unsupported"))?;
        let inner = at + align(4, a);
        if inner + n > stream.len() {
            return Err(format!("pipeline subobject type {ty} at {at:#x} runs past the stream end"));
        }
        if out.iter().any(|(t, _)| *t == ty) {
            return Err(format!("pipeline subobject type {ty} appears twice"));
        }
        out.push((ty, inner));
        at = align(inner + n, 8);
    }
    Ok(out)
}

fn bytecode(stream: &[u8], inner: usize) -> (u64, u64) {
    let p = u64::from_le_bytes(stream[inner..inner + 8].try_into().unwrap());
    let n = u64::from_le_bytes(stream[inner + 8..inner + 16].try_into().unwrap());
    (p, n)
}

pub fn validate(stream: &[u8], subs: &[(u32, usize)]) -> Result<(), String> {
    let has = |t: u32| subs.iter().find(|(x, _)| *x == t).map(|(_, o)| bytecode(stream, *o).0 != 0).unwrap_or(false);
    if has(SUB_MS) && has(SUB_VS) {
        return Err("When mesh shader is specified, vertex shader must be turned off".into());
    }
    if has(SUB_AS) && !has(SUB_MS) {
        return Err("amplification shader without a mesh shader".into());
    }
    Ok(())
}

pub const AS_ROOT_FLAG_ERR: &str = "When an amplification shader is used, the root signature must use D3D12XBOX_ROOT_SIGNATURE_FLAG_FORCE_MEMORY_BASED_ABI flag";

pub fn as_root_rule(stream: &[u8], subs: &[(u32, usize)], flags_of: &dyn Fn(u64) -> Option<u32>) -> Result<(), String> {
    let code = |t: u32| subs.iter().find(|(x, _)| *x == t).map(|(_, o)| bytecode(stream, *o)).filter(|(p, n)| *p != 0 && *n != 0);
    let Some((ap, an)) = code(SUB_AS) else { return Ok(()) };
    if code(SUB_MS).is_none() {
        return Ok(());
    }
    let rs = subs.iter().find(|(t, _)| *t == SUB_ROOT_SIGNATURE).map(|(_, o)| u64::from_le_bytes(stream[*o..*o + 8].try_into().unwrap())).unwrap_or(0);
    let flags = if rs != 0 {
        flags_of(rs).ok_or_else(|| format!("amplification PSO root signature {rs:#x} was not created through the console CreateRootSignature, its flags are unknown"))?
    } else {
        let blob = unsafe { core::slice::from_raw_parts(ap as *const u8, an as usize) };
        crate::console_slots_mesh::console_root_flags(blob).filter(|_| blob.get(0..4) == Some(b"DXBC")).ok_or("amplification PSO has no root signature subobject and its shader embeds none")?
    };
    if flags & crate::console_slots_mesh::FORCE_MEMORY_BASED_ABI == 0 {
        return Err(AS_ROOT_FLAG_ERR.into());
    }
    Ok(())
}

pub fn rt_formats(stream: &[u8], subs: &[(u32, usize)]) -> (u32, [u32; 8]) {
    let Some((_, o)) = subs.iter().find(|(t, _)| *t == 15) else { return (0, [0; 8]) };
    let mut f = [0u32; 8];
    for (k, v) in f.iter_mut().enumerate() {
        *v = u32::from_le_bytes(stream[o + 4 * k..o + 4 * k + 4].try_into().unwrap());
    }
    (u32::from_le_bytes(stream[o + 32..o + 36].try_into().unwrap()), f)
}

pub fn stage_rtfmt(ty: u32, (n, f): (u32, [u32; 8])) -> [u8; 8] {
    if ty == SUB_PS { crate::shader_xlat::export_formats(n, &f) } else { [0; 8] }
}

fn mesh_dxil_topology(dxbc: &[u8]) -> Option<u32> {
    match psv0_mesh_info(dxbc)?.topology {
        1 => Some(2),
        2 => Some(3),
        _ => None,
    }
}

fn stream_summary(stream: &[u8]) -> String {
    match parse(stream) {
        Ok(subs) => subs
            .iter()
            .map(|&(ty, inner)| {
                let w0 = stream.get(inner..inner + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0);
                format!("{ty}:{w0:#x}")
            })
            .collect::<Vec<_>>()
            .join(","),
        Err(e) => format!("parse-err({e})"),
    }
}

const XBOX_TOPOLOGY_TYPE_RECT: u32 = 0x13;

pub fn translate_stream(stream: &[u8], xlat: &dyn Fn(u32, &[u8], (u32, [u32; 8]), Option<&[u8]>) -> Result<Arc<Vec<u8>>, String>) -> Result<(Vec<u8>, Vec<Arc<Vec<u8>>>), String> {
    let subs = parse(stream)?;
    validate(stream, &subs)?;
    let rts = rt_formats(stream, &subs);
    let code_of = |t: u32| subs.iter().find(|(x, _)| *x == t).map(|(_, o)| bytecode(stream, *o)).filter(|(p, n)| *p != 0 && *n != 0);
    let feeder = [SUB_MS, SUB_GS, SUB_DS, SUB_VS].into_iter().find(|&t| code_of(t).is_some());
    let ps = code_of(SUB_PS).map(|(p, n)| unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) });
    let has_ms = code_of(SUB_MS).is_some();
    let mut ms_topo = code_of(SUB_MS).and_then(|(p, n)| mesh_dxil_topology(unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) }));
    let mut out = Vec::with_capacity(stream.len() + 8);
    let mut keep = Vec::new();
    let mut topo_dst: Option<usize> = None;
    for &(ty, inner) in &subs {
        let (a, n) = inner_layout(ty).unwrap();
        let start = inner - align(4, a);
        let end = align(inner + n, 8).min(stream.len());
        if has_ms && matches!(ty, SUB_VS | SUB_DS | SUB_HS | SUB_GS) {
            let (p, blen) = bytecode(stream, inner);
            if p == 0 || blen == 0 {
                continue;
            }
        }
        if has_ms && ty == SUB_PRIMITIVE_TOPOLOGY {
            if ms_topo.is_none() {
                continue;
            }
            topo_dst = Some(out.len() + (inner - start));
        }
        let base = out.len();
        out.extend_from_slice(&stream[start..end]);
        if !has_ms && ty == SUB_PRIMITIVE_TOPOLOGY && out.len() >= base + (inner - start) + 4 {
            let dst = base + (inner - start);
            let val = u32::from_le_bytes(out[dst..dst + 4].try_into().unwrap());
            if val == XBOX_TOPOLOGY_TYPE_RECT {
                out[dst..dst + 4].copy_from_slice(&3u32.to_le_bytes());
                static LOGGED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
                if LOGGED.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 8 {
                    record("d3d12_xs", "CreatePipelineState", &format!("Xbox rect-list primitive topology type {val:#x} mapped to TRIANGLE (3) for PC PSO stream"));
                }
            }
        }
        if !matches!(ty, 1..=6 | SUB_AS | SUB_MS) {
            continue;
        }
        let (p, n) = bytecode(stream, inner);
        if p == 0 || n == 0 {
            continue;
        }
        let blob = unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) };
        let feeds = if Some(ty) == feeder { ps } else { None };
        let code = xlat(ty, blob, rts, feeds).map_err(|e| format!("subobject {ty}: {e}"))?;
        if ty == SUB_MS && ms_topo.is_none() {
            ms_topo = mesh_dxil_topology(&code);
        }
        let dst = base + (inner - start);
        out[dst..dst + 8].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
        out[dst + 8..dst + 16].copy_from_slice(&(code.len() as u64).to_le_bytes());
        keep.push(code);
    }
    if has_ms {
        if let Some(t) = ms_topo {
            if let Some(off) = topo_dst {
                out[off..off + 4].copy_from_slice(&t.to_le_bytes());
            } else {
                while out.len() % 8 != 0 {
                    out.push(0);
                }
                out.extend_from_slice(&SUB_PRIMITIVE_TOPOLOGY.to_le_bytes());
                out.extend_from_slice(&t.to_le_bytes());
            }
        }
    }
    Ok((out, keep))
}

pub unsafe fn create_pipeline_state(real: *mut c_void, desc: *const [u64; 2], riid: *const c_void, ppv: *mut *mut c_void) -> i32 {
    if desc.is_null() || real.is_null() || ppv.is_null() {
        return E_INVALIDARG;
    }
    let [size, ptr] = *desc;
    if ptr == 0 || size == 0 {
        return E_INVALIDARG;
    }
    let raw = core::slice::from_raw_parts(ptr as *const u8, size as usize);
    let pc_stream = match xbox_to_pc_stream(raw, &|| unsafe { crate::pso_ext::caps(real).1 }) {
        Ok(s) => {
            if s.numbering == Numbering::Console {
                note_console_stream(&s.types, &s.notes);
            }
            s.stream
        }
        Err(e) => {
            record("d3d12_xs", "CreatePipelineState", &format!("{e} -> E_INVALIDARG"));
            return E_INVALIDARG;
        }
    };
    let stream = pc_stream.as_slice();
    let rule = parse(stream).and_then(|s| as_root_rule(stream, &s, &|rs| crate::console_slots_mesh::root_flags(rs as usize)));
    if let Err(e) = rule {
        record("d3d12_xs", "CreatePipelineState", &format!("{e} -> E_INVALIDARG"));
        return E_INVALIDARG;
    }
    let root = parse(stream).ok().and_then(|s| s.iter().find(|(t, _)| *t == 0).map(|&(_, o)| bytecode(stream, o).0)).unwrap_or(0);
    let pc_centroid = crate::centroid_bind::tag_rtfmt(root, [0; 8])[0] & crate::centroid_bind::RTFMT_PC_CENTROID != 0;
    let xlat = |ty: u32, b: &[u8], (n, f): (u32, [u32; 8]), feeds: Option<&[u8]>| {
        let link = feeds.map(|ps| crate::shader_xlat::ps_link(ps, pc_centroid)).unwrap_or_else(crate::shader_xlat::no_link);
        if ty == SUB_MS && root != 0 && crate::shader_xlat::needs_external_root_data(b) {
            if let Some(data) = crate::console_slots_mesh::root_data(root as *mut c_void) {
                return crate::shader_xlat::translate_mesh_with_root(b, &data, &link).map_err(|e| e.to_string());
            }
        }
        let k = stage_rtfmt(ty, (n, f));
        let k = if ty == SUB_PS { crate::centroid_bind::tag_rtfmt(root, k) } else { k };
        crate::shader_xlat::translate_for_targets(b.as_ptr(), b.len(), k, &link).map_err(|e| e.to_string())
    };
    let (out, keep) = match translate_stream(stream, &xlat) {
        Ok(v) => v,
        Err(e) => {
            record("d3d12_xs", "CreatePipelineState", &format!("stream rejected: {e} -> E_INVALIDARG"));
            return E_INVALIDARG;
        }
    };
    let vt = *(real as *const *const usize);
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(*vt);
    let mut dev2: *mut c_void = core::ptr::null_mut();
    let hr = qi(real, &IID_DEVICE2, &mut dev2);
    if hr < 0 || dev2.is_null() {
        return hr;
    }
    let vt2 = *(dev2 as *const *const usize);
    let f: unsafe extern "system" fn(*mut c_void, *const [u64; 2], *const c_void, *mut *mut c_void) -> i32 = core::mem::transmute(*vt2.add(PC_DEVICE2_CREATE_PIPELINE_STATE));
    let d = [out.len() as u64, out.as_ptr() as u64];
    let hr = f(dev2, &d, riid, ppv);
    if hr < 0 {
        record("d3d12_xs", "CreatePipelineState", &format!("PC driver rejected stream hr={hr:#010x} subs=[{}]", stream_summary(&out)));
    }
    if hr >= 0 && !(*ppv).is_null() && parse(stream).is_ok_and(|s| s.iter().any(|(t, o)| *t == SUB_AS && bytecode(stream, *o).0 != 0)) {
        crate::console_slots_mesh::note_as_pso(*ppv as usize);
    }
    if hr >= 0 && !(*ppv).is_null() {
        crate::dbcount::note_stream_pso(*ppv as usize, &out, &keep);
    }
    let rel: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt2.add(2));
    rel(dev2);
    drop(keep);
    hr
}

pub fn dispatch_mesh_args_ok(_x: u32, y: u32, z: u32) -> bool {
    y == 1 && z == 1
}

pub unsafe fn dispatch_mesh(list6: *mut c_void, x: u32, y: u32, z: u32) {
    if !dispatch_mesh_args_ok(x, y, z) {
        record("d3d12_xs", "DispatchMesh", &format!("ThreadGroupCountY and Z should be 1 (got {x},{y},{z}): dispatch skipped"));
        return;
    }
    if list6.is_null() {
        return;
    }
    let vt = *(list6 as *const *const usize);
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(*vt);
    let mut l6: *mut c_void = core::ptr::null_mut();
    let hr = qi(list6, &IID_LIST6, &mut l6);
    if hr < 0 || l6.is_null() {
        record("d3d12_xs", "DispatchMesh", &format!("PC command list has no ID3D12GraphicsCommandList6 (hr={hr:#010x}): dispatch not executed"));
        return;
    }
    let vt6 = *(l6 as *const *const usize);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = core::mem::transmute(*vt6.add(PC_LIST6_DISPATCH_MESH));
    f(l6, x, y, z);
    let rel: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt6.add(2));
    rel(l6);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(s: &mut Vec<u8>, ty: u32, body: &[u8]) {
        let (a, n) = inner_layout(ty).unwrap();
        assert_eq!(body.len(), n);
        s.extend_from_slice(&ty.to_le_bytes());
        while s.len() % a != 0 {
            s.push(0);
        }
        s.extend_from_slice(body);
        while s.len() % 8 != 0 {
            s.push(0);
        }
    }

    fn bc(b: &[u8]) -> Vec<u8> {
        let mut v = (b.as_ptr() as u64).to_le_bytes().to_vec();
        v.extend_from_slice(&(b.len() as u64).to_le_bytes());
        v
    }

    #[test]
    fn depth_stencil1_is_56_bytes_with_bounds_last() {
        let mut s = Vec::new();
        let mut ds1 = vec![0u8; 56];
        ds1[52] = 1;
        push(&mut s, 21, &ds1);
        push(&mut s, 17, &[4, 0, 0, 0, 0, 0, 0, 0]);
        let subs = parse(&s).unwrap();
        assert_eq!(subs, vec![(21, 4), (17, 68)]);
        assert_eq!(s[4 + 52], 1);
        assert_eq!(inner_layout(11), Some((4, 52)));
    }

    #[test]
    fn stream_layout_and_translation() {
        let ms = vec![1u8, 2, 3];
        let ps = vec![9u8; 5];
        let mut s = Vec::new();
        push(&mut s, SUB_ROOT_SIGNATURE, &0x1234u64.to_le_bytes());
        push(&mut s, SUB_MS, &bc(&ms));
        push(&mut s, SUB_PS, &bc(&ps));
        let mut rtf = [0u8; 36];
        rtf[0] = 10;
        rtf[32] = 1;
        push(&mut s, 15, &rtf);
        push(&mut s, 17, &[1, 0, 0, 0, 0, 0, 0, 0]);
        let subs = parse(&s).unwrap();
        assert_eq!(subs, vec![(0, 8), (25, 24), (2, 48), (15, 68), (17, 108)]);
        let xl = |ty: u32, b: &[u8], rts: (u32, [u32; 8]), feeds: Option<&[u8]>| {
            assert_eq!(rts, (1, [10, 0, 0, 0, 0, 0, 0, 0]), "subobject {ty}");
            assert_eq!(feeds, if ty == SUB_MS { Some(&ps[..]) } else { None }, "subobject {ty}");
            Ok(Arc::new(b.iter().map(|x| x + 100).collect::<Vec<u8>>()))
        };
        let (out, keep) = translate_stream(&s, &xl).unwrap();
        assert_eq!(keep.len(), 2);
        let (p, n) = bytecode(&out, 24);
        assert_eq!(unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) }, &[101, 102, 103]);
        let (p, n) = bytecode(&out, 48);
        assert_eq!(unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) }, &[109; 5]);
        assert_eq!(&out[..8], &s[..8]);
        assert_eq!(&out[64..], &s[64..]);
    }

    #[test]
    fn stream_rules() {
        let b = vec![1u8; 4];
        let mut s = Vec::new();
        push(&mut s, SUB_VS, &bc(&b));
        push(&mut s, SUB_MS, &bc(&b));
        let subs = parse(&s).unwrap();
        assert_eq!(validate(&s, &subs).unwrap_err(), "When mesh shader is specified, vertex shader must be turned off");
        let mut s = Vec::new();
        push(&mut s, SUB_VS, &[0u8; 16]);
        push(&mut s, SUB_MS, &bc(&b));
        assert!(validate(&s, &parse(&s).unwrap()).is_ok());
        let mut s = Vec::new();
        push(&mut s, SUB_AS, &bc(&b));
        assert!(validate(&s, &parse(&s).unwrap()).is_err());
        let mut s = Vec::new();
        push(&mut s, SUB_MS, &bc(&b));
        push(&mut s, SUB_MS, &bc(&b));
        assert!(parse(&s).unwrap_err().contains("twice"));
        let mut s = 23u32.to_le_bytes().to_vec();
        s.extend_from_slice(&[0u8; 12]);
        assert!(parse(&s).unwrap_err().contains("type 23"));
        let mut s = Vec::new();
        push(&mut s, SUB_MS, &bc(&b));
        s.truncate(20);
        assert!(parse(&s).unwrap_err().contains("past the stream end"));
        let fail = |_: u32, _: &[u8], _: (u32, [u32; 8]), _: Option<&[u8]>| Err::<Arc<Vec<u8>>, String>("translate: x".into());
        let mut s = Vec::new();
        push(&mut s, SUB_MS, &bc(&b));
        assert!(translate_stream(&s, &fail).unwrap_err().contains("subobject 25"));
    }

    #[test]
    fn only_the_pixel_stage_gets_target_formats() {
        let rts = (2u32, [10u32, 26, 0, 0, 0, 0, 0, 0]);
        assert_eq!(stage_rtfmt(SUB_MS, rts), [0; 8]);
        assert_eq!(stage_rtfmt(SUB_AS, rts), [0; 8]);
        assert_eq!(stage_rtfmt(SUB_PS, rts), crate::shader_xlat::export_formats(2, &rts.1));
    }

    #[test]
    fn amplification_needs_force_memory_based_abi_root_signature() {
        let b = vec![1u8; 4];
        let known = |rs: u64| match rs {
            0x10 => Some(0x0100_0001),
            0x20 => Some(0x1),
            _ => None,
        };
        let stream = |rs: u64, with_as: bool| {
            let mut s = Vec::new();
            push(&mut s, SUB_ROOT_SIGNATURE, &rs.to_le_bytes());
            if with_as {
                push(&mut s, SUB_AS, &bc(&b));
            }
            push(&mut s, SUB_MS, &bc(&b));
            s
        };
        let check = |s: &Vec<u8>| as_root_rule(s, &parse(s).unwrap(), &known);
        assert!(check(&stream(0x10, true)).is_ok());
        assert_eq!(check(&stream(0x20, true)).unwrap_err(), AS_ROOT_FLAG_ERR);
        assert!(check(&stream(0x20, false)).is_ok());
        assert!(check(&stream(0x30, true)).unwrap_err().contains("flags are unknown"));
        let rts = |flags: u32| {
            let body: Vec<u8> = [1u32, 0, 24, 0, 24, flags].iter().flat_map(|x| x.to_le_bytes()).collect();
            crate::device_wrap::wrap_root_signature(&body)
        };
        for (flags, ok) in [(0x0100_0000u32, true), (0, false)] {
            let asb = rts(flags);
            let mut s = Vec::new();
            push(&mut s, SUB_AS, &bc(&asb));
            push(&mut s, SUB_MS, &bc(&b));
            assert_eq!(as_root_rule(&s, &parse(&s).unwrap(), &known).is_ok(), ok);
        }
        let mut s = Vec::new();
        push(&mut s, SUB_AS, &bc(&b));
        push(&mut s, SUB_MS, &bc(&b));
        assert!(as_root_rule(&s, &parse(&s).unwrap(), &known).unwrap_err().contains("embeds none"));
    }

    fn fake_mesh_dxbc(psv_topology: u8) -> Vec<u8> {
        let mut info = [0u8; 48];
        info[24] = 13;
        info[27] = psv_topology;
        let chunk_sz = 4u32 + info.len() as u32;
        let total = 36u32 + 8 + chunk_sz;
        let mut b = Vec::with_capacity(total as usize);
        b.extend_from_slice(b"DXBC");
        b.extend_from_slice(&[0u8; 16]);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&total.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&36u32.to_le_bytes());
        b.extend_from_slice(b"PSV0");
        b.extend_from_slice(&chunk_sz.to_le_bytes());
        b.extend_from_slice(&(info.len() as u32).to_le_bytes());
        b.extend_from_slice(&info);
        b
    }

    #[test]
    fn mesh_stream_sets_primitive_topology_from_psv0_and_drops_empty_legacy_stages() {
        let ms_in = vec![1u8, 2, 3];
        let ps = vec![9u8; 5];
        let ms_tri = fake_mesh_dxbc(2);
        let ms_line = fake_mesh_dxbc(1);

        let xl_tri = |ty: u32, b: &[u8], _: (u32, [u32; 8]), _: Option<&[u8]>| {
            Ok(Arc::new(if ty == SUB_MS { ms_tri.clone() } else { b.to_vec() }))
        };
        let mut s_before = Vec::new();
        push(&mut s_before, SUB_PRIMITIVE_TOPOLOGY, &0u32.to_le_bytes());
        push(&mut s_before, SUB_VS, &[0u8; 16]);
        push(&mut s_before, SUB_HS, &[0u8; 16]);
        push(&mut s_before, SUB_DS, &[0u8; 16]);
        push(&mut s_before, SUB_GS, &[0u8; 16]);
        push(&mut s_before, SUB_MS, &bc(&ms_in));
        push(&mut s_before, SUB_PS, &bc(&ps));
        let (out_before, keep_before) = translate_stream(&s_before, &xl_tri).unwrap();
        let subs_before = parse(&out_before).unwrap();
        assert_eq!(subs_before, vec![(SUB_MS, 8), (SUB_PS, 32), (SUB_PRIMITIVE_TOPOLOGY, 52)]);
        assert_eq!(u32::from_le_bytes(out_before[52..56].try_into().unwrap()), 3);
        assert_eq!(keep_before.len(), 2);

        let xl_line = |ty: u32, b: &[u8], _: (u32, [u32; 8]), _: Option<&[u8]>| {
            Ok(Arc::new(if ty == SUB_MS { ms_line.clone() } else { b.to_vec() }))
        };
        let mut s_after = Vec::new();
        push(&mut s_after, SUB_MS, &bc(&ms_in));
        push(&mut s_after, SUB_PRIMITIVE_TOPOLOGY, &0u32.to_le_bytes());
        push(&mut s_after, SUB_PS, &bc(&ps));
        let (out_after, _) = translate_stream(&s_after, &xl_line).unwrap();
        let subs_after = parse(&out_after).unwrap();
        assert_eq!(subs_after, vec![(SUB_MS, 8), (SUB_PRIMITIVE_TOPOLOGY, 28), (SUB_PS, 40)]);
        assert_eq!(u32::from_le_bytes(out_after[28..32].try_into().unwrap()), 2);

        let mut s_omitted = Vec::new();
        push(&mut s_omitted, SUB_MS, &bc(&ms_in));
        push(&mut s_omitted, SUB_PS, &bc(&ps));
        let (out_omitted, _) = translate_stream(&s_omitted, &xl_tri).unwrap();
        let subs_omitted = parse(&out_omitted).unwrap();
        assert_eq!(subs_omitted, vec![(SUB_MS, 8), (SUB_PS, 32), (SUB_PRIMITIVE_TOPOLOGY, 52)]);
        assert_eq!(u32::from_le_bytes(out_omitted[52..56].try_into().unwrap()), 3);

        let xl_raw = |_: u32, b: &[u8], _: (u32, [u32; 8]), _: Option<&[u8]>| Ok(Arc::new(b.to_vec()));
        let mut ms_no_psv = Vec::new();
        push(&mut ms_no_psv, SUB_MS, &bc(&ms_in));
        push(&mut ms_no_psv, SUB_PRIMITIVE_TOPOLOGY, &0u32.to_le_bytes());
        push(&mut ms_no_psv, SUB_PS, &bc(&ps));
        let (out_no_psv, _) = translate_stream(&ms_no_psv, &xl_raw).unwrap();
        assert_eq!(parse(&out_no_psv).unwrap(), vec![(SUB_MS, 8), (SUB_PS, 32)]);

        let mut vs_stream = Vec::new();
        push(&mut vs_stream, SUB_VS, &bc(&ms_in));
        push(&mut vs_stream, SUB_PRIMITIVE_TOPOLOGY, &3u32.to_le_bytes());
        push(&mut vs_stream, SUB_PS, &bc(&ps));
        let (out_vs, keep_vs) = translate_stream(&vs_stream, &xl_raw).unwrap();
        let subs_vs = parse(&out_vs).unwrap();
        assert_eq!(subs_vs, vec![(SUB_VS, 8), (SUB_PRIMITIVE_TOPOLOGY, 28), (SUB_PS, 40)]);
        assert_eq!(u32::from_le_bytes(out_vs[28..32].try_into().unwrap()), 3);
        assert_eq!(keep_vs.len(), 2);

        for (xbox, pc) in [(0x13u32, 3u32), (0x20, 0x20), (2, 2), (4, 4)] {
            let mut s = Vec::new();
            push(&mut s, SUB_VS, &bc(&ms_in));
            push(&mut s, SUB_PRIMITIVE_TOPOLOGY, &xbox.to_le_bytes());
            push(&mut s, SUB_PS, &bc(&ps));
            let (out, _) = translate_stream(&s, &xl_raw).unwrap();
            assert_eq!(u32::from_le_bytes(out[28..32].try_into().unwrap()), pc, "Xbox topology {xbox:#x}");
        }
    }

    fn rec(ty: u32, size: usize, payload_at: usize, payload: &[u8]) -> Vec<u8> {
        let mut r = vec![0u8; size];
        r[0..4].copy_from_slice(&ty.to_le_bytes());
        r[payload_at..payload_at + payload.len()].copy_from_slice(payload);
        r
    }

    fn stage_dxbc(stage: u8) -> Vec<u8> {
        let mut b = fake_mesh_dxbc(2);
        b[72] = stage;
        b
    }

    fn conv(x: &[u8], per_face: bool) -> Result<PcStream, String> {
        xbox_to_pc_stream(x, &|| per_face)
    }

    fn head(a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        push(&mut s, SUB_ROOT_SIGNATURE, &0x10u64.to_le_bytes());
        push(&mut s, SUB_VS, &bc(a));
        push(&mut s, SUB_PS, &bc(b));
        s
    }

    fn ds_desc(stencil: bool, masks: (u8, u8), front: [u32; 4], back: [u32; 4]) -> Vec<u8> {
        let mut d = Vec::new();
        for v in [1u32, 1, 2, stencil as u32] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&[masks.0, masks.1, 0, 0]);
        for v in front.iter().chain(back.iter()) {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d
    }

    fn ds_opts(backface_disable: u32, color: (u32, u32), back_masks: (u8, u8), refs: (u8, u8), ops: (u8, u8)) -> Vec<u8> {
        let mut p = Vec::new();
        for v in [backface_disable, 0, color.0, color.1] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        p.extend_from_slice(&[back_masks.0, back_masks.1, refs.0, refs.1, ops.0, ops.1, 0xab, 0xcd]);
        p
    }

    #[test]
    fn console_record_table_matches_pc_layout_below_23() {
        for ty in 0..=22u32 {
            assert_eq!(console_record_size(ty), pc_record_size(ty), "type {ty}");
        }
        assert_eq!(&CONSOLE_RECORD_SIZE[23..], &[24, 24, 40, 32, 64, 16, 32, 16]);
        assert_eq!(console_record_size(31), None);
        for (ty, sz) in [(24u32, 24usize), (25, 24), (26, 64), (27, 48), (28, 48), (29, 24)] {
            assert_eq!(pc_record_size(ty), Some(sz), "PC type {ty}");
        }
        assert_eq!(pc_record_size(23), None);
        for &(ty, _, off, len) in &CONSOLE_EXT {
            assert!(off + len <= CONSOLE_RECORD_SIZE[ty as usize]);
        }
    }

    #[test]
    fn console_stream_remaps_as_and_ms() {
        let ms = vec![1u8, 2, 3, 4];
        let ps = vec![5u8, 6, 7];
        let mut x = Vec::new();
        push(&mut x, SUB_ROOT_SIGNATURE, &0x1234u64.to_le_bytes());
        x.extend_from_slice(&rec(XBOX_SUB_MS, 24, 8, &bc(&ms)));
        x.extend_from_slice(&rec(XBOX_SUB_AS, 24, 8, &[]));
        push(&mut x, SUB_PS, &bc(&ps));
        let out = conv(&x, false).unwrap();
        assert_eq!(out.numbering, Numbering::Console);
        assert_eq!(out.types, vec![0, 24, 23, 2]);
        let subs = parse(&out.stream).unwrap();
        assert_eq!(subs, vec![(SUB_ROOT_SIGNATURE, 8), (SUB_MS, 24), (SUB_PS, 48)]);
        assert!(validate(&out.stream, &subs).is_ok());

        let mut x_as = Vec::new();
        push(&mut x_as, SUB_ROOT_SIGNATURE, &0x10u64.to_le_bytes());
        x_as.extend_from_slice(&rec(XBOX_SUB_AS, 24, 8, &bc(&ms)));
        x_as.extend_from_slice(&rec(XBOX_SUB_MS, 24, 8, &bc(&ps)));
        let out_as = conv(&x_as, false).unwrap();
        assert_eq!(parse(&out_as.stream).unwrap(), vec![(SUB_ROOT_SIGNATURE, 8), (SUB_AS, 24), (SUB_MS, 48)]);

        let mesh = stage_dxbc(13);
        let mut lone = Vec::new();
        push(&mut lone, SUB_ROOT_SIGNATURE, &0x20u64.to_le_bytes());
        lone.extend_from_slice(&rec(XBOX_SUB_MS, 24, 8, &bc(&mesh)));
        push(&mut lone, SUB_PS, &bc(&ps));
        let out_lone = conv(&lone, false).unwrap();
        assert_eq!(out_lone.numbering, Numbering::Console);
        assert_eq!(parse(&out_lone.stream).unwrap(), vec![(SUB_ROOT_SIGNATURE, 8), (SUB_MS, 24), (SUB_PS, 48)]);
    }

    #[test]
    fn pc_streams_pass_through_untouched_including_agility_26_to_29() {
        let a = vec![1u8, 2, 3, 4];
        let b = vec![5u8, 6, 7];

        let mut pc_as_ms = Vec::new();
        push(&mut pc_as_ms, SUB_ROOT_SIGNATURE, &0x10u64.to_le_bytes());
        push(&mut pc_as_ms, SUB_AS, &bc(&a));
        push(&mut pc_as_ms, SUB_MS, &bc(&b));
        push(&mut pc_as_ms, SUB_PS, &bc(&a));
        let out = conv(&pc_as_ms, false).unwrap();
        assert_eq!((out.numbering, out.stream.clone()), (Numbering::Pc, pc_as_ms.clone()));
        assert!(out.notes.is_empty());

        let mut basic = head(&a, &b);
        push(&mut basic, 17, &[1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(conv(&basic, false).unwrap().stream, basic);

        let mut pc_ms_only = Vec::new();
        push(&mut pc_ms_only, SUB_ROOT_SIGNATURE, &0x10u64.to_le_bytes());
        push(&mut pc_ms_only, SUB_MS, &bc(&b));
        push(&mut pc_ms_only, SUB_PS, &bc(&a));
        assert_eq!(conv(&pc_ms_only, false).unwrap().stream, pc_ms_only);

        let amp = stage_dxbc(14);
        let mut pc_as_only = Vec::new();
        push(&mut pc_as_only, SUB_ROOT_SIGNATURE, &0x10u64.to_le_bytes());
        push(&mut pc_as_only, SUB_AS, &bc(&amp));
        assert_eq!(conv(&pc_as_only, false).unwrap().numbering, Numbering::Pc);

        let mut ds2 = [0u8; 60];
        ds2[0] = 1;
        ds2[12] = 1;
        ds2[32..34].copy_from_slice(&[0xff, 0x0f]);
        ds2[52..54].copy_from_slice(&[0xff, 0xf0]);
        let rast1 = [0u8; 44];
        let rast2 = [0u8; 40];
        let srs = bc(&a);
        for (ty, inner) in [(26u32, &ds2[..]), (27, &rast1[..]), (28, &rast2[..]), (29, &srs[..])] {
            let mut s = head(&a, &b);
            push(&mut s, ty, inner);
            let out = conv(&s, true).unwrap();
            assert_eq!(out.numbering, Numbering::Pc, "PC Agility type {ty}");
            assert_eq!(out.stream, s, "PC Agility type {ty} must pass through untouched");
            assert_eq!(parse(&out.stream).unwrap().last().unwrap().0, ty);
        }
    }

    #[test]
    fn ambiguous_and_malformed_streams_are_refused() {
        let a = vec![1u8, 2, 3, 4];
        let mut lone = Vec::new();
        push(&mut lone, SUB_ROOT_SIGNATURE, &0x10u64.to_le_bytes());
        lone.extend_from_slice(&rec(XBOX_SUB_MS, 24, 8, &bc(&a)));
        push(&mut lone, SUB_PS, &bc(&a));
        assert!(conv(&lone, false).err().unwrap().contains("ambiguous pipeline stream: subobjects [0, 24, 2]"));

        let mut x23_pc25 = rec(XBOX_SUB_AS, 24, 8, &bc(&a));
        push(&mut x23_pc25, SUB_MS, &bc(&a));
        assert!(conv(&x23_pc25, false).err().unwrap().contains("past the stream end"));

        let mut dup = Vec::new();
        push(&mut dup, 11, &[0u8; 52]);
        push(&mut dup, 21, &[0u8; 56]);
        assert!(conv(&dup, false).err().unwrap().contains("twice"));

        let mut unk = 31u32.to_le_bytes().to_vec();
        unk.extend_from_slice(&[0u8; 12]);
        assert!(conv(&unk, false).err().unwrap().contains("type 31 at 0x0 unsupported"));

        let short = rec(26, 20, 4, &[]);
        assert!(conv(&short, false).err().unwrap().contains("past the stream end"));
    }

    #[test]
    fn console_25_and_28_drop_only_when_default() {
        let a = vec![1u8, 2, 3, 4];
        let mut x = head(&a, &a);
        x.extend_from_slice(&rec(25, 40, 4, &[]));
        let out = conv(&x, false).unwrap();
        assert_eq!(out.numbering, Numbering::Console);
        assert_eq!(out.stream, head(&a, &a));
        assert!(out.notes[0].starts_with("console subobject 25 (D3D12XBOX_PIXEL_SHADER_OPTIONS_DESC, 40-byte record"));
        assert!(out.notes[0].ends_with("leaves the extension unset; dropped"));

        let mut x = head(&a, &a);
        x.extend_from_slice(&rec(25, 40, 4, &1u32.to_le_bytes()));
        assert!(conv(&x, false).err().unwrap().contains("PerSampleIteration=0x1"));

        for w1 in [0u32, 0x8000_0000] {
            let mut x = head(&a, &a);
            x.extend_from_slice(&rec(28, 16, 8, &w1.to_le_bytes()));
            let out = conv(&x, false).unwrap();
            assert_eq!(out.stream, head(&a, &a));
            assert!(out.notes[0].contains("D3D12XBOX_PIPELINE_TESSELLATION_PARAMETERS_DESC") && out.notes[0].ends_with("dropped"));
        }
        let mut x = head(&a, &a);
        x.extend_from_slice(&rec(28, 16, 4, &4u32.to_le_bytes()));
        assert!(conv(&x, false).err().unwrap().contains("PatchesPerThreadgroup=0x4"));
    }

    #[test]
    fn console_27_29_30_are_dropped_with_their_reason() {
        let a = vec![1u8, 2, 3, 4];
        let mut x = head(&a, &a);
        x.extend_from_slice(&rec(27, 64, 4, &[0xff, 0xff, 0xff, 0xff, 0x0f, 0, 0, 0]));
        x.extend_from_slice(&rec(29, 32, 4, &[3, 0, 0, 0]));
        x.extend_from_slice(&rec(30, 16, 8, &0x1234_5678u64.to_le_bytes()));
        let out = conv(&x, false).unwrap();
        assert_eq!(out.numbering, Numbering::Console);
        assert_eq!(out.types, vec![0, 1, 2, 27, 29, 30]);
        assert_eq!(out.stream, head(&a, &a));
        assert!(out.notes[0].contains("D3D12XBOX_GRAPHICS_SHADER_LIMITS_DESC") && out.notes[0].contains("scheduling limits"));
        assert!(out.notes[1].contains("D3D12XBOX_COMPUTE_SHADER_LIMITS_DESC") && out.notes[1].contains("only for compute pipelines"));
        assert!(out.notes[2].contains("payload [0x12345678, 0x0]") && out.notes[2].contains("never reads it"));
    }

    #[test]
    fn console_26_depth_stencil_options_map_or_refuse() {
        let a = vec![1u8, 2, 3, 4];
        let build = |ds: &[u8], ty: u32, opts: &[u8]| {
            let mut x = head(&a, &a);
            let mut r = Vec::new();
            push(&mut r, ty, ds);
            x.extend_from_slice(&r);
            x.extend_from_slice(&rec(26, 32, 4, opts));
            x
        };
        let ds_at = 64 + 4;
        let ds = ds_desc(true, (0xff, 0xff), [8, 1, 4, 3], [1, 5, 7, 3]);

        let mut dflt = ds_opts(0, (0, 0), (0, 0), (0, 0), (1, 1));
        dflt[22] = 0x77;
        let out = conv(&build(&ds, 11, &dflt), false).unwrap();
        assert_eq!(out.stream[ds_at..ds_at + 52], ds[..]);
        assert!(out.notes[0].contains("console default"));

        let title = ds_opts(0, (0, 0), (0xff, 0xff), (0, 0), (0, 0));
        let out = conv(&build(&ds, 11, &title), false).unwrap();
        assert_eq!(parse(&out.stream).unwrap()[3], (11, ds_at));
        assert_eq!(out.stream[ds_at..ds_at + 52], ds_desc(true, (0xff, 0xff), [1, 1, 1, 3], [1, 1, 1, 3])[..]);
        assert!(out.notes[0].contains("mapped (STENCILOPVAL=0: INCR/DECR stencil ops add 0, mapped to KEEP)"));

        let off = ds_desc(false, (0xff, 0xff), [8, 1, 4, 3], [1, 5, 7, 3]);
        let out = conv(&build(&off, 11, &title), false).unwrap();
        assert_eq!(out.stream[ds_at..ds_at + 52], off[..]);
        assert!(out.notes[0].contains("no PC-visible change"));

        let unit = ds_opts(0, (0, 0), (0xff, 0xff), (0, 0), (1, 1));
        assert_eq!(conv(&build(&ds, 11, &unit), false).unwrap().stream[ds_at..ds_at + 52], ds[..]);

        let flip = ds_opts(1, (0, 0), (0x0f, 0x0f), (0, 0), (1, 0));
        assert_eq!(conv(&build(&ds, 11, &flip), false).unwrap().stream[ds_at..ds_at + 52], ds_desc(true, (0xff, 0xff), [8, 1, 4, 3], [8, 1, 4, 3])[..]);

        for (opts, why) in [
            (ds_opts(0, (0, 0), (0xff, 0xff), (0, 0), (2, 1)), "STENCILOPVAL 2"),
            (ds_opts(0, (0, 0), (0xff, 0xff), (3, 0), (1, 1)), "StencilTestRefValueFront/Back 0x3/0x0"),
            (ds_opts(0, (1, 0), (0xff, 0xff), (0, 0), (1, 1)), "ENABLE_COLOR_WRITES_ON_DEPTH_FAIL"),
            (ds_opts(0, (0, 0), (0xff, 0xf4), (0, 0), (1, 1)), "lacks IndependentFrontAndBackStencilRefMaskSupported"),
        ] {
            let e = conv(&build(&ds, 11, &opts), false).err().unwrap();
            assert!(e.starts_with("console subobject 26 (DEPTH_STENCIL_OPTIONS, 32-byte record") && e.contains(why), "{e}");
        }

        let mut ds1 = ds.clone();
        ds1.extend_from_slice(&1u32.to_le_bytes());
        let per_face = ds_opts(0, (0, 0), (0xff, 0xf4), (0, 0), (1, 1));
        let out = conv(&build(&ds1, 21, &per_face), true).unwrap();
        let subs = parse(&out.stream).unwrap();
        assert_eq!(subs[3], (26, ds_at));
        let d2 = &out.stream[ds_at..ds_at + 60];
        assert_eq!(d2[0..16], ds[0..16]);
        assert_eq!(d2[16..32], ds[20..36]);
        assert_eq!(d2[32..34], [0xff, 0xff]);
        assert_eq!(d2[36..52], ds[36..52]);
        assert_eq!(d2[52..54], [0xff, 0xf4]);
        assert_eq!(d2[56..60], 1u32.to_le_bytes());
        assert!(out.notes[0].contains("DEPTH_STENCIL2"));
    }

    #[test]
    fn dispatch_mesh_console_rule() {
        assert!(dispatch_mesh_args_ok(7, 1, 1));
        assert!(!dispatch_mesh_args_ok(1, 2, 1));
        assert!(!dispatch_mesh_args_ok(1, 1, 2));
    }
}
