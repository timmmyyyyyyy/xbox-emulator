use super::{gds, r, Emitter};
use crate::rdna2::{Enc, Insn, Opnd};
use crate::ShaderExtract;

const FSSM_SUBVERSION_AT: usize = 0x10;
const RSRC2_BY_SUBVERSION: [(u32, usize); 2] = [(5, 0x130), (6, 0x138)];
const RSRC2_MUST_BE_ZERO: u32 = 0xff00_6040;
const LDS_GRANULE_DWORDS: u32 = 128;
const GROUPSHARED_BYTES: u32 = 32768;

pub(super) fn is_atomic64(m: &str) -> bool {
    matches!(
        m.replace("_rtn", "").as_str(),
        "ds_add_u64"
            | "ds_sub_u64"
            | "ds_rsub_u64"
            | "ds_inc_u64"
            | "ds_dec_u64"
            | "ds_min_i64"
            | "ds_max_i64"
            | "ds_min_u64"
            | "ds_max_u64"
            | "ds_and_b64"
            | "ds_or_b64"
            | "ds_xor_b64"
            | "ds_mskor_b64"
            | "ds_cmpst_b64"
            | "ds_wrxchg_b64"
            | "ds_wrxchg2_b64"
            | "ds_wrxchg2st64_b64"
    )
}

fn le32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn rsrc2_location(ex: &ShaderExtract) -> Result<usize, String> {
    if crate::gcn::is_durango(&ex.fssm) {
        return Err("64-bit LDS atomic: COMPUTE_PGM_RSRC2 not found in the shader container (Durango container, no proven location), so the LDS size is unknown and the lock word cannot be placed".into());
    }
    let v = le32(&ex.fssm, FSSM_SUBVERSION_AT)
        .ok_or("64-bit LDS atomic: COMPUTE_PGM_RSRC2 not found in the shader container (no FSSM sub-version), so the LDS size is unknown and the lock word cannot be placed")?;
    let at = RSRC2_BY_SUBVERSION
        .iter()
        .find(|e| e.0 == v)
        .map(|e| e.1)
        .ok_or_else(|| format!("64-bit LDS atomic: COMPUTE_PGM_RSRC2 not found in the shader container (FSSM sub-version {v:#x} has no known location), so the LDS size is unknown and the lock word cannot be placed"))?;
    if ex.isa_offset_in_fssm.checked_sub(16).is_none_or(|end| at + 4 > end) {
        return Err(format!("64-bit LDS atomic: COMPUTE_PGM_RSRC2 not found in the shader container (byte {at:#x} is not before the ISA), so the LDS size is unknown and the lock word cannot be placed"));
    }
    Ok(at)
}

fn compute_pgm_rsrc2(ex: &ShaderExtract) -> Result<u32, String> {
    let at = rsrc2_location(ex)?;
    le32(&ex.fssm, at).ok_or_else(|| format!("64-bit LDS atomic: COMPUTE_PGM_RSRC2 not found in the shader container (byte {at:#x} is past its end), so the LDS size is unknown and the lock word cannot be placed"))
}

pub(super) fn lds_dwords(ex: &ShaderExtract) -> Result<u32, String> {
    let w = compute_pgm_rsrc2(ex)?;
    if w & RSRC2_MUST_BE_ZERO != 0 {
        return Err(format!("64-bit LDS atomic: COMPUTE_PGM_RSRC2 {w:#010x} has TRAP_PRESENT, EXCP_EN or bit 31 set, not trusted as an LDS size"));
    }
    let n = ((w >> 15) & 0x1ff) * LDS_GRANULE_DWORDS;
    if n == 0 {
        return Err(format!("64-bit LDS atomic: COMPUTE_PGM_RSRC2 {w:#010x} declares no LDS"));
    }
    if n * 4 + 4 > GROUPSHARED_BYTES {
        return Err(format!("64-bit LDS atomic: the shader declares {} bytes of LDS, no room for the lock word under the {GROUPSHARED_BYTES}-byte groupshared limit", n * 4));
    }
    Ok(n)
}

