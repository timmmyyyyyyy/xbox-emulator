//! Xbox Series shader container extraction.
//!
//! Fortnite XSX ships each shader as a DXBC container whose portable DXIL bitcode has
//! been stripped (`NOBC`) and replaced, inside the `DXIL` chunk, with an Xbox-native
//! `XDAT` payload. The real shader is zlib-compressed inside `XDAT`; inflating it yields
//! an `FSSM` container whose body is **native RDNA2 microcode** (gfx10xx, Series X|S).
//!
//! Chain:  `DXBC → DXIL chunk → XDAT → zlib → FSSM → RDNA2 ISA`
//!
//! This module parses that chain and returns the raw ISA plus the surrounding metadata.
//! All parsing is bounds-checked and never panics on malformed input (returns `Err`).
//! Reverse-engineered from dumped Fortnite shaders and the GDK UMD `xgs12_10.dll`; see
//! `docs/FORTNITE_ANALYSIS.md`. Field offsets inferred empirically are validated against
//! RDNA2 markers before use, so a wrong guess fails loudly rather than yielding garbage.

pub mod hlsl;
#[cfg(windows)]
pub mod dxc;
pub mod meta;
pub mod fssm_bind;
pub mod dxil_meta;
pub mod dxil_hash;
pub mod decls;
pub mod uearchive;
pub mod rdna2;
pub mod trans16;
pub mod trans32;
pub mod gcn;
pub mod gds_abi;
pub mod global_abi;
pub mod centroid_abi;
pub mod user_data_abi;
pub mod tess_abi;
pub mod runtime_hlsl;
pub mod cbv_raw_hlsl;
#[cfg(windows)]
pub mod shader_cache;
pub mod game_scan;
pub mod pack_key;
pub mod branch_free;
#[cfg(windows)]
#[path = "../../tools/xreplay/src/d3d.rs"]
#[allow(dead_code)]
#[macro_use]
pub(crate) mod d3d;
#[cfg(windows)]
pub mod fma_probe;
#[cfg(test)]
mod sm5_helpers_tests;
#[cfg(test)]
mod xdat_layout_tests;
#[cfg(all(test, windows))]
mod fma16_tests;

#[cfg(windows)]
fn dxil_disassembly(dxbc: &[u8]) -> Option<String> {
    dxc::disassemble(dxbc).ok()
}

#[cfg(not(windows))]
fn dxil_disassembly(_: &[u8]) -> Option<String> {
    None
}

/// Little-endian readers with explicit bounds checks.
fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("u32 read out of bounds at {off:#x} (len {})", b.len()))
}
fn fourcc_at(b: &[u8], off: usize) -> Result<[u8; 4], String> {
    b.get(off..off + 4)
        .map(|s| [s[0], s[1], s[2], s[3]])
        .ok_or_else(|| format!("fourcc read out of bounds at {off:#x}"))
}

/// A DXBC container chunk.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub fourcc: [u8; 4],
    pub offset: usize,
    pub size: usize,
}

/// Parsed DXBC container (the outer wrapper used by both PC and Xbox shaders).
#[derive(Debug, Clone)]
pub struct Dxbc {
    pub digest: [u8; 16],
    pub total_size: u32,
    pub chunks: Vec<Chunk>,
}

impl Dxbc {
    pub fn parse(b: &[u8]) -> Result<Dxbc, String> {
        if fourcc_at(b, 0)? != *b"DXBC" {
            return Err("not a DXBC container (bad magic)".into());
        }
        let mut digest = [0u8; 16];
        digest.copy_from_slice(b.get(4..20).ok_or("truncated digest")?);
        let _version = u32_at(b, 20)?;
        let total_size = u32_at(b, 24)?;
        let nchunks = u32_at(b, 28)? as usize;
        if nchunks > 64 {
            return Err(format!("implausible chunk count {nchunks}"));
        }
        let mut chunks = Vec::with_capacity(nchunks);
        for i in 0..nchunks {
            let off = u32_at(b, 32 + i * 4)? as usize;
            let size = u32_at(b, off + 4)? as usize;
            if off + 8 + size > b.len() {
                return Err(format!("chunk {i} at {off:#x} size {size} exceeds blob"));
            }
            chunks.push(Chunk { fourcc: fourcc_at(b, off)?, offset: off + 8, size });
        }
        Ok(Dxbc { digest, total_size, chunks })
    }

