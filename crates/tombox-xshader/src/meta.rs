//! Parsers for the standard (PC-format) metadata chunks that survive in XSX shader
//! containers: ISG1/OSG1 signatures, PSV0 resource bindings and the RTS0 root signature.
//! These give the HLSL emitter the D3D-side view (semantics, registers, root parameters).

fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("meta: u32 out of bounds at {off:#x} (len {})", b.len()))
}

fn cstr_at(b: &[u8], off: usize) -> String {
    let tail = b.get(off..).unwrap_or(&[]);
    let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

/// One element of an ISG1/OSG1 signature.
#[derive(Debug, Clone)]
pub struct SigElement {
    pub name: String,
    pub index: u32,
    /// D3D_NAME system value (0 = none, 1 = SV_Position, 64 = SV_Target, ...).
    pub sysval: u32,
    /// D3D_REGISTER_COMPONENT_TYPE: 1 = uint, 2 = int, 3 = float.
    pub comp_type: u32,
    pub register: u32,
    pub mask: u8,
    pub rw_mask: u8,
}

pub fn parse_signature(tag: &[u8; 4], body: &[u8]) -> Result<Vec<SigElement>, String> {
    let count = u32_at(body, 0)? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 64 {
        return Err(format!("implausible signature element count {count}"));
    }
    let (stride, prefix) = match tag {
        b"ISGN" | b"OSGN" | b"PCSG" | b"PSGN" => (24, 0),
        b"OSG5" => (28, 4),
        _ => (32, 4),
    };
    if 8 + count * stride > body.len() {
        return Err(format!("signature body truncated for {count} elements"));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let record = 8 + i * stride;
        if tag == b"OSG5" && u32_at(body, record)? != 0 {
            return Err("nonzero geometry output streams in OSG5 are unsupported".into());
        }
        let e = record + prefix;
        let name_off = u32_at(body, e)? as usize;
        out.push(SigElement {
            name: cstr_at(body, name_off),
            index: u32_at(body, e + 4)?,
            sysval: u32_at(body, e + 8)?,
            comp_type: u32_at(body, e + 12)?,
            register: u32_at(body, e + 16)?,
            mask: body[e + 20],
            rw_mask: body[e + 21],
        });
    }
    Ok(out)
}

/// A PSV0 resource binding.
#[derive(Debug, Clone)]
pub struct PsvResource {
    /// 1 Sampler, 2 CBV, 3 SRVTyped, 4 SRVRaw, 5 SRVStructured, 6 UAVTyped, 7 UAVRaw, 8 UAVStructured...
    pub res_type: u32,
    pub space: u32,
    pub lower: u32,
    pub upper: u32,
    pub kind: u32,
}

/// Parse the PSV0 chunk far enough to recover the resource-binding list.
/// Geometry-shader facts from the PSV0 runtime info.
#[derive(Debug, Clone, Copy)]
pub struct GsInfo {
    /// D3D input primitive: 1 point, 2 line, 3 triangle, 6 line-adj, 7 triangle-adj.
    pub input_primitive: u32,
    /// D3D output topology: 1 point list, 3 line strip, 5 triangle strip.
    pub output_topology: u32,
    pub max_vertex_count: u16,
    pub mesh: Option<MeshInfo>,
    pub tess: Option<TessInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TessInfo {
    pub stage: u8,
    pub input_control_points: u32,
    pub output_control_points: u32,
    pub domain: u32,
    pub output_primitive: u32,
    pub output_position: bool,
    pub patch_const_vectors: u8,
}

pub fn parse_psv0_tess(info: &[u8]) -> Option<TessInfo> {
    let stage = *info.get(24)?;
    let input_control_points = u32_at(info, 0).ok()?;
    let patch_const_vectors = *info.get(26)?;
    let (output_control_points, domain, output_primitive, output_position) = match stage {
        3 => (u32_at(info, 4).ok()?, u32_at(info, 8).ok()?, u32_at(info, 12).ok()?, false),
        4 => (0, u32_at(info, 8).ok()?, 0, *info.get(4)? != 0),
        _ => return None,
    };
    let control_points_ok = |n: u32| (1..=32).contains(&n);
    let valid = control_points_ok(input_control_points)
        && (1..=3).contains(&domain)
        && (stage == 4 || (control_points_ok(output_control_points) && (1..=4).contains(&output_primitive)));
    valid.then_some(TessInfo {
        stage,
        input_control_points,
        output_control_points,
        domain,
        output_primitive,
        output_position,
        patch_const_vectors,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshInfo {
    pub stage: u8,
    pub groupshared_bytes: u32,
    pub payload_bytes: u32,
    pub max_vertices: u16,
    pub max_primitives: u16,
    pub prim_vectors: u8,
    pub topology: u8,
    pub num_threads: [u32; 3],
}

pub(crate) fn mesh_thread_count(dims: [u32; 3]) -> Result<u32, String> {
    mesh_thread_count_for(14, dims)
}

pub(crate) fn mesh_thread_count_for(stage: u8, dims: [u32; 3]) -> Result<u32, String> {
    let count = dims.into_iter().try_fold(1u32, u32::checked_mul);
    match count {
        Some(n @ 1..=128) => Ok(n),
        Some(n @ 129..=256) if stage == 13 && n % 2 == 0 => Ok(n),
        _ => Err(format!("mesh/amplification thread-group size {dims:?} outside 1..128")),
    }
}

pub fn parse_psv0_mesh(info: &[u8]) -> Option<MeshInfo> {
    let stage = *info.get(24)?;
    let num_threads = if info.len() >= 48 { [u32_at(info, 36).ok()?, u32_at(info, 40).ok()?, u32_at(info, 44).ok()?] } else { [0; 3] };
    match stage {
        13 => Some(MeshInfo {
            stage,
            groupshared_bytes: u32_at(info, 0).ok()?,
            payload_bytes: u32_at(info, 8).ok()?,
            max_vertices: u16::from_le_bytes([*info.get(12)?, *info.get(13)?]),
            max_primitives: u16::from_le_bytes([*info.get(14)?, *info.get(15)?]),
            prim_vectors: *info.get(26)?,
            topology: *info.get(27)?,
            num_threads,
        }),
        14 => Some(MeshInfo {
            stage,
            groupshared_bytes: 0,
            payload_bytes: u32_at(info, 0).ok()?,
            max_vertices: 0,
            max_primitives: 0,
            prim_vectors: 0,
            topology: 0,
            num_threads,
        }),
        _ => None,
    }
}

pub fn parse_psv0_cs_threads(body: &[u8]) -> Option<[u32; 3]> {
    let info_size = u32_at(body, 0).ok()? as usize;
    if info_size < 48 {
        return None;
    }
    let info = body.get(4..4 + info_size)?;
    let t = [u32_at(info, 36).ok()?, u32_at(info, 40).ok()?, u32_at(info, 44).ok()?];
    let prod = (t[0] as u64).checked_mul(t[1] as u64)?.checked_mul(t[2] as u64)?;
    if t.iter().any(|d| !(1..=1024).contains(d)) || prod > 1024 {
        return None;
    }
    Some(t)
}

/// Parse the GS part of PSV0's PSVRuntimeInfo: the stage union starts at +4
/// ({InputPrimitive u32, OutputTopology u32, OutputStreamMask u32, ...}); PSVRuntimeInfo1
/// adds ShaderStage u8 @+24, UsesViewID u8 @+25 and MaxVertexCount u16 @+26.
pub fn parse_psv0_gs(body: &[u8]) -> Option<GsInfo> {
    let info_size = u32_at(body, 0).ok()? as usize;
    if info_size < 28 {
        return None;
    }
    let info = body.get(4..4 + info_size)?;
    Some(GsInfo {
        input_primitive: u32_at(info, 0).ok()?,
        output_topology: u32_at(info, 4).ok()?,
        max_vertex_count: u16::from_le_bytes([*info.get(26)?, *info.get(27)?]),
        mesh: parse_psv0_mesh(info),
        tess: parse_psv0_tess(info),
    })
}

pub fn parse_psv0(body: &[u8]) -> Result<Vec<PsvResource>, String> {
    let info_size = u32_at(body, 0)? as usize;
    let mut p = 4 + info_size;
    let count = u32_at(body, p)? as usize;
    p += 4;
    if count == 0 {
        return Ok(vec![]);
    }
    if count > 256 {
        return Err(format!("implausible PSV0 resource count {count}"));
    }
    let bind_size = u32_at(body, p)? as usize;
    p += 4;
    if bind_size < 16 {
        return Err(format!("PSV0 bind size {bind_size} too small"));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = p + i * bind_size;
        out.push(PsvResource {
            res_type: u32_at(body, e)?,
            space: u32_at(body, e + 4)?,
            lower: u32_at(body, e + 8)?,
            upper: u32_at(body, e + 12)?,
            kind: if bind_size >= 24 { u32_at(body, e + 16)? } else { 0 },
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct RootRange {
    /// 0 SRV, 1 UAV, 2 CBV, 3 Sampler.
    pub range_type: u32,
    pub num: u32,
    pub base_register: u32,
    pub space: u32,
    pub offset_in_table: u32,
}

#[derive(Debug, Clone)]
pub enum RootParamKind {
    Table(Vec<RootRange>),
    Constants { register: u32, space: u32, num32: u32 },
    /// 2 = CBV, 3 = SRV, 4 = UAV root descriptor.
    Descriptor { kind: u32, register: u32, space: u32 },
}

#[derive(Debug, Clone)]
pub struct RootParam {
    pub visibility: u32,
    pub kind: RootParamKind,
}

#[derive(Debug, Clone)]
pub struct RootSig {
    pub version: u32,
    pub flags: u32,
    pub params: Vec<RootParam>,
    pub num_static_samplers: u32,
    pub static_samplers: Vec<StaticSampler>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticSampler {
    pub filter: u32,
    pub address_v: u32,
    pub address_w: u32,
    pub max_anisotropy: u32,
    pub comparison_func: u32,
    pub border_color: u32,
    pub min_lod_bits: u32,
    pub max_lod_bits: u32,
    pub address_u: u32,
    pub register: u32,
    pub space: u32,
    pub visibility: u32,
}

/// Parse an RTS0 (serialized root signature) chunk body.
pub fn parse_rts0(body: &[u8]) -> Result<RootSig, String> {
    let version = u32_at(body, 0)?;
    let nparams = u32_at(body, 4)? as usize;
    let params_off = u32_at(body, 8)? as usize;
    let num_static_samplers = u32_at(body, 12)?;
    let flags = u32_at(body, 20)?;
    if nparams > 64 {
        return Err(format!("implausible root parameter count {nparams}"));
    }
    let v11 = version >= 2;
    let mut params = Vec::with_capacity(nparams);
    for i in 0..nparams {
        let e = params_off + i * 12;
        let ptype = u32_at(body, e)?;
        let visibility = u32_at(body, e + 4)?;
        let off = u32_at(body, e + 8)? as usize;
        let kind = match ptype {
            0 => {
                let nranges = u32_at(body, off)? as usize;
                let roff = u32_at(body, off + 4)? as usize;
                let stride = if v11 { 24 } else { 20 };
                let available = body.get(roff..).map_or(0, |remaining| remaining.len() / stride);
                if nranges > available {
                    return Err(format!("root table has {nranges} ranges but only {available} fit in RTS0"));
                }
                let mut ranges = Vec::with_capacity(nranges);
                for r in 0..nranges {
                    let q = roff + r * stride;
                    ranges.push(RootRange {
                        range_type: u32_at(body, q)?,
                        num: u32_at(body, q + 4)?,
                        base_register: u32_at(body, q + 8)?,
                        space: u32_at(body, q + 12)?,
                        offset_in_table: u32_at(body, q + if v11 { 20 } else { 16 })?,
                    });
                }
                RootParamKind::Table(ranges)
            }
            1 => RootParamKind::Constants {
                register: u32_at(body, off)?,
                space: u32_at(body, off + 4)?,
                num32: u32_at(body, off + 8)?,
            },
            2..=4 => RootParamKind::Descriptor {
                kind: ptype,
                register: u32_at(body, off)?,
                space: u32_at(body, off + 4)?,
            },
            other => return Err(format!("unknown root parameter type {other}")),
        };
        params.push(RootParam { visibility, kind });
    }
    let mut static_samplers = Vec::new();
    if num_static_samplers <= 64 {
        if let Ok(so) = u32_at(body, 16) {
            let stride = if version >= 3 { 56 } else { 52 };
            for i in 0..num_static_samplers as usize {
                let q = so as usize + i * stride;
                if let (Ok(address_u), Ok(register), Ok(space), Ok(visibility)) = (u32_at(body, q + 4), u32_at(body, q + 40), u32_at(body, q + 44), u32_at(body, q + 48)) {
                    let at = |o: usize| u32_at(body, q + o).unwrap_or(0);
                    static_samplers.push(StaticSampler {
                        filter: at(0),
                        address_v: at(8),
                        address_w: at(12),
                        max_anisotropy: at(20),
                        comparison_func: at(24),
                        border_color: at(28),
                        min_lod_bits: at(32),
                        max_lod_bits: at(36),
                        address_u,
                        register,
                        space,
                        visibility,
                    });
                }
            }
        }
    }
    Ok(RootSig { version, flags, params, num_static_samplers, static_samplers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_24byte_isgn_and_32byte_isg1_signatures() {
        let mut isgn = Vec::new();
        isgn.extend_from_slice(&1u32.to_le_bytes());
        isgn.extend_from_slice(&8u32.to_le_bytes());
        let name_offset_24 = 8 + 24;
        isgn.extend_from_slice(&(name_offset_24 as u32).to_le_bytes());
        isgn.extend_from_slice(&0u32.to_le_bytes());
        isgn.extend_from_slice(&0u32.to_le_bytes());
        isgn.extend_from_slice(&3u32.to_le_bytes());
        isgn.extend_from_slice(&0u32.to_le_bytes());
        isgn.push(0x0f);
        isgn.push(0x0f);
        isgn.extend_from_slice(&[0, 0]);
        isgn.extend_from_slice(b"POSITION\0");

        let sig_24 = parse_signature(b"ISGN", &isgn).expect("parse ISGN");
        assert_eq!(sig_24.len(), 1);
        assert_eq!(sig_24[0].name, "POSITION");
        assert_eq!(sig_24[0].comp_type, 3);
        assert_eq!(sig_24[0].register, 0);
        assert_eq!(sig_24[0].mask, 0x0f);
        assert_eq!(sig_24[0].rw_mask, 0x0f);

        let mut isg1 = Vec::new();
        isg1.extend_from_slice(&1u32.to_le_bytes());
        isg1.extend_from_slice(&8u32.to_le_bytes());
        let name_offset_32 = 8 + 32;
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.extend_from_slice(&(name_offset_32 as u32).to_le_bytes());
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.extend_from_slice(&3u32.to_le_bytes());
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.push(0x0f);
        isg1.push(0x0f);
        isg1.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        isg1.extend_from_slice(b"POSITION\0");

        let sig_32 = parse_signature(b"ISG1", &isg1).expect("parse ISG1");
        assert_eq!(sig_32.len(), 1);
        assert_eq!(sig_32[0].name, "POSITION");
        assert_eq!(sig_32[0].comp_type, 3);
        assert_eq!(sig_32[0].register, 0);
        assert_eq!(sig_32[0].mask, 0x0f);
        assert_eq!(sig_32[0].rw_mask, 0x0f);
    }

    #[test]
    fn stream_signature_keeps_the_second_outputs_semantic_and_masks() {
        let mut body = Vec::new();
        for word in [2u32, 8, 0, 64, 0, 1, 3, 0, 0x000f, 0, 76, 3, 0, 3, 1, 0x0c03] {
            body.extend(word.to_le_bytes());
        }
        body.extend(b"SV_Position\0TEXCOORD\0");
        let container = crate::Dxbc {
            digest: [0; 16],
            total_size: body.len() as u32,
            chunks: vec![crate::Chunk { fourcc: *b"OSG5", offset: 0, size: body.len() }],
        };
        let sig = container.output_signature(&body).unwrap().unwrap();
        assert_eq!(sig.len(), 2);
        assert_eq!((sig[0].name.as_str(), sig[0].sysval, sig[0].mask), ("SV_Position", 1, 15));
        let uv = &sig[1];
        assert_eq!((uv.name.as_str(), uv.index, uv.register, uv.comp_type, uv.mask, uv.rw_mask), ("TEXCOORD", 3, 1, 3, 3, 12));
        body[36..40].copy_from_slice(&1u32.to_le_bytes());
        assert!(container.output_signature(&body).unwrap_err().contains("nonzero geometry output streams"));
        assert!(parse_signature(b"OSG5", &body[..60]).is_err());
    }

    fn cs_blob(info_size: u32, dims: [u32; 3]) -> Vec<u8> {
        let mut b = vec![0u8; 4 + info_size as usize];
        b[..4].copy_from_slice(&info_size.to_le_bytes());
        if (info_size as usize) >= 48 {
            for (i, d) in dims.iter().enumerate() {
                let o = 4 + 36 + i * 4;
                b[o..o + 4].copy_from_slice(&d.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn cs_thread_counts_reject_a_bad_group_size() {
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [8, 8, 1])), Some([8, 8, 1]));
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [32, 1, 1])), Some([32, 1, 1]));
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [32, 32, 1])), Some([32, 32, 1]));
        assert_eq!(parse_psv0_cs_threads(&cs_blob(36, [8, 8, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(12, [8, 8, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [0, 8, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [1025, 1, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [32, 32, 2])), None);
        assert_eq!(parse_psv0_cs_threads(&48u32.to_le_bytes()), None);
        let mut union_only = cs_blob(48, [4, 2, 1]);
        union_only[4..16].copy_from_slice(&[8, 0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(parse_psv0_cs_threads(&union_only), Some([4, 2, 1]));
    }

    fn tess_info_bytes(stage: u8, union: [u32; 4], patch_const_vectors: u8) -> Vec<u8> {
        let mut info = vec![0u8; 36];
        for (k, v) in union.iter().enumerate() {
            info[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        info[24] = stage;
        info[26] = patch_const_vectors;
        info
    }

    #[test]
    fn psv0_hull_runtime_info_gives_control_points_domain_and_output_primitive() {
        let info = tess_info_bytes(3, [3, 4, 2, 3], 2);
        assert_eq!(
            parse_psv0_tess(&info),
            Some(TessInfo {
                stage: 3,
                input_control_points: 3,
                output_control_points: 4,
                domain: 2,
                output_primitive: 3,
                output_position: false,
                patch_const_vectors: 2,
            })
        );
    }

    #[test]
    fn psv0_domain_runtime_info_reads_the_position_byte_and_the_aligned_domain() {
        let info = tess_info_bytes(4, [16, 1, 3, 0xdead_beef], 1);
        assert_eq!(
            parse_psv0_tess(&info),
            Some(TessInfo {
                stage: 4,
                input_control_points: 16,
                output_control_points: 0,
                domain: 3,
                output_primitive: 0,
                output_position: true,
                patch_const_vectors: 1,
            })
        );
        let mut no_position = tess_info_bytes(4, [16, 0, 3, 0], 1);
        no_position[5] = 0xff;
        assert_eq!(parse_psv0_tess(&no_position).map(|t| t.output_position), Some(false));
    }

    #[test]
    fn psv0_tess_info_refuses_other_stages_and_out_of_range_fields() {
        for stage in [0u8, 1, 2, 5, 13, 14] {
            assert_eq!(parse_psv0_tess(&tess_info_bytes(stage, [3, 3, 2, 3], 0)), None, "stage {stage}");
        }
        for union in [[0, 3, 2, 3], [33, 3, 2, 3], [3, 0, 2, 3], [3, 33, 2, 3], [3, 3, 0, 3], [3, 3, 4, 3], [3, 3, 2, 0], [3, 3, 2, 5]] {
            assert_eq!(parse_psv0_tess(&tess_info_bytes(3, union, 0)), None, "hull {union:?}");
        }
        for union in [[0, 1, 2, 0], [33, 1, 2, 0], [3, 1, 0, 0], [3, 1, 4, 0]] {
            assert_eq!(parse_psv0_tess(&tess_info_bytes(4, union, 0)), None, "domain {union:?}");
        }
        assert_eq!(parse_psv0_tess(&tess_info_bytes(3, [3, 3, 2, 3], 0)[..26]), None);
    }

    #[test]
    fn psv0_gs_info_carries_the_tess_info_of_a_hull_shader() {
        let info = tess_info_bytes(3, [32, 32, 3, 4], 0);
        let mut body = (info.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(&info);
        let gs = parse_psv0_gs(&body).expect("PSV0 runtime info");
        assert_eq!(gs.mesh, None);
        assert_eq!(gs.tess.map(|t| (t.input_control_points, t.output_control_points, t.domain, t.output_primitive)), Some((32, 32, 3, 4)));
        let vertex = tess_info_bytes(1, [0; 4], 0);
        let mut body = (vertex.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(&vertex);
        assert_eq!(parse_psv0_gs(&body).and_then(|g| g.tess), None);
    }
}

#[cfg(test)]
mod wide_table_tests {
    use super::*;

    #[test]
    fn reads_large_root_tables_and_rejects_truncated_ranges() {
        for (version, stride) in [(1, 20), (2, 24)] {
            let count = 261usize;
            let mut body = vec![0; 44 + count * stride];
            for (off, value) in [(0, version), (4, 1), (8, 24), (32, 36), (36, count as u32), (40, 44)] {
                body[off..off + 4].copy_from_slice(&value.to_le_bytes());
            }
            for register in 0..count {
                let off = 44 + register * stride + 8;
                body[off..off + 4].copy_from_slice(&(register as u32).to_le_bytes());
            }
            let root = parse_rts0(&body).unwrap();
            let RootParamKind::Table(ranges) = &root.params[0].kind else { panic!("expected table") };
            assert_eq!(ranges.len(), count);
            assert_eq!(ranges[count - 1].base_register, 260);
            body.pop();
            assert!(parse_rts0(&body).is_err());
        }
    }
}
