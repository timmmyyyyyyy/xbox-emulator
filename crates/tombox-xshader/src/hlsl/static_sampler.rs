use super::durango::{decode_ssharp, is_compare_sample, literal_mismatch, same_state};
use super::Emitter;
use crate::fssm_bind::Source;
use crate::meta::StaticSampler;
use crate::rdna2::{Enc, Insn, Opnd};
use crate::Stage;
use std::collections::HashMap;
use std::ops::Range;

pub(super) const LAST_PLAIN_SGPR: u8 = 105;

type Known = HashMap<u8, u32>;

enum Writes {
    None,
    Regs(u8, u8),
    All,
}

fn sgpr_dst(ins: &Insn) -> Option<u8> {
    match ins.dst.first() {
        Some(Opnd::Sgpr(n)) => Some(*n),
        _ => None,
    }
}

fn wide(m: &str) -> bool {
    m.ends_with("_b64") || m.ends_with("_u64") || m.ends_with("_i64")
}

fn writes(ins: &Insn) -> Writes {
    let m = ins.mnemonic.as_str();
    let w0 = ins.raw.first().copied().unwrap_or(0);
    let regs = |base: Option<u8>, n: u8| base.map_or(Writes::None, |b| Writes::Regs(b, n));
    match ins.enc {
        Enc::Sop1 if matches!(ins.op, 0x20..=0x22 | 0x2a | 0x2e..=0x31) => Writes::All,
        Enc::Sopk if matches!(ins.op, 0x11 | 0x16) => Writes::All,
        Enc::Sopk if m.starts_with("s_cmpk_") || m.starts_with("s_setreg") || m.starts_with("s_waitcnt_") || m == "s_version" => Writes::None,
        Enc::Sop1 | Enc::Sop2 | Enc::Sopk => regs(sgpr_dst(ins), if m.starts_with("s_") && !wide(m) { 1 } else { 2 }),
        Enc::Sopc => Writes::None,
        Enc::Sopp => Writes::None,
        Enc::Smem => {
            let n = match ins.op {
                0..=4 => 1u8 << ins.op,
                8..=12 => 1u8 << (ins.op - 8),
                0x24 | 0x25 => 2,
                _ => 16,
            };
            regs(sgpr_dst(ins), n)
        }
        Enc::Vop1 => regs(sgpr_dst(ins), 1),
        Enc::Vopc => regs(sgpr_dst(ins), 2),
        Enc::Vop3 => {
            if m.contains("_co_") || m.starts_with("v_div_scale") || m.starts_with("v_mad_u64_u32") || m.starts_with("v_mad_i64_i32") {
                regs(Some(((w0 >> 8) & 0x7f) as u8), 2)
            } else if m.starts_with("v_readlane_b32") || m.starts_with("v_readfirstlane_b32") {
                regs(Some((w0 & 0xff) as u8), 1)
            } else if !m.starts_with("v_") {
                Writes::All
            } else {
                regs(sgpr_dst(ins), 2)
            }
        }
        Enc::Vop2 | Enc::Mimg | Enc::Mubuf | Enc::Mtbuf | Enc::Ds | Enc::Flat | Enc::Exp | Enc::Vinterp => Writes::None,
        _ => Writes::All,
    }
}

fn value(st: &Known, o: Option<&Opnd>) -> Option<u32> {
    match o? {
        Opnd::Literal(v) => Some(*v),
        Opnd::ConstInt(v) => Some(*v as i32 as u32),
        Opnd::ConstF32(f) => Some(f.to_bits()),
        Opnd::Sgpr(n) => st.get(n).copied(),
        _ => None,
    }
}