    pub fn chunk(&self, fourcc: &[u8; 4]) -> Option<&Chunk> {
        self.chunks.iter().find(|c| c.fourcc == *fourcc)
    }

    fn output_signature(&self, blob: &[u8]) -> Result<Option<Vec<meta::SigElement>>, String> {
        let mut found = false;
        for tag in [b"OSG1", b"OSG5", b"OSGN"] {
            if let Some(chunk) = self.chunk(tag) {
                found = true;
                let body = &blob[chunk.offset..chunk.offset + chunk.size];
                match meta::parse_signature(tag, body) {
                    Ok(signature) => return Ok(Some(signature)),
                    Err(error) if tag == b"OSG5" => return Err(error),
                    Err(_) => continue,
                }
            }
        }
        Ok(found.then(Vec::new))
    }
}

pub fn parse_root_signature(data: &[u8]) -> Result<meta::RootSig, String> {
    let body = if data.starts_with(b"DXBC") {
        let dxbc = Dxbc::parse(data)?;
        let rts = dxbc.chunk(b"RTS0").ok_or("DXBC has no RTS0 chunk")?;
        &data[rts.offset..rts.offset + rts.size]
    } else {
        data
    };
    let version = u32_at(body, 0)?;
    if !(1..=3).contains(&version) {
        return Err(format!("unsupported root signature version {version}"));
    }
    meta::parse_rts0(body)
}

pub use hlsl::infer::infer_root_signature;
pub use hlsl::translate_with_inference;
pub use hlsl::{translate_with_fssm_root, translate_linked_with_fssm_root};

/// Program type from the DXIL program header (D3D12_SHADER_VERSION_TYPE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Pixel,
    Vertex,
    Geometry,
    Hull,
    Domain,
    Compute,
    Mesh,
    Amplification,
    Unknown(u16),
}
impl Stage {
    fn from_program_version(pv: u32) -> Stage {
        match (pv >> 16) & 0xffff {
            0 => Stage::Pixel,
            1 => Stage::Vertex,
            2 => Stage::Geometry,
            3 => Stage::Hull,
            4 => Stage::Domain,
            5 => Stage::Compute,
            13 => Stage::Mesh,
            14 => Stage::Amplification,
            x => Stage::Unknown(x as u16),
        }
    }
}

/// The fully-extracted shader: native RDNA2 ISA plus the metadata we understand.
#[derive(Debug, Clone)]
pub struct ShaderExtract {
    pub stage: Stage,
    /// Shader model major.minor from the DXIL program header.
    pub shader_model: (u32, u32),
    /// Whether the DXIL bitcode slot was the `NOBC` placeholder (always true for XSX).
    pub bitcode_stripped: bool,
    /// Inflated FSSM container bytes.
    pub fssm: Vec<u8>,
    /// Entry-point name recovered from FSSM, if found.
    pub entry: Option<String>,
    /// Native RDNA2 ISA bytes (the shader program, s_endpgm-terminated).
    pub isa: Vec<u8>,
    /// Offset of the ISA within the FSSM blob.
    pub isa_offset_in_fssm: usize,
    /// The 16-byte ISA-section preamble (compute: dwords 1..3 = thread-group size X,Y,Z).
    pub isa_preamble: [u32; 4],
    pub psv_numthreads: Option<[u32; 3]>,
    /// The program followed by any constant data embedded after it in the code section.
    pub isa_data: Vec<u8>,
    /// Presence of the standard signature/validation/root-sig chunks in the outer DXBC.
    pub has_input_sig: bool,
    pub has_output_sig: bool,
    pub has_psv0: bool,
    pub has_root_sig: bool,
    /// Parsed D3D-side metadata (empty/None when the chunk is absent or malformed).
    pub input_sig: Vec<meta::SigElement>,
    pub output_sig: Vec<meta::SigElement>,
    pub prim_sig: Vec<meta::SigElement>,
    pub resources: Vec<meta::PsvResource>,
    /// Geometry-shader info from PSV0 (only meaningful for `Stage::Geometry`).
    pub gs_info: Option<meta::GsInfo>,
    pub root_sig: Option<meta::RootSig>,
}