fn ds_reach(i: &Insn) -> Option<u32> {
    let m = i.mnemonic.as_str();
    if ["swizzle", "permute", "gws", "append", "consume", "ordered", "addtid"].iter().any(|k| m.contains(k)) {
        return None;
    }
    let base = m.replace("st64", "").replace("_d16", "");
    let elem = if base.contains("64") {
        8
    } else if ["_b8", "_u8", "_i8"].iter().any(|k| base.contains(k)) {
        1
    } else if ["_b16", "_u16", "_i16"].iter().any(|k| base.contains(k)) {
        2
    } else {
        4
    };
    let off = i.aux.imm as u32 & 0xffff;
    if ["write2", "read2", "wrxchg2"].iter().any(|k| m.contains(k)) {
        let unit = elem * if m.contains("st64") { 64 } else { 1 };
        return Some((off & 0xff).max(off >> 8) * unit + elem);
    }
    Some(off + elem)
}

fn pair_hi(m: &str, o: &Opnd) -> Result<Opnd, String> {
    match o {
        Opnd::Vgpr(v) => v.checked_add(1).map(Opnd::Vgpr).ok_or_else(|| format!("{m}: 64-bit operand pair starts at v255")),
        _ => Err(format!("{m}: 64-bit operand is not a VGPR pair")),
    }
}