fn step(ins: &Insn, st: &mut Known) {
    let (base, n) = match writes(ins) {
        Writes::None => return,
        Writes::All => {
            st.clear();
            return;
        }
        Writes::Regs(b, n) => (b, n),
    };
    let m = ins.mnemonic.as_str();
    let src = ins.src.first();
    let new: [Option<u32>; 2] = match m {
        "s_mov_b32" => [value(st, src), None],
        "s_movk_i32" => [Some(ins.aux.imm as i32 as u32), None],
        "s_brev_b32" => [value(st, src).map(u32::reverse_bits), None],
        "s_not_b32" => [value(st, src).map(|v| !v), None],
        "s_mov_b64" => match src {
            Some(Opnd::Sgpr(s)) => [st.get(s).copied(), s.checked_add(1).and_then(|h| st.get(&h).copied())],
            Some(Opnd::ConstInt(v)) => [Some(*v as u64 as u32), Some((*v as u64 >> 32) as u32)],
            _ => [None, None],
        },
        _ => [None, None],
    };
    for k in 0..n {
        let Some(r) = base.checked_add(k) else { break };
        match new.get(k as usize).copied().flatten() {
            Some(v) => st.insert(r, v),
            None => st.remove(&r),
        };
    }
}

fn is_sampling(ins: &Insn) -> bool {
    ins.enc == Enc::Mimg && (ins.mnemonic.starts_with("image_sample") || ins.mnemonic.starts_with("image_gather") || ins.mnemonic == "image_get_lod")
}

pub(super) fn sampler_literals(insns: &[Insn], ranges: &[Range<usize>], succs: &[Vec<usize>]) -> HashMap<usize, [u32; 4]> {
    let mut out = HashMap::new();
    if ranges.is_empty() || !insns.iter().any(is_sampling) {
        return out;
    }
    let mut states: Vec<Option<Known>> = vec![None; ranges.len()];
    states[0] = Some(Known::new());
    let mut work = vec![0usize];
    let mut budget = 50_000usize;
    while let Some(b) = work.pop() {
        if budget == 0 {
            return HashMap::new();
        }
        budget -= 1;
        let mut st = states[b].clone().unwrap_or_default();
        for i in ranges[b].clone() {
            step(&insns[i], &mut st);
        }
        for &s in &succs[b] {
            let changed = match &mut states[s] {
                slot @ None => {
                    *slot = Some(st.clone());
                    true
                }
                Some(o) => {
                    let before = o.len();
                    o.retain(|k, v| st.get(k) == Some(v));
                    o.len() != before
                }
            };
            if changed && !work.contains(&s) {
                work.push(s);
            }
        }
    }
    for (b, r) in ranges.iter().enumerate() {
        let Some(mut st) = states[b].clone() else { continue };
        for i in r.clone() {
            let ins = &insns[i];
            if is_sampling(ins) {
                if let Some(Opnd::Sgpr(ss)) = ins.src.last() {
                    if *ss <= LAST_PLAIN_SGPR - 3 {
                        if let [Some(a), Some(b), Some(c), Some(d)] = [0u8, 1, 2, 3].map(|k| st.get(&(ss + k)).copied()) {
                            out.insert(i, [a, b, c, d]);
                        }
                    }
                }
            }
            step(ins, &mut st);
        }
    }
    out
}

fn visibility(stage: Stage) -> u32 {
    match stage {
        Stage::Vertex => 1,
        Stage::Hull => 2,
        Stage::Domain => 3,
        Stage::Geometry => 4,
        Stage::Pixel => 5,
        Stage::Amplification => 6,
        Stage::Mesh => 7,
        _ => 0,
    }
}