/// RDNA2 SOPP opcodes we key on to locate/validate the ISA.
const S_ENDPGM: u32 = 0xBF81_0000; // SOPP op=1
const S_CODE_END: u32 = 0xBF9F_0000; // SOPP op=0x1f, used as code padding
fn is_sopp(w: u32) -> bool {
    (w >> 23) == 0x17f
}

/// Extract the native RDNA2 ISA (and metadata) from a full shader DXBC blob.
pub fn extract(dxbc: &[u8]) -> Result<ShaderExtract, String> {
    let container = Dxbc::parse(dxbc)?;
    let dxil = container
        .chunk(b"DXIL")
        .ok_or("no DXIL chunk (not an Xbox shader?)")?;
    let body = &dxbc[dxil.offset..dxil.offset + dxil.size];

    // DxilProgramHeader: ProgramVersion u32, SizeInUint32 u32, then DxilBitcodeHeader
    // { DxilMagic 'DXIL', DxilVersion, BitcodeOffset, BitcodeSize }. XDAT follows (off 24).
    let program_version = u32_at(body, 0)?;
    let stage = Stage::from_program_version(program_version);
    let sm = ((program_version >> 4) & 0xf, program_version & 0xf);
    let bc_off = u32_at(body, 16)? as usize; // from start of DxilBitcodeHeader (body+8)
    let bc = body.get(8 + bc_off..8 + bc_off + 4).unwrap_or(&[]);
    let bitcode_stripped = bc == b"NOBC";

    let xdat = body.get(24..).ok_or("DXIL chunk too small for XDAT")?;
    if is_bitcode_at(xdat, 0) {
        return Err(bitcode_only_error(dxbc, "DXIL+24 is LLVM DXIL bitcode with no XDAT section"));
    }
    if fourcc_at(xdat, 0)? != *b"XDAT" {
        return Err(format!(
            "expected XDAT at DXIL+24, found {:?}",
            String::from_utf8_lossy(&fourcc_at(xdat, 0)?)
        ));
    }

    // The shader is a zlib stream inside XDAT. The XDAT section table layout is only
    // partially understood, so locate the stream robustly by scanning for the zlib
    // header (0x78 0x9c / 0x78 0xda / 0x78 0x01); inflate self-terminates. Pick the
    // first blob that inflates to an FSSM container.
    //
    // A shader can carry several FSSM programs (observed on vertex shaders: one variant
    // that leaves through `s_setpc_b64` — a hardware-pipeline continuation form using LDS
    // and a barrier — and one standalone program ending in `s_endpgm`). The standalone
    // variant is the one that maps onto a PC pipeline stage, so take the first FSSM whose
    // ISA section contains `s_endpgm`.
    if let Some(blob) = gcn::find_durango(xdat) {
        return gcn::extract_durango(dxbc, &container, stage, sm, bitcode_stripped, &blob);
    }
    let variants = match find_and_inflate_fssm(xdat) {
        Ok(v) => v,
        Err(_) if xdat_bitcode_section(xdat).is_some() => {
            return Err(bitcode_only_error(dxbc, "XDAT holds an LLVM DXIL bitcode section and no compressed GPU program"));
        }
        Err(e) => return Err(e),
    };
    let mut chosen = None;
    let mut last_err = String::from("no FSSM program found");
    for v in variants {
        match locate_isa(&v) {
            Ok((isa, off)) => {
                chosen = Some((v, isa, off));
                break;
            }
            Err(e) => last_err = e,
        }
    }
    let (fssm, isa, isa_off) = chosen.ok_or(last_err)?;
    // Program plus any constant tables embedded after it (up to the s_code_end padding or
    // the end of the ISA section): PC-relative `s_load`s read from here.
    let isa_data = {
        let sect_end = (u32_at(&fssm, 0xc0).unwrap_or(0) as usize + u32_at(&fssm, 0xc8).unwrap_or(0) as usize)
            .min(fssm.len())
            .max(isa_off + isa.len());
        let mut end = isa_off + isa.len();
        while end + 4 <= sect_end && u32_at(&fssm, end).unwrap_or(S_CODE_END) != S_CODE_END {
            end += 4;
        }
        fssm[isa_off..end.min(fssm.len())].to_vec()
    };
    let isa_preamble = {
        let mut p = [0u32; 4];
        if isa_off >= 16 {
            for (i, v) in p.iter_mut().enumerate() {
                *v = u32_at(&fssm, isa_off - 16 + i * 4).unwrap_or(0);
            }
        }
        if stage == Stage::Compute && p[1..].contains(&0) && isa_off >= 24 {
            let q: Vec<u32> = (0..4).map(|i| u32_at(&fssm, isa_off - 24 + i * 4).unwrap_or(0)).collect();
            if q[1..].iter().all(|&x| (1..=1024).contains(&x)) && q[1] * q[2] * q[3] <= 1024 {
                p = [q[0], q[1], q[2], q[3]];
            }
        }
        p
    };
    let chunk_body = |cc: &[u8; 4]| container.chunk(cc).map(|c| &dxbc[c.offset..c.offset + c.size]);
    let entry = find_entry_name(&fssm);

    let psv_numthreads = if stage == Stage::Compute { chunk_body(b"PSV0").and_then(meta::parse_psv0_cs_threads) } else { None };
    let mut gs_info = chunk_body(b"PSV0").and_then(meta::parse_psv0_gs);
    dxil_meta::fill_num_threads(&mut gs_info, || dxil_disassembly(dxbc))?;
    let output_sig = container.output_signature(dxbc)?;
    Ok(ShaderExtract {
        stage,
        shader_model: sm,
        bitcode_stripped,
        entry,
        isa,
        isa_offset_in_fssm: isa_off,
        isa_preamble,
        psv_numthreads,
        isa_data,
        fssm,
        has_input_sig: container.chunk(b"ISG1").is_some() || container.chunk(b"ISGN").is_some(),
        has_output_sig: output_sig.is_some(),
        has_psv0: container.chunk(b"PSV0").is_some(),
        has_root_sig: container.chunk(b"RTS0").is_some(),
        input_sig: chunk_body(b"ISG1").and_then(|b| meta::parse_signature(b"ISG1", b).ok())
            .or_else(|| chunk_body(b"ISGN").and_then(|b| meta::parse_signature(b"ISGN", b).ok()))
            .unwrap_or_default(),
        output_sig: output_sig.unwrap_or_default(),
        prim_sig: chunk_body(b"PSG1").and_then(|b| meta::parse_signature(b"PSG1", b).ok())
            .or_else(|| chunk_body(b"PSGN").and_then(|b| meta::parse_signature(b"PSGN", b).ok()))
            .unwrap_or_default(),
        resources: chunk_body(b"PSV0").and_then(|b| meta::parse_psv0(b).ok()).unwrap_or_default(),
        gs_info,
        root_sig: chunk_body(b"RTS0").and_then(|b| meta::parse_rts0(b).ok()),
    })
}