impl Emitter<'_> {
    pub(super) fn plan_lds64(&mut self) {
        if self.is_cs && !self.is_ms() && self.insns.iter().any(|i| i.enc == Enc::Ds && is_atomic64(&i.mnemonic)) {
            self.lds64_n = lds_dwords(self.ex).ok();
        }
    }

    pub(super) fn lds_wrap(&self) -> String {
        match self.lds64_n {
            Some(n) => format!("% {n}u"),
            None => "& 8191u".into(),
        }
    }

    pub(super) fn lds64_dwords(&self) -> Result<Option<u32>, String> {
        let Some(n) = self.lds64_n else { return Ok(None) };
        if self.lds_nested && self.lds64_nested {
            return Err("a 64-bit LDS atomic sits in a branch or a loop of a shader that has another LDS access in a branch or a loop: 64-bit LDS atomics are serialised only against each other, and no group sync can separate them there".into());
        }
        for i in self.insns.iter().filter(|i| i.enc == Enc::Ds && !gds::is_gds(i)) {
            if let Some(reach) = ds_reach(i).filter(|&x| x > n * 4) {
                return Err(format!("{}: static LDS offset reaches byte {reach}, past the {} bytes the shader declares, in a shader with a 64-bit LDS atomic", i.mnemonic, n * 4));
            }
        }
        Ok(Some(n))
    }

    fn pair64(&mut self, m: &str, o: &Opnd) -> Result<String, String> {
        let h = pair_hi(m, o)?;
        self.touch(o);
        self.touch(&h);
        Ok(format!("(((uint64_t)({}) << 32) | (uint64_t)({}))", r(&h)?, r(o)?))
    }

    pub(super) fn emit_lds_atomic64(&mut self, ins: &Insn) -> Result<(), String> {
        let m = ins.mnemonic.as_str();
        let op = m.replace("_rtn", "");
        if self.is_ms() {
            return Err(format!("{m}: 64-bit LDS atomics are not translated in a mesh shader"));
        }
        lds_dwords(self.ex)?;
        if self.brace_depth != 0 || self.loop_depth != 0 {
            self.lds64_nested = true;
        }
        let off = ins.aux.imm as u32 & 0xffff;
        let wrap = self.lds_wrap();
        let two = op.starts_with("ds_wrxchg2");
        let offs: Vec<u32> = if two {
            let unit = if op.contains("st64") { 512 } else { 8 };
            vec![(off & 0xff) * unit, (off >> 8) * unit]
        } else {
            vec![off]
        };
        if offs.iter().any(|&o| o + 4 > 0xffff) {
            return Err(format!("{m}: offset {off:#x} reaches past the 16-bit DS offset"));
        }
        self.uses_lds = true;
        self.touch(&ins.src[0]);
        let addr = r(&ins.src[0])?;
        let d = self.pair64(m, &ins.src[1])?;
        let e = if two || matches!(op.as_str(), "ds_mskor_b64" | "ds_cmpst_b64") { self.pair64(m, &ins.src[2])? } else { "0".to_string() };
        let t = self.tmp;
        self.tmp += 1;
        let (o, dv, ev) = (format!("t{t}o0"), format!("t{t}d"), format!("t{t}e"));
        let new = match op.as_str() {
            "ds_add_u64" => format!("{o} + {dv}"),
            "ds_sub_u64" => format!("{o} - {dv}"),
            "ds_rsub_u64" => format!("{dv} - {o}"),
            "ds_inc_u64" => format!("({o} >= {dv}) ? (uint64_t)0 : ({o} + 1)"),
            "ds_dec_u64" => format!("(({o} == 0) || ({o} > {dv})) ? {dv} : ({o} - 1)"),
            "ds_min_u64" => format!("({dv} < {o}) ? {dv} : {o}"),
            "ds_max_u64" => format!("({dv} > {o}) ? {dv} : {o}"),
            "ds_min_i64" => format!("((int64_t){dv} < (int64_t){o}) ? {dv} : {o}"),
            "ds_max_i64" => format!("((int64_t){dv} > (int64_t){o}) ? {dv} : {o}"),
            "ds_and_b64" => format!("{o} & {dv}"),
            "ds_or_b64" => format!("{o} | {dv}"),
            "ds_xor_b64" => format!("{o} ^ {dv}"),
            "ds_mskor_b64" => format!("({o} & ~{dv}) | {ev}"),
            "ds_cmpst_b64" => format!("({o} == {dv}) ? {ev} : {o}"),
            _ => dv.clone(),
        };
        self.line(&format!("uint64_t {dv} = {d}; uint64_t {ev} = {e}; uint64_t t{t}o0 = 0; uint64_t t{t}o1 = 0;"));
        self.line(&format!(
            "if (exec_lo & 1u) {{ bool t{t}g = true; while (t{t}g) {{ uint t{t}k; InterlockedCompareExchange(tb_lds64_lock, 0u, 1u, t{t}k); if (t{t}k == 0u) {{"
        ));
        for (k, &b) in offs.iter().enumerate() {
            let at = |x: u32| format!("tb_lds[(({addr} + {x:#x}u) >> 2) {wrap}]");
            let (w0, w1) = (at(b), at(b + 4));
            let n = if !two {
                new.clone()
            } else if k == 0 {
                dv.clone()
            } else {
                ev.clone()
            };
            self.line(&format!(
                "uint t{t}l{k}; uint t{t}h{k}; InterlockedOr({w0}, 0u, t{t}l{k}); InterlockedOr({w1}, 0u, t{t}h{k}); t{t}o{k} = ((uint64_t)t{t}h{k} << 32) | (uint64_t)t{t}l{k}; uint64_t t{t}n{k} = {n}; InterlockedExchange({w0}, (uint)t{t}n{k}, t{t}k); InterlockedExchange({w1}, (uint)(t{t}n{k} >> 32), t{t}k);"
            ));
        }
        self.line(&format!("InterlockedExchange(tb_lds64_lock, 0u, t{t}k); t{t}g = false; }} }} }}"));
        self.lds_w_pending = true;
        self.lds_r_pending = true;
        if !m.contains("_rtn") {
            return Ok(());
        }
        let mut dst = *ins.dst.first().ok_or_else(|| format!("{m}: no vdst"))?;
        for k in 0..offs.len() {
            self.wv(&dst, &format!("(uint)t{t}o{k}"))?;
            let h = pair_hi(m, &dst)?;
            self.wv(&h, &format!("(uint)(t{t}o{k} >> 32)"))?;
            if k + 1 < offs.len() {
                dst = pair_hi(m, &h)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn lds_bytes(w: u32) -> u32 {
        ((w >> 15) & 0x1ff) * LDS_GRANULE_DWORDS * 4
    }

    fn static_reach(ex: &ShaderExtract) -> u32 {
        crate::rdna2::disassemble(&ex.isa).iter().filter(|i| i.enc == Enc::Ds && !gds::is_gds(i)).filter_map(ds_reach).max().unwrap_or(0)
    }

    fn fits(w: u32, reach: u32) -> bool {
        w & RSRC2_MUST_BE_ZERO == 0 && if reach == 0 { lds_bytes(w) == 0 } else { lds_bytes(w) >= reach }
    }

    fn candidates(ex: &ShaderExtract, reach: u32) -> BTreeSet<usize> {
        let end = ex.isa_offset_in_fssm.saturating_sub(16).min(ex.fssm.len());
        (0..end / 4).map(|k| k * 4).filter(|&o| le32(&ex.fssm, o).is_some_and(|w| fits(w, reach))).collect()
    }

    fn common(sets: &[BTreeSet<usize>]) -> BTreeSet<usize> {
        let mut it = sets.iter();
        let Some(first) = it.next() else { return BTreeSet::new() };
        it.fold(first.clone(), |a, s| a.intersection(s).copied().collect())
    }

    fn synth(fssm: Vec<u8>, isa_off: usize, words: &[u32]) -> ShaderExtract {
        let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        ShaderExtract {
            stage: crate::Stage::Compute,
            shader_model: (6, 0),
            bitcode_stripped: true,
            fssm,
            entry: None,
            isa: isa.clone(),
            isa_offset_in_fssm: isa_off,
            isa_preamble: [0, 64, 1, 1],
            psv_numthreads: None,
            isa_data: isa,
            has_input_sig: false,
            has_output_sig: false,
            has_psv0: false,
            has_root_sig: false,
            input_sig: Vec::new(),
            output_sig: Vec::new(),
            prim_sig: Vec::new(),
            resources: Vec::new(),
            gs_info: None,
            root_sig: None,
        }
    }

    fn header(len: usize, sub: u32, at: usize, w: u32) -> Vec<u8> {
        let mut b = vec![0xffu8; len];
        b[FSSM_SUBVERSION_AT..FSSM_SUBVERSION_AT + 4].copy_from_slice(&sub.to_le_bytes());
        b[at..at + 4].copy_from_slice(&w.to_le_bytes());
        b
    }

    const WRITE_AT_0X1FC: [u32; 3] = [0xd800_0000 | (0x0d << 18) | 0x1fc, 0x0000_0201, 0xbf81_0000];

    #[test]
    fn reader_takes_the_dword_the_fssm_sub_version_names_and_only_before_the_isa() {
        let w = 5 << 15 | 0x10;
        for (sub, at) in RSRC2_BY_SUBVERSION {
            let read = |isa_off: usize| compute_pgm_rsrc2(&synth(header(0x200, sub, at, w), isa_off, &WRITE_AT_0X1FC)).ok();
            assert_eq!(read(0x210), Some(w));
            assert_eq!(read(at + 20), Some(w));
            assert_eq!(read(at + 16), None);
            assert_eq!(read(0), None);
            assert_eq!(lds_dwords(&synth(header(0x200, sub, at, w), 0x210, &WRITE_AT_0X1FC)), Ok(640));
        }
        let v6_at_0x130 = synth(header(0x200, 6, 0x130, w), 0x210, &WRITE_AT_0X1FC);
        assert_eq!(compute_pgm_rsrc2(&v6_at_0x130), Ok(u32::MAX));
        assert!(lds_dwords(&v6_at_0x130).unwrap_err().contains("not trusted as an LDS size"));
        for sub in [0, 4, 7, u32::MAX] {
            let e = lds_dwords(&synth(header(0x200, sub, 0x130, w), 0x210, &WRITE_AT_0X1FC)).unwrap_err();
            assert!(e.contains("not found in the shader container") && e.contains("has no known location"), "{e}");
        }
        let mut durango = header(0x200, 5, 0x130, w);
        durango[..0x40].fill(0);
        durango[0..8].copy_from_slice(&0x200u64.to_le_bytes());
        durango[0x18..0x1c].copy_from_slice(&5u32.to_le_bytes());
        durango[0x28..0x30].copy_from_slice(&0x40u64.to_le_bytes());
        assert!(crate::gcn::is_durango(&durango));
        let ex = synth(durango, 0x1f0, &WRITE_AT_0X1FC);
        assert!(compute_pgm_rsrc2(&ex).is_err());
        assert!(lds_dwords(&ex).unwrap_err().contains("not found in the shader container"));
    }

    #[test]
    fn scan_recovers_a_planted_dword_and_tells_a_fixed_offset_from_one_counted_back_from_the_isa() {
        let a = synth(header(0x180, 5, 0x130, 1 << 15), 0x190, &WRITE_AT_0X1FC);
        let b = synth(header(0x280, 5, 0x130, 2 << 15 | 0x10), 0x290, &WRITE_AT_0X1FC);
        assert_eq!(static_reach(&a), 0x200);
        assert_eq!(static_reach(&synth(Vec::new(), 0, &[0xbf81_0000])), 0);
        assert!(fits(1 << 15, 0x200));
        assert!(!fits(1 << 15, 0x201));
        assert!(!fits(1 << 15 | 1 << 6, 0x200));
        assert!(!fits(1 << 15 | 1 << 24, 0x200));
        assert!(fits(0x10, 0));
        assert!(!fits(1 << 15, 0));
        let sets = [candidates(&a, 0x200), candidates(&b, 0x200)];
        assert_eq!(common(&sets), BTreeSet::from([0x130]));
        let back = [sets[0].iter().map(|o| 0x190 - o).collect(), sets[1].iter().map(|o| 0x290 - o).collect()];
        assert!(common(&back).is_empty());
        let c = synth(header(0x180, 5, 0x150, 1 << 15), 0x190, &WRITE_AT_0X1FC);
        let d = synth(header(0x280, 5, 0x250, 1 << 15), 0x290, &WRITE_AT_0X1FC);
        assert!(common(&[candidates(&c, 0x200), candidates(&d, 0x200)]).is_empty());
        assert_eq!(compute_pgm_rsrc2(&c).map(|w| fits(w, 0x200)), Ok(false));
    }

    fn dxbc_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dxbc_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "dxbc") {
                out.push(p);
            }
        }
    }

    #[test]
    #[cfg_attr(not(tombox_fixtures_shaders), ignore = "needs analysis/shaders fixtures extracted from local title dumps")]
    fn compute_pgm_rsrc2_covers_the_lds_reach_of_every_compute_fixture() {
        let mut files = Vec::new();
        dxbc_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../analysis/shaders"), &mut files);
        dxbc_files(&std::env::var_os("TOMBOX_SHADER_DUMPS").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("tombox_shaders")), &mut files);
        files.sort();
        let (mut fssm_n, mut fssm_lds, mut durango_n, mut durango_lds) = (0, 0, 0, 0);
        let mut bad = Vec::new();
        let mut fixed: std::collections::BTreeMap<Option<u32>, Vec<BTreeSet<usize>>> = Default::default();
        let mut back: std::collections::BTreeMap<Option<u32>, Vec<BTreeSet<usize>>> = Default::default();
        let (mut dfixed, mut dback) = (Vec::new(), Vec::new());
        for f in &files {
            let Ok(bytes) = std::fs::read(f) else { continue };
            let Ok(ex) = crate::extract(&bytes) else { continue };
            if ex.stage != crate::Stage::Compute {
                continue;
            }
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let durango = crate::gcn::is_durango(&ex.fssm);
            let sub = le32(&ex.fssm, FSSM_SUBVERSION_AT);
            let reach = static_reach(&ex);
            let at = rsrc2_location(&ex);
            let got = compute_pgm_rsrc2(&ex);
            let cand = candidates(&ex, reach);
            let rel: BTreeSet<usize> = cand.iter().map(|o| ex.isa_offset_in_fssm - o).collect();
            let show = |w: u32| format!("{w:#010x} (LDS {} bytes, must-be-zero bits {:#x})", lds_bytes(w), w & RSRC2_MUST_BE_ZERO);
            eprintln!(
                "{name}: {}, sub-version at 0x10 {}, ISA at {:#x}, static LDS reach {reach} bytes, reader: {}, {} dwords before the ISA fit",
                if durango { "Durango" } else { "FSSM" },
                sub.map_or("none".to_string(), |w| format!("{w:#x}")),
                ex.isa_offset_in_fssm,
                match (&at, &got) {
                    (Ok(at), Ok(w)) => format!("{at:#x} = {}", show(*w)),
                    (_, Err(e)) => e.clone(),
                    (Err(e), _) => e.clone(),
                },
                cand.len()
            );
            if reach != 0 {
                eprintln!("  fixed offsets {:#x?}", cand.iter().take(32).collect::<Vec<_>>());
                eprintln!("  counted back from the ISA {:#x?}", rel.iter().take(32).collect::<Vec<_>>());
            }
            if durango {
                durango_n += 1;
                if got.is_ok() {
                    bad.push(format!("{name}: a Durango container is read while no Durango location is proven"));
                }
                if reach != 0 {
                    durango_lds += 1;
                    dfixed.push(cand);
                    dback.push(rel);
                }
                continue;
            }
            fssm_n += 1;
            match &got {
                Ok(w) if fits(*w, reach) => {}
                Ok(w) => bad.push(format!("{name}: sub-version {sub:#x?}, the dword at {:#x} is {} for a static LDS reach of {reach} bytes", at.clone().unwrap_or(0), show(*w))),
                Err(e) => bad.push(format!("{name}: {e}")),
            }
            if reach != 0 {
                fssm_lds += 1;
                fixed.entry(sub).or_default().push(cand);
                back.entry(sub).or_default().push(rel);
            }
        }
        for (sub, sets) in &fixed {
            eprintln!(
                "FSSM sub-version {sub:#x?}: {} compute fixtures with LDS instructions; offsets that fit all of those: fixed {:#x?}, counted back from the ISA {:#x?}",
                sets.len(),
                common(sets),
                common(&back[sub])
            );
        }
        eprintln!("FSSM: {fssm_n} compute fixtures, {fssm_lds} with LDS instructions");
        eprintln!("Durango: {durango_n} compute fixtures, {durango_lds} with LDS instructions; offsets that fit all of those: fixed {:#x?}, counted back from the code {:#x?}", common(&dfixed), common(&dback));
        assert!(bad.is_empty(), "COMPUTE_PGM_RSRC2 is not where the reader looks:
{}", bad.join("
"));
        assert!(fssm_n >= 2 && fssm_lds >= 1, "not proven: {fssm_n} FSSM compute fixtures, {fssm_lds} of them with LDS instructions; at least two fixtures and one that uses LDS are needed");
        for (sub, sets) in &fixed {
            let at = RSRC2_BY_SUBVERSION.iter().find(|e| Some(e.0) == *sub).map(|e| e.1);
            assert!(at.is_some_and(|at| common(sets).contains(&at)), "sub-version {sub:#x?}: the reader's offset {at:#x?} is not among the offsets that fit every fixture");
        }
    }

}
