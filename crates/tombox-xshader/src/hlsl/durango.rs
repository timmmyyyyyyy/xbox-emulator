use super::rootmem::RM_TABLE;
use super::{Emitter, BINDLESS};
use crate::meta::{RootParamKind, StaticSampler};
use crate::ShaderExtract;
use std::fmt::Write as _;

pub(super) const FIRST_USER: u32 = 3;
pub(super) const SGPR_END: u32 = 16;

fn size_of(k: &RootParamKind) -> u32 {
    match k {
        RootParamKind::Table(_) => 1,
        RootParamKind::Descriptor { .. } => 2,
        RootParamKind::Constants { num32, .. } => *num32,
    }
}

pub(super) fn layout(ex: &ShaderExtract) -> Result<Option<Vec<(u32, u32)>>, String> {
    if !crate::gcn::is_durango(&ex.fssm) {
        return Ok(None);
    }
    let Some(rs) = ex.root_sig.as_ref() else { return Ok(None) };
    let mut u = FIRST_USER;
    let mut out = Vec::with_capacity(rs.params.len());
    for p in &rs.params {
        let n = size_of(&p.kind);
        if u < SGPR_END && u + n > SGPR_END {
            u = SGPR_END;
        }
        out.push((u, n));
        u += n;
    }
    if out.len() >= RM_TABLE as usize {
        return Err(format!("durango user data: {} root parameters exceed the table id space", out.len()));
    }
    let d = crate::gcn::parse(&ex.fssm)?;
    let w: Vec<u32> = d.data.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let mut i = 0;
    while i + 5 <= w.len() && (w[i] != 0 || w[i + 1] != 0) && i < 5 * 64 {
        let (b, c) = (w[i + 1], w[i + 2]);
        let kind = c >> 28;
        let p = (b & 0xff) as usize;
        let at = (b >> 16) & 0xff;
        if matches!(kind, 3 | 4) || (kind == 6 && at >= FIRST_USER) {
            match out.get(p) {
                Some(&(start, _)) if start == at => {}
                Some(&(start, _)) => return Err(format!("durango user data: the shader's binding record puts root parameter {p} at user dword {at}, the root-signature layout puts it at {start}")),
                None => return Err(format!("durango user data: binding record names root parameter {p}, the root signature has {}", out.len())),
            }
        }
        i += 5;
    }
    Ok(Some(out))
}

impl Emitter<'_> {
    pub(super) fn dg(&self) -> Option<&Vec<(u32, u32)>> {
        self.durango.as_ref()
    }

    pub(super) fn dg_slot(&self, sgpr: u8) -> Option<Option<(usize, u32)>> {
        let l = self.dg()?;
        if sgpr >= RM_TABLE {
            let p = (sgpr & !RM_TABLE) as usize;
            return Some((p < l.len()).then_some((p, 0)));
        }
        let s = sgpr as u32;
        Some(l.iter().position(|&(a, n)| a < SGPR_END && s >= a && s < a + n).map(|p| (p, s - l[p].0)))
    }

    pub(super) fn dg_has_mem(&self) -> bool {
        self.dg().is_some_and(|l| l.iter().any(|&(a, _)| a >= SGPR_END))
    }

    pub(super) fn dg_mem(&self, dword: u32) -> Option<(usize, u32)> {
        let l = self.dg()?;
        let u = dword.checked_add(SGPR_END)?;
        l.iter().position(|&(a, n)| a >= SGPR_END && u >= a && u < a + n).map(|p| (p, u - l[p].0))
    }

    pub(super) fn dg_table_ptr(&self, table: u8, off: u32, k: u32) -> Option<u8> {
        if self.dg().is_none() || !self.rm_ptr(table) || off & BINDLESS != 0 || off % 4 != 0 {
            return None;
        }
        let (p, 0) = self.dg_mem(off / 4 + k)? else { return None };
        match self.ex.root_sig.as_ref()?.params.get(p).map(|x| &x.kind) {
            Some(RootParamKind::Table(_)) => Some(RM_TABLE | p as u8),
            _ => None,
        }
    }

    pub(super) fn dg_sgpr_end(&self) -> Option<usize> {
        let l = self.dg()?;
        Some(l.iter().filter(|&&(a, _)| a < SGPR_END).map(|&(a, n)| (a + n) as usize).max().unwrap_or(FIRST_USER as usize).max(FIRST_USER as usize))
    }

    pub(super) fn dg_sgpr_init(&mut self) -> String {
        let mut init = String::new();
        let Some(l) = self.dg().cloned() else { return init };
        let Some(rs) = self.ex.root_sig.as_ref() else { return init };
        for (p, (a, n)) in l.iter().enumerate() {
            if *a >= SGPR_END {
                continue;
            }
            let Some(RootParamKind::Constants { .. }) = rs.params.get(p).map(|x| &x.kind) else { continue };
            for i in 0..*n {
                let r = format!("s{}", a + i);
                if self.live.contains(&r) {
                    self.rm_used.insert(p);
                    self.regs.insert(r.clone());
                    let _ = writeln!(init, "    {r} = tb_rc{p}_{i};");
                }
            }
        }
        init
    }
}