const XDAT_BITCODE_SECTION: u32 = 7;

fn is_bitcode_at(b: &[u8], off: usize) -> bool {
    b.get(off..off + 4) == Some(b"BC\xC0\xDE")
}

fn xdat_bitcode_section(xdat: &[u8]) -> Option<(usize, usize)> {
    let table = u32_at(xdat, 8).ok()? as usize;
    if table % 12 != 0 || 12 + table > xdat.len() {
        return None;
    }
    (0..table / 12).find_map(|e| {
        let at = 12 + e * 12;
        let (kind, off, size) = (u32_at(xdat, at).ok()?, u32_at(xdat, at + 4).ok()? as usize, u32_at(xdat, at + 8).ok()? as usize);
        (kind == XDAT_BITCODE_SECTION && off.checked_add(size)? <= xdat.len() && is_bitcode_at(xdat, off)).then_some((off, size))
    })
}

fn bitcode_only_error(dxbc: &[u8], layout: &str) -> String {
    let dis = dxil_disassembly(dxbc).unwrap_or_default();
    let triple = dis.lines().find_map(|l| l.trim().strip_prefix("target triple = ")).map(|t| t.trim_matches('"').to_string()).unwrap_or_else(|| "unknown".into());
    let jit = if dis.contains("__XBOX_DISABLE_PRECOMPILE") { ", built with __XBOX_DISABLE_PRECOMPILE" } else { "" };
    format!("{layout} (runtime-JIT DXIL, triple {triple}{jit}): the console compiles it when the PSO is created; TomBox has no DXIL passthrough yet")
}