impl Emitter<'_> {
    fn psv_sampler_space(&self, register: u32) -> Result<u32, String> {
        let mut spaces: Vec<u32> = self.ex.resources.iter().filter(|r| r.res_type == 1 && r.lower <= register && register <= r.upper).map(|r| r.space).collect();
        spaces.sort_unstable();
        spaces.dedup();
        match (spaces.as_slice(), self.ex.has_psv0) {
            ([one], _) => Ok(*one),
            ([], false) => Ok(0),
            ([], true) => Err(format!("static sampler s{register} from the FSSM table is not among the shader's PSV0 sampler bindings")),
            (many, _) => Err(format!("static sampler s{register} from the FSSM table is bound in register spaces {many:?}")),
        }
    }

    pub(super) fn fssm_static_sampler(&mut self, idx: usize, ss: u16) -> Result<Option<u32>, String> {
        if self.dg().is_some() {
            return Ok(None);
        }
        let Ok(Some(table)) = crate::fssm_bind::parse(&self.ex.fssm) else { return Ok(None) };
        let mut used: Vec<u32> = table.uses.iter().filter(|u| u.source == Source::StaticSampler).map(|u| u.register).collect();
        used.sort_unstable();
        used.dedup();
        if used.is_empty() {
            return Ok(None);
        }
        if self.sampler_literals.is_none() {
            let (ranges, succs) = self.cfg.as_ref().ok_or("literal sampler: no control-flow graph")?;
            let found = sampler_literals(&self.insns, ranges, succs);
            self.sampler_literals = Some(found);
        }
        let Some(words) = self.sampler_literals.as_ref().and_then(|l| l.get(&idx)).copied() else { return Ok(None) };
        let Some(rs) = self.ex.root_sig.as_ref() else { return Ok(None) };
        let off = self.insns[idx].offset;
        let named = || used.iter().map(|r| format!("s{r}")).collect::<Vec<_>>().join(", ");
        if rs.static_samplers.is_empty() {
            return match used.as_slice() {
                [reg] => Ok(Some((self.psv_sampler_space(*reg)? << 16) | reg)),
                _ => Err(format!(
                    "mimg at {off:#x}: literal sampler s{ss} words {words:#x?}: the FSSM table lists {} static samplers ({}) and carries no sampler state to choose between them",
                    used.len(),
                    named()
                )),
            };
        }
        let vis = visibility(self.ex.stage);
        let cands: Vec<StaticSampler> = rs.static_samplers.iter().filter(|s| (s.visibility == 0 || s.visibility == vis) && used.contains(&s.register)).copied().collect();
        if cands.is_empty() {
            return Err(format!("mimg at {off:#x}: literal sampler s{ss}: the root signature has no visible static sampler at the FSSM table's registers ({})", named()));
        }
        let compare = is_compare_sample(&self.insns[idx].mnemonic);
        let decoded = decode_ssharp(words[0], Some(words[1]), Some(words[2]), compare);
        let mut fits: Vec<StaticSampler> = Vec::new();
        let mut why: Vec<String> = Vec::new();
        for s in &cands {
            match literal_mismatch(&decoded, words[3], s) {
                None => fits.push(*s),
                Some(m) => why.push(format!("s{}, space{}: {m}", s.register, s.space)),
            }
        }
        match fits.as_slice() {
            [] => Err(format!("mimg at {off:#x}: literal sampler s{ss} words {words:#x?} matches no static sampler at the FSSM table's registers ({})", why.join("; "))),
            [first, rest @ ..] if rest.iter().all(|s| same_state(s, first)) => Ok(Some((first.space << 16) | first.register)),
            _ => Err(format!(
                "mimg at {off:#x}: literal sampler s{ss} words {words:#x?} matches {} static samplers with different state ({})",
                fits.len(),
                fits.iter().map(|s| format!("s{}, space{}", s.register, s.space)).collect::<Vec<_>>().join("; ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rdna2::disassemble;

    fn graph(insns: &[Insn]) -> (Vec<Range<usize>>, Vec<Vec<usize>>) {
        let mut starts = vec![0usize];
        let mut tgt: HashMap<usize, usize> = HashMap::new();
        for (i, ins) in insns.iter().enumerate() {
            if ins.enc == Enc::Sopp && matches!(ins.op, 2 | 4..=9) {
                let t = (ins.offset as i64 + 4 + ins.aux.imm * 4) as usize;
                let ti = insns.iter().position(|x| x.offset == t).unwrap();
                tgt.insert(i, ti);
                starts.push(ti);
                starts.push(i + 1);
            }
        }
        starts.sort_unstable();
        starts.dedup();
        starts.retain(|&s| s < insns.len());
        let block_of = |i: usize| starts.partition_point(|&s| s <= i) - 1;
        let ranges: Vec<Range<usize>> = (0..starts.len()).map(|b| starts[b]..starts.get(b + 1).copied().unwrap_or(insns.len())).collect();
        let succs = ranges
            .iter()
            .map(|r| {
                let last = r.end - 1;
                let mut v = Vec::new();
                if let Some(&t) = tgt.get(&last) {
                    v.push(block_of(t));
                }
                let ins = &insns[last];
                if !(ins.enc == Enc::Sopp && matches!(ins.op, 1 | 2)) && r.end < insns.len() {
                    v.push(block_of(r.end));
                }
                v
            })
            .collect();
        (ranges, succs)
    }

    fn literals(words: &[u32]) -> HashMap<usize, [u32; 4]> {
        let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let insns = disassemble(&isa);
        let (r, s) = graph(&insns);
        sampler_literals(&insns, &r, &s)
    }

    const SET_S16: [u32; 9] = [0xbe90_03ff, 0x0000_0092, 0xbe91_03ff, 0x00c0_0000, 0xbe92_03ff, 0x1850_0000, 0xbe93_0380, 0xbf8c_c07f, 0xbf8c_c07f];
    const SAMPLE: [u32; 2] = [0xf080_0f08, 0x0082_0402];
    const END: u32 = 0xbf81_0000;

    fn at_sample(words: &[u32]) -> Option<[u32; 4]> {
        let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let i = disassemble(&isa).iter().position(|x| x.mnemonic == "image_sample").unwrap();
        literals(words).get(&i).copied()
    }

    #[test]
    fn straight_line_literals_reach_the_sampler() {
        let w = [&SET_S16[..], &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), Some([0x92, 0xc0_0000, 0x1850_0000, 0]));
    }

    #[test]
    fn movk_and_copies_are_followed() {
        let w = [&[0xb014_0092u32, 0xbe90_0314][..], &SET_S16[2..], &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), Some([0x92, 0xc0_0000, 0x1850_0000, 0]));
    }

    #[test]
    fn literals_survive_a_branch_when_both_paths_agree() {
        let skip_one = [0xbf88_0001u32, 0x7e00_0280];
        let w = [&SET_S16[..], &skip_one, &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), Some([0x92, 0xc0_0000, 0x1850_0000, 0]));
    }

    #[test]
    fn a_word_set_on_one_path_only_is_not_proven() {
        let w = [&SET_S16[..6], &[0xbf88_0001u32], &SET_S16[6..7], &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), None);
        let w = [&SET_S16[..], &[0xbf88_0001u32, 0xbe93_0381], &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), None);
    }

    #[test]
    fn unmodelled_scalar_writes_clobber_the_words() {
        let carry_out_s18 = [0xd70f_1200u32, 0x0002_0101];
        let readlane_s17 = [0xd760_0011u32, 0x0001_0101];
        let buffer_load_s16_x4 = [0xf428_0400u32, 0xfa00_0000];
        let swappc = [0xbe80_2104u32];
        let movreld_s16 = [0xbe90_3014u32];
        let unnamed_sop1_into_s18 = [0xbe92_3a14u32];
        for clobber in [&carry_out_s18[..], &readlane_s17, &buffer_load_s16_x4, &swappc, &movreld_s16, &unnamed_sop1_into_s18] {
            let w = [&SET_S16[..], clobber, &SAMPLE, &[END]].concat();
            assert_eq!(at_sample(&w), None, "{clobber:x?}");
        }
        let unrelated_carry_out_s40 = [0xd70f_2800u32, 0x0002_0101];
        let unnamed_sop1_into_s40 = [0xbea8_3a14u32];
        let compare_k_on_s16 = [0xb190_0092u32];
        for keep in [&unrelated_carry_out_s40[..], &unnamed_sop1_into_s40, &compare_k_on_s16] {
            let w = [&SET_S16[..], keep, &SAMPLE, &[END]].concat();
            assert_eq!(at_sample(&w), Some([0x92, 0xc0_0000, 0x1850_0000, 0]), "{keep:x?}");
        }
    }

    #[test]
    fn loop_back_edges_reach_a_fixpoint() {
        let body = [0xbe93_0380u32, 0xbf89_fffe];
        let w = [&SET_S16[..6], &body, &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), Some([0x92, 0xc0_0000, 0x1850_0000, 0]));
        let changing = [0xbe93_0381u32, 0xbf89_fffe];
        let w = [&SET_S16[..], &changing, &SAMPLE, &[END]].concat();
        assert_eq!(at_sample(&w), Some([0x92, 0xc0_0000, 0x1850_0000, 1]));
    }
}