fn d3d_address(gcn_clamp: u32) -> u32 {
    match gcn_clamp {
        0 => 1,
        1 => 2,
        2 => 3,
        3 | 5 | 7 => 5,
        _ => 4,
    }
}

impl Emitter<'_> {
    fn dg_sgpr_literal(&self, idx: usize, r: u8) -> Option<u32> {
        for ins in self.insns[..idx].iter().rev() {
            if !ins.dst.iter().any(|d| matches!(d, crate::rdna2::Opnd::Sgpr(n) if *n == r)) {
                continue;
            }
            let src = match ins.src.first() {
                Some(crate::rdna2::Opnd::Literal(v)) => Some(*v),
                Some(crate::rdna2::Opnd::ConstInt(v)) => Some(*v as u32),
                _ => None,
            };
            return match ins.mnemonic.as_str() {
                "s_movk_i32" => Some(ins.aux.imm as i16 as i32 as u32),
                "s_mov_b32" => src,
                "s_brev_b32" => src.map(u32::reverse_bits),
                "s_not_b32" => src.map(|v| !v),
                _ => None,
            };
        }
        None
    }

    pub(super) fn static_sampler(&self, idx: usize, ss: u16) -> Result<Option<u32>, String> {
        let Some(rs) = self.ex.root_sig.as_ref() else { return Ok(None) };
        let vis = match self.ex.stage {
            crate::Stage::Vertex => 1,
            crate::Stage::Hull => 2,
            crate::Stage::Domain => 3,
            crate::Stage::Geometry => 4,
            crate::Stage::Pixel => 5,
            _ => 0,
        };
        let cands: Vec<_> = rs.static_samplers.iter().filter(|s| s.visibility == 0 || s.visibility == vis).collect();
        if self.dg().is_none() {
            if cands.is_empty() || ss > 102 || self.insns.iter().any(|ins| {
                ins.mnemonic.contains("branch") || ins.mnemonic.contains("setpc")
                    || ins.mnemonic.contains("swappc") || ins.mnemonic.starts_with("s_call")
                    || ins.mnemonic.starts_with("s_rfe")
            }) || self.insns[..idx].iter().any(|ins| ins.mnemonic.starts_with("s_endpgm")) {
                return Ok(None);
            }
            let words = [0, 1, 2, 3].map(|word| Self::sgpr_const_in(&self.insns, 0, idx, (ss + word) as u8));
            let [Some(w0), Some(w1), Some(w2), Some(_)] = words else {
                return Ok(None);
            };
            let decoded = decode_ssharp(w0, Some(w1), Some(w2), is_compare_sample(&self.insns[idx].mnemonic));
            let owned: Vec<StaticSampler> = cands.iter().map(|s| **s).collect();
            let pick = pick_static_sampler(&decoded, &owned)
                .map_err(|why| format!("literal sampler s{ss} (decoded {decoded:?}): {why}"))?;
            let sampler = owned[pick];
            return Ok(Some((sampler.space << 16) | sampler.register));
        }

        let pick = match cands.len() {
            0 => return Ok(None),
            1 => *cands[0],
            _ => {
                let w0 = self.dg_sgpr_literal(idx, ss as u8).ok_or_else(|| format!("literal sampler s{ss}: {} static samplers and the S# word 0 is not a constant", cands.len()))?;
                let w1 = self.dg_sgpr_literal(idx, (ss + 1) as u8);
                let w2 = self.dg_sgpr_literal(idx, (ss + 2) as u8);
                let compare = self.insns.get(idx).is_some_and(|i| is_compare_sample(&i.mnemonic));
                let d = decode_ssharp(w0, w1, w2, compare);
                let owned: Vec<StaticSampler> = cands.iter().map(|s| **s).collect();
                match pick_static_sampler(&d, &owned) {
                    Ok(i) => owned[i],
                    Err(why) => return Err(format!("literal sampler s{ss} (S# {w0:#x} {w1:x?} {w2:x?}, decoded {d:?}): {why} among {} static samplers", cands.len())),
                }
            }
        };
        Ok(Some((pick.space << 16) | pick.register))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SSharp {
    pub addr: [u32; 3],
    pub cmp_func: u32,
    pub reduction: u32,
    pub filter: Option<u32>,
    pub max_aniso: Option<u32>,
    pub lod: Option<(u32, u32)>,
}

pub fn is_compare_sample(mnemonic: &str) -> bool {
    (mnemonic.starts_with("image_sample") || mnemonic.starts_with("image_gather")) && mnemonic.split('_').any(|t| t == "c")
}

pub fn decode_ssharp(w0: u32, w1: Option<u32>, w2: Option<u32>, compare: bool) -> SSharp {
    let reduction = match (w0 >> 29) & 3 {
        1 => 2,
        2 => 3,
        _ => u32::from(compare),
    };
    let ratio = (w0 >> 9) & 7;
    let (filter, max_aniso) = match w2 {
        Some(w2) => {
            let (mag, min, mip) = ((w2 >> 20) & 3, (w2 >> 22) & 3, (w2 >> 26) & 3);
            if (mag >= 2 || min >= 2) && ratio > 0 {
                (Some(0x55), Some(1u32 << ratio.min(4)))
            } else {
                let f = (if min & 1 == 1 { 0x10 } else { 0 }) | (if mag & 1 == 1 { 0x4 } else { 0 }) | (if mip == 2 { 0x1 } else { 0 });
                (Some(f), None)
            }
        }
        None => (None, None),
    };
    SSharp {
        addr: [d3d_address(w0 & 7), d3d_address((w0 >> 3) & 7), d3d_address((w0 >> 6) & 7)],
        cmp_func: ((w0 >> 12) & 7) + 1,
        reduction,
        filter,
        max_aniso,
        lod: w1.map(|w| (w & 0xfff, (w >> 12) & 0xfff)),
    }
}

fn lod_fixed(bits: u32) -> u32 {
    let v = f32::from_bits(bits);
    if v.is_nan() {
        return 0;
    }
    (v.clamp(0.0, 4095.0 / 256.0) * 256.0) as u32
}

fn hard_match(d: &SSharp, s: &StaticSampler) -> bool {
    let reduction = (s.filter >> 7) & 3;
    [s.address_u, s.address_v, s.address_w] == d.addr && reduction == d.reduction && (reduction != 1 || s.comparison_func == d.cmp_func)
}

fn soft_score(d: &SSharp, s: &StaticSampler) -> u32 {
    let mut n = 0;
    if d.filter == Some(s.filter & 0x7f) {
        n += 2;
    }
    if let Some(a) = d.max_aniso {
        n += u32::from(s.max_anisotropy == a);
    }
    if let Some((lo, hi)) = d.lod {
        n += u32::from(lod_fixed(s.min_lod_bits) == lo) + u32::from(lod_fixed(s.max_lod_bits) == hi);
    }
    n
}

pub(super) fn same_state(a: &StaticSampler, b: &StaticSampler) -> bool {
    StaticSampler { register: 0, space: 0, visibility: 0, ..*a } == StaticSampler { register: 0, space: 0, visibility: 0, ..*b }
}

pub fn literal_mismatch(d: &SSharp, w3: u32, s: &StaticSampler) -> Option<String> {
    let addr = [s.address_u, s.address_v, s.address_w];
    if addr != d.addr {
        return Some(format!("address modes {addr:?}, literal {:?}", d.addr));
    }
    let reduction = (s.filter >> 7) & 3;
    if reduction != d.reduction {
        return Some(format!("filter reduction {reduction}, literal {}", d.reduction));
    }
    if reduction == 1 && s.comparison_func != d.cmp_func {
        return Some(format!("comparison function {}, literal {}", s.comparison_func, d.cmp_func));
    }
    let basic = s.filter & 0x7f;
    match (d.filter, d.max_aniso) {
        (Some(_), Some(a)) if basic & 0x40 == 0 || s.max_anisotropy != a => return Some(format!("filter {basic:#x} anisotropy {}, literal anisotropic x{a}", s.max_anisotropy)),
        (Some(f), None) if basic != f => return Some(format!("filter {basic:#x}, literal {f:#x}")),
        _ => {}
    }
    if addr.contains(&4) {
        let ok = match w3 >> 30 {
            0 => s.border_color == 0,
            1 => matches!(s.border_color, 1 | 3),
            2 => matches!(s.border_color, 2 | 4),
            _ => false,
        };
        if !ok {
            return Some(format!("border colour {}, literal border type {}", s.border_color, w3 >> 30));
        }
    }
    None
}

pub fn pick_static_sampler(d: &SSharp, cands: &[StaticSampler]) -> Result<usize, String> {
    let ok: Vec<usize> = (0..cands.len()).filter(|&i| hard_match(d, &cands[i])).collect();
    match ok.len() {
        0 => return Err("no static sampler has the decoded address modes and filter reduction".into()),
        1 => return Ok(ok[0]),
        _ => {}
    }
    let best = ok.iter().map(|&i| soft_score(d, &cands[i])).max().unwrap_or(0);
    let top: Vec<usize> = ok.into_iter().filter(|&i| soft_score(d, &cands[i]) == best).collect();
    if top.len() == 1 || top.iter().all(|&i| same_state(&cands[i], &cands[top[0]])) {
        return Ok(top[0]);
    }
    Err(format!("{} static samplers with different state decode equally well", top.len()))
}

#[cfg(test)]
mod ssharp_tests {
    use super::*;

    fn ss(filter: u32, addr: u32, cmp: u32, reg: u32) -> StaticSampler {
        StaticSampler { filter, address_v: addr, address_w: addr, max_anisotropy: 1, comparison_func: cmp, border_color: 0, min_lod_bits: 0, max_lod_bits: f32::MAX.to_bits(), address_u: addr, register: reg, space: 0, visibility: 0 }
    }

    #[test]
    fn trackmania_s12_comparison_sampler_resolves_by_reduction_and_func() {
        let d = decode_ssharp(0x6092, None, None, true);
        assert_eq!(d.addr, [3, 3, 3]);
        assert_eq!(d.cmp_func, 7);
        assert_eq!(d.reduction, 1);
        let cands = [ss(0x15, 1, 4, 0), ss(0x15, 3, 4, 1), ss(0x95, 3, 7, 2), ss(0x14, 3, 4, 3), ss(0x55, 1, 4, 4)];
        assert_eq!(pick_static_sampler(&d, &cands), Ok(2));
        let plain = decode_ssharp(0x92, None, None, false);
        assert!(pick_static_sampler(&plain, &cands).is_err());
        let plain = decode_ssharp(0x92, None, Some((1 << 20) | (1 << 22) | (2 << 26)), false);
        assert_eq!(plain.filter, Some(0x15));
        assert_eq!(pick_static_sampler(&plain, &cands), Ok(1));
    }

    #[test]
    fn identical_state_ties_pick_the_first_and_different_state_ties_fail() {
        let d = decode_ssharp(0x92, None, None, false);
        assert_eq!(pick_static_sampler(&d, &[ss(0x15, 3, 4, 5), ss(0x15, 3, 4, 6)]), Ok(0));
        assert!(pick_static_sampler(&d, &[ss(0x15, 3, 4, 5), ss(0x14, 3, 4, 6)]).is_err());
        assert!(pick_static_sampler(&d, &[ss(0x15, 1, 4, 5)]).is_err());
    }

    #[test]
    fn buffer_load_overwrite_invalidates_a_literal_sampler_word() {
        let words = [0xbe93_0380u32, 0xf428_0400, 0xfa00_0000];
        let isa: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        let insns = crate::rdna2::disassemble(&isa);
        assert_eq!(insns[1].mnemonic, "s_buffer_load_dwordx4");
        assert_eq!(Emitter::sgpr_const_in(&insns, 0, insns.len(), 19), None);
    }

    #[test]
    fn compare_mnemonics_and_aniso_decode() {
        assert!(is_compare_sample("image_sample_c_lz"));
        assert!(is_compare_sample("image_gather4_c"));
        assert!(!is_compare_sample("image_sample_cd"));
        assert!(!is_compare_sample("image_sample_l"));
        let d = decode_ssharp(0x92 | (4 << 9), Some(0x00f_f000), Some((3 << 20) | (3 << 22) | (2 << 26)), false);
        assert_eq!((d.filter, d.max_aniso, d.lod), (Some(0x55), Some(16), Some((0, 0xff))));
        let d = decode_ssharp(0x92 | (1 << 29), None, None, false);
        assert_eq!(d.reduction, 2);
    }
}