/// Scan a blob for zlib streams and return every one that inflates to an `FSSM` container
/// (in file order).
fn find_and_inflate_fssm(xdat: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut last_err = String::from("no zlib stream found in XDAT");
    let mut found = Vec::new();
    let mut i = 8; // past "XDAT" + version
    while i + 2 < xdat.len() {
        if xdat[i] == 0x78 && matches!(xdat[i + 1], 0x01 | 0x9c | 0xda) {
            match miniz_oxide::inflate::decompress_to_vec_zlib(&xdat[i..]) {
                Ok(out) if out.len() >= 8 && &out[0..4] == b"FSSM" => found.push(out),
                Ok(out) => last_err = format!("inflated {} bytes but magic={:?}", out.len(), &out[0..4.min(out.len())]),
                Err(e) => last_err = format!("inflate at {i:#x} failed: {e:?}"),
            }
        }
        i += 1;
    }
    if found.is_empty() { Err(last_err) } else { Ok(found) }
}

/// Locate the RDNA2 ISA section inside an FSSM blob.
///
/// The FSSM header holds a section descriptor whose 64-bit offset/size (observed at
/// +0xc0 / +0xc8) brackets the ISA. We read it, then VALIDATE that the range both
/// contains `s_endpgm` and is dword-aligned RDNA2; if that fails we fall back to a
/// marker-based scan (first SOPP-dense run ending at `s_endpgm`). Either way the
/// returned bytes are checked to contain `s_endpgm`, so we never return non-ISA.
fn locate_isa(fssm: &[u8]) -> Result<(Vec<u8>, usize), String> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return Err("not an FSSM container".into());
    }
    // Candidate from the header descriptor (empirical offsets).
    if let (Ok(off64), Ok(size_lo)) = (u32_at(fssm, 0xc0), u32_at(fssm, 0xc8)) {
        let off = off64 as usize;
        let size = size_lo as usize;
        if off >= 0x40 && size >= 8 && off + size <= fssm.len() {
            let sect = &fssm[off..off + size];
            if range_contains_endpgm(sect) {
                // The ISA section begins with a 16-byte preamble (dword0 = code offset,
                // dwords 1..3 = 0) before the executable RDNA2 code. Skip it when present.
                let (code, code_off) = strip_isa_preamble(sect, off);
                return Ok((trim_to_endpgm(code), code_off));
            }
        }
    }
    // Fallback: find the dword-aligned s_endpgm, then walk back over SOPP/other code.
    let mut end = None;
    let mut p = 0;
    while p + 4 <= fssm.len() {
        if u32::from_le_bytes([fssm[p], fssm[p + 1], fssm[p + 2], fssm[p + 3]]) == S_ENDPGM {
            end = Some(p + 4);
            break;
        }
        p += 4;
    }
    let end = end.ok_or("no s_endpgm found in FSSM — not RDNA2?")?;
    // Walk backward while the words look like plausible code (nonzero, or SOPP).
    let mut start = end - 4;
    while start >= 4 {
        let w = u32::from_le_bytes([fssm[start - 4], fssm[start - 3], fssm[start - 2], fssm[start - 1]]);
        if w == 0 && !is_sopp(w) {
            break;
        }
        start -= 4;
    }
    Ok((fssm[start..end].to_vec(), start))
}

/// Strip the 16-byte ISA-section preamble (dword0 = code offset, dwords 1..3 zero) that
/// precedes the executable RDNA2 code. Returns the code slice and its absolute FSSM offset.
fn strip_isa_preamble(sect: &[u8], sect_off: usize) -> (&[u8], usize) {
    if sect.len() >= 24 {
        let w = |o: usize| u32::from_le_bytes([sect[o], sect[o + 1], sect[o + 2], sect[o + 3]]);
        // Graphics: dwords 1..3 are zero. Compute: dwords 1..3 are the thread-group size.
        // In both cases the code proper starts with the driver hash marker, a VOP3
        // `v_nop` (0xD580....) whose second dword is 0x03FE..../0x03FF.....
        let zeros = w(4) == 0 && w(8) == 0 && w(12) == 0;
        let marker = (w(16) >> 16) == 0xd580 && (w(20) >> 17) == 0x1ff;
        if zeros || marker {
            return (&sect[16..], sect_off + 16);
        }
    }
    (sect, sect_off)
}

fn range_contains_endpgm(sect: &[u8]) -> bool {
    let mut p = 0;
    while p + 4 <= sect.len() {
        let w = u32::from_le_bytes([sect[p], sect[p + 1], sect[p + 2], sect[p + 3]]);
        if w == S_ENDPGM {
            return true;
        }
        p += 4;
    }
    false
}

/// Trim an ISA section to end just after the first `s_endpgm`, dropping `s_code_end` pad.
fn trim_to_endpgm(sect: &[u8]) -> Vec<u8> {
    // A program may contain early `s_endpgm`s (e.g. a pixel-kill path) with more code
    // after them, so the program ends at the LAST `s_endpgm` before the `s_code_end`
    // padding, not the first one.
    //
    // Preferred rule: decode instruction by instruction, tracking the furthest forward
    // branch target; the program ends at the first `s_endpgm` at or beyond it (data that
    // merely looks like `s_endpgm` can follow the real end). Fallback: last raw match.
    let code_end = {
        let mut p = 0;
        while p + 4 <= sect.len() {
            if u32::from_le_bytes([sect[p], sect[p + 1], sect[p + 2], sect[p + 3]]) == S_CODE_END {
                break;
            }
            p += 4;
        }
        p
    };
    let mut max_target = 0i64;
    for ins in rdna2::disassemble(&sect[..code_end]) {
        if ins.enc != rdna2::Enc::Sopp {
            continue;
        }
        if matches!(ins.op, 2 | 4..=9) {
            max_target = max_target.max(ins.offset as i64 + 4 + ins.aux.imm * 4);
        } else if ins.op == 1 && ins.offset as i64 >= max_target {
            return sect[..ins.offset + 4].to_vec();
        }
    }
    let mut last_end = None;
    let mut p = 0;
    while p + 4 <= sect.len() {
        let w = u32::from_le_bytes([sect[p], sect[p + 1], sect[p + 2], sect[p + 3]]);
        if w == S_CODE_END {
            break;
        }
        if w == S_ENDPGM {
            last_end = Some(p + 4);
        }
        p += 4;
    }
    match last_end {
        Some(e) => sect[..e].to_vec(),
        None => sect.to_vec(),
    }
}

/// Find the entry-point name (observed as an ASCII run near the FSSM tail, e.g. "Main").
fn find_entry_name(fssm: &[u8]) -> Option<String> {
    // Scan for a short printable ASCII run that looks like an identifier.
    let mut best: Option<String> = None;
    let mut cur = Vec::new();
    for &b in fssm {
        if b.is_ascii_alphanumeric() || b == b'_' {
            cur.push(b);
        } else {
            if cur.len() >= 3 && cur.len() <= 64 && cur[0].is_ascii_alphabetic() {
                let s = String::from_utf8_lossy(&cur).into_owned();
                // Prefer a name that isn't obviously a chunk fourcc.
                if !matches!(s.as_str(), "FSSM" | "XDAT" | "DXBC" | "DXIL" | "NOBC") {
                    best = Some(s);
                }
            }
            cur.clear();
        }
    }
    best
}
