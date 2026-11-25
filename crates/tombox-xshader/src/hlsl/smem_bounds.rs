use super::{Emitter, LANE_TEMP_BASE};
use crate::rdna2::{Enc, Insn, Opnd};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lat {
    Const(u32),
    Hi(u32),
    Real,
    Fake,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Bound {
    Mem,
    Static { nr: u32, stride: Option<u32> },
    Rt { w1: String, w2: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Cond {
    Always,
    Never,
    Expr(String),
}

pub(super) const HELPERS: &str = "bool tb_smem_le(uint olo, uint ohi, uint llo, uint lhi) { uint elo = olo + 4u; uint ehi = ohi + (elo < 4u ? 1u : 0u); return ehi < lhi || (ehi == lhi && elo <= llo); }
bool tb_smem_in(uint w1, uint w2, uint olo, uint ohi) { uint st = (w1 >> 16) & 0x3fffu; uint llo = st != 0u ? w2 * st : w2; uint lhi = st != 0u ? ((w2 >> 16) * st + (((w2 & 0xffffu) * st) >> 16)) >> 16 : 0u; return tb_smem_le(olo, ohi, llo, lhi); }
bool tb_cbfit(uint o) { return o < 0x10000u; }
";

pub const CBFIT_CBUFFER: &str = "bool tb_cbfit(uint o) { return o < 0x10000u; }";
pub const CBFIT_RAW: &str = "bool tb_cbfit(uint o) { return true; }";

type St = HashMap<u16, Lat>;

const M0_KEY: u16 = 3000;

fn key(o: &Opnd) -> Option<u16> {
    match o {
        Opnd::Sgpr(n) => Some(*n as u16),
        Opnd::Temp(n) if *n >= LANE_TEMP_BASE => Some(1000 + (*n - LANE_TEMP_BASE)),
        Opnd::M0 => Some(M0_KEY),
        _ => None,
    }
}

fn get(st: &St, o: &Opnd) -> Lat {
    match o {
        Opnd::ConstInt(v) => Lat::Const(*v as i32 as u32),
        Opnd::ConstF32(f) => Lat::Const(f.to_bits()),
        Opnd::Literal(v) => Lat::Const(*v),
        Opnd::Null => Lat::Const(0),
        Opnd::Vgpr(_) | Opnd::VccLo | Opnd::VccHi | Opnd::ExecLo | Opnd::ExecHi | Opnd::Scc => Lat::Real,
        _ => key(o).and_then(|k| st.get(&k).copied()).unwrap_or(Lat::Fake),
    }
}

fn get_hi(st: &St, o: &Opnd) -> Lat {
    match o {
        Opnd::Sgpr(n) => st.get(&(*n as u16 + 1)).copied().unwrap_or(Lat::Fake),
        Opnd::ConstInt(v) => Lat::Const(((*v) >> 32) as u32),
        Opnd::Literal(_) | Opnd::ConstF32(_) | Opnd::Null => Lat::Const(0),
        Opnd::Vgpr(_) | Opnd::VccLo | Opnd::ExecLo | Opnd::Scc => Lat::Real,
        _ => Lat::Fake,
    }
}

fn join(a: Lat, b: Lat) -> Lat {
    match (a, b) {
        (x, y) if x == y => x,
        (Lat::Hi(x), Lat::Hi(y)) | (Lat::Hi(x), Lat::Const(y)) | (Lat::Const(y), Lat::Hi(x)) if x >> 16 == y >> 16 => Lat::Hi(x & 0xffff_0000),
        (Lat::Fake, _) | (_, Lat::Fake) | (Lat::Hi(_), _) | (_, Lat::Hi(_)) => Lat::Fake,
        _ => Lat::Real,
    }
}

fn hi_or(a: Lat, b: Lat) -> Option<Lat> {
    match (a, b) {
        (Lat::Hi(x), Lat::Const(c)) | (Lat::Const(c), Lat::Hi(x)) => Some(Lat::Hi((x | c) & 0xffff_0000)),
        _ => None,
    }
}

fn set(st: &mut St, k: u16, v: Lat) {
    if v == Lat::Fake {
        st.remove(&k);
    } else {
        st.insert(k, v);
    }
}

fn fold(m: &str, a: u32, b: u32) -> Option<u32> {
    Some(match m {
        "s_or_b32" => a | b,
        "s_and_b32" => a & b,
        "s_xor_b32" => a ^ b,
        "s_andn2_b32" => a & !b,
        "s_orn2_b32" => a | !b,
        "s_add_u32" | "s_add_i32" => a.wrapping_add(b),
        "s_sub_u32" | "s_sub_i32" => a.wrapping_sub(b),
        "s_mul_i32" => a.wrapping_mul(b),
        "s_lshl_b32" => a << (b & 31),
        "s_lshr_b32" => a >> (b & 31),
        "s_min_u32" => a.min(b),
        "s_max_u32" => a.max(b),
        _ => return None,
    })
}

fn wide(m: &str) -> bool {
    (m.ends_with("_b64") || m.ends_with("_u64") || m.ends_with("_i64")) && !super::i32_result_of_64(m)
}

fn step(ins: &Insn, st: &mut St) {
    let m = ins.mnemonic.as_str();
    match ins.enc {
        Enc::Smem => {
            let (n, v) = match ins.op {
                0..=4 => (1u16 << ins.op, Lat::Fake),
                8..=12 => (1u16 << (ins.op - 8), Lat::Real),
                0x24 | 0x25 => (2, Lat::Real),
                _ => (0, Lat::Fake),
            };
            if let Some(d) = ins.dst.first().and_then(key) {
                for k in 0..n {
                    set(st, d + k, v);
                }
                if ins.op == 1 {
                    set(st, d + 1, Lat::Hi(0));
                }
            }
        }
        Enc::Sop1 | Enc::Sop2 | Enc::Sopk | Enc::Sopc => {
            if m.starts_with("s_setreg") {
                return;
            }
            let Some(d) = ins.dst.first().and_then(key) else { return };
            let two = wide(m);
            let s0 = ins.src.first();
            let (lo, hi) = match m {
                "s_mov_b32" => (s0.map(|o| get(st, o)).unwrap_or(Lat::Fake), None),
                "s_movk_i32" => (Lat::Const(ins.aux.imm as i32 as u32), None),
                "s_mov_b64" => (s0.map(|o| get(st, o)).unwrap_or(Lat::Fake), Some(s0.map(|o| get_hi(st, o)).unwrap_or(Lat::Fake))),
                "s_cmov_b32" | "s_cmovk_i32" => {
                    let new = if m == "s_cmovk_i32" { Lat::Const(ins.aux.imm as i32 as u32) } else { s0.map(|o| get(st, o)).unwrap_or(Lat::Fake) };
                    (join(st.get(&d).copied().unwrap_or(Lat::Fake), new), None)
                }
                "s_getpc_b64" => (Lat::Fake, Some(Lat::Fake)),
                _ => {
                    let mut ls: Vec<Lat> = ins.src.iter().map(|o| get(st, o)).collect();
                    if two {
                        ls.extend(ins.src.iter().map(|o| get_hi(st, o)));
                    }
                    if ins.enc == Enc::Sopk {
                        ls.push(st.get(&d).copied().unwrap_or(Lat::Fake));
                    }
                    let hor = if m == "s_or_b32" && ls.len() == 2 { hi_or(ls[0], ls[1]) } else { None };
                    let v = if let Some(h) = hor {
                        h
                    } else if ls.iter().any(|l| matches!(l, Lat::Fake | Lat::Hi(_))) {
                        Lat::Fake
                    } else {
                        match (ls.first(), ls.get(1), two || ins.enc == Enc::Sopk) {
                            (Some(Lat::Const(a)), Some(Lat::Const(b)), false) if ls.len() == 2 => fold(m, *a, *b).map(Lat::Const).unwrap_or(Lat::Real),
                            _ => Lat::Real,
                        }
                    };
                    (v, two.then_some(v))
                }
            };
            set(st, d, lo);
            if let Some(h) = hi {
                set(st, d + 1, h);
            } else if two {
                set(st, d + 1, Lat::Fake);
            }
        }
        _ => {
            for o in &ins.dst {
                if let Some(d) = key(o) {
                    set(st, d, Lat::Real);
                    if !matches!(m, "v_readfirstlane_b32" | "v_readlane_b32") && d < 1000 {
                        set(st, d + 1, Lat::Fake);
                    }
                }
            }
        }
    }
}

fn soff_lat(st: &St, ins: &Insn) -> Lat {
    match ins.raw.get(1).copied().unwrap_or(0) >> 25 {
        0x7d => Lat::Const(0),
        s @ 0..=105 => st.get(&(s as u16)).copied().unwrap_or(Lat::Fake),
        124 => st.get(&M0_KEY).copied().unwrap_or(Lat::Fake),
        _ => Lat::Fake,
    }
}

fn record(ins: &Insn, st: &St) -> Option<[Lat; 3]> {
    let pair = |b: u8| [st.get(&(b as u16 + 1)).copied().unwrap_or(Lat::Fake), st.get(&(b as u16 + 2)).copied().unwrap_or(Lat::Fake)];
    match ins.enc {
        Enc::Smem if (8..=12).contains(&ins.op) => {
            let Some(Opnd::Sgpr(b)) = ins.src.first() else { return None };
            let [w1, w2] = pair(*b);
            Some([w1, w2, soff_lat(st, ins)])
        }
        Enc::Mubuf | Enc::Mtbuf => {
            let Some(Opnd::Sgpr(b)) = ins.src.get(1) else { return None };
            let [w1, w2] = pair(*b);
            Some([w1, w2, ins.src.get(2).map(|o| get(st, o)).unwrap_or(Lat::Fake)])
        }
        _ => None,
    }
}

pub(super) fn lattice(insns: &[Insn], ranges: &[std::ops::Range<usize>], succs: &[Vec<usize>]) -> HashMap<usize, [Lat; 3]> {
    let mut out = HashMap::new();
    if ranges.is_empty() {
        return out;
    }
    let mut states: Vec<Option<St>> = vec![None; ranges.len()];
    states[0] = Some(St::new());
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
                    let before = o.clone();
                    let keys: Vec<u16> = o.keys().copied().collect();
                    for k in keys {
                        let j = join(o[&k], st.get(&k).copied().unwrap_or(Lat::Fake));
                        set(o, k, j);
                    }
                    *o != before
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
            if let Some(l) = record(&insns[i], &st) {
                out.insert(i, l);
            }
            step(&insns[i], &mut st);
        }
    }
    out
}

pub(super) fn limit(nr: u32, stride: u32) -> u64 {
    if stride == 0 {
        nr as u64
    } else {
        nr as u64 * stride as u64
    }
}

pub(super) fn static_in(b: &Bound, o: u64) -> Option<bool> {
    if o >= 1u64 << 32 {
        return Some(false);
    }
    match b {
        Bound::Static { nr, stride: Some(s) } => Some(o + 4 <= limit(*nr, *s)),
        Bound::Static { nr, stride: None } if o + 4 <= *nr as u64 => Some(true),
        Bound::Static { nr: 0, stride: None } => Some(false),
        _ => None,
    }
}

#[cfg(test)]
fn smem_offset(soff: u32, imm: u32) -> u64 {
    (soff & !3) as u64 + (imm & !3) as u64
}

impl Emitter<'_> {
    pub(super) fn vs_bound(&mut self, idx: usize, base: u8) -> Bound {
        let Some(l) = self.vs_lat.get(&idx) else { return Bound::Mem };
        match (l[0], l[1]) {
            (Lat::Const(w1) | Lat::Hi(w1), Lat::Const(w2)) => Bound::Static { nr: w2, stride: Some((w1 >> 16) & 0x3fff) },
            (Lat::Fake, Lat::Const(w2)) => Bound::Static { nr: w2, stride: None },
            (Lat::Hi(w1), Lat::Real) => {
                let b = super::sreg_opnd(base + 2);
                self.touch(&b);
                match super::r(&b) {
                    Ok(w2) => Bound::Rt { w1: format!("{:#x}u", w1 & 0xffff_0000), w2 },
                    Err(_) => Bound::Mem,
                }
            }
            (Lat::Fake | Lat::Hi(_), _) | (_, Lat::Fake | Lat::Hi(_)) => Bound::Mem,
            _ => {
                let (a, b) = (super::sreg_opnd(base + 1), super::sreg_opnd(base + 2));
                self.touch(&a);
                self.touch(&b);
                match (super::r(&a), super::r(&b)) {
                    (Ok(w1), Ok(w2)) => Bound::Rt { w1, w2 },
                    _ => Bound::Mem,
                }
            }
        }
    }

    pub(super) fn note_cb_bound(&mut self, slot: u32, b: Option<&Bound>) {
        let lim = match b {
            Some(Bound::Static { nr, stride: Some(s) }) => Some(limit(*nr, *s)),
            _ => None,
        };
        let e = self.cb_lim.entry(slot).or_insert(lim);
        *e = match (*e, lim) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
    }

    pub(super) fn cb_limits(&self) -> std::collections::BTreeMap<u32, u64> {
        self.cb_lim.iter().filter(|(s, _)| *s & super::BINDLESS == 0).filter_map(|(s, l)| l.map(|l| (*s, l))).collect()
    }

    pub(super) fn smem_cond_static(&mut self, b: &Bound, o: u64) -> Cond {
        match static_in(b, o) {
            Some(true) => Cond::Always,
            Some(false) => Cond::Never,
            None => match b {
                Bound::Rt { w1, w2 } => {
                    self.uses_smem_bounds = true;
                    Cond::Expr(format!("tb_smem_in({w1}, {w2}, {:#x}u, 0u)", o as u32))
                }
                _ => Cond::Always,
            },
        }
    }

    pub(super) fn smem_cond_dyn(&mut self, b: &Bound, lo: &str, carry: &str, k: u32, cbfit: bool, mem_cap: bool) -> Cond {
        self.uses_smem_bounds = true;
        let lo_k = if k == 0 { lo.to_string() } else { format!("({lo} + {:#x}u)", 4 * k) };
        let hi_k = if k == 0 { carry.to_string() } else { format!("({carry} + ({lo_k} < {lo} ? 1u : 0u))") };
        let fit = if cbfit { format!(" && {hi_k} == 0u && tb_cbfit({lo_k})") } else { format!(" && {hi_k} == 0u") };
        let memc = if mem_cap { format!("({hi_k} == 0u && {lo_k} < 0x10000u)") } else { format!("({hi_k} == 0u)") };
        Cond::Expr(match b {
            Bound::Static { nr, stride: Some(s) } => {
                let l = limit(*nr, *s);
                format!("(tb_smem_le({lo_k}, {hi_k}, {:#x}u, {:#x}u){fit})", l as u32, (l >> 32) as u32)
            }
            Bound::Static { nr: 0, stride: None } => return Cond::Never,
            Bound::Static { nr, stride: None } => format!("((tb_smem_le({lo_k}, {hi_k}, {nr:#x}u, 0u){fit}) || {memc})"),
            Bound::Rt { w1, w2 } => format!("(tb_smem_in({w1}, {w2}, {lo_k}, {hi_k}){fit})"),
            Bound::Mem => memc,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_multiplies_by_stride_without_wrapping() {
        assert_eq!(limit(0x20, 0), 0x20);
        assert_eq!(limit(2, 16), 32);
        assert_eq!(limit(u32::MAX, 0x3fff), u32::MAX as u64 * 0x3fff);
    }

    #[test]
    fn offsets_round_each_term_down_to_four_and_do_not_wrap() {
        assert_eq!(smem_offset(6, 2), 4);
        assert_eq!(smem_offset(7, 0x13), 0x14);
        assert_eq!(smem_offset(0xffff_fff0, 0x20), 0x1_0000_0010);
    }

    #[test]
    fn static_proofs_follow_num_records() {
        let k = |nr, s| Bound::Static { nr, stride: Some(s) };
        assert_eq!(static_in(&k(0x20, 0), 0x1c), Some(true));
        assert_eq!(static_in(&k(0x20, 0), 0x20), Some(false));
        assert_eq!(static_in(&k(0x21, 0), 0x1d), Some(true));
        assert_eq!(static_in(&k(0x22, 0), 0x20), Some(false));
        assert_eq!(static_in(&k(2, 16), 0x1c), Some(true));
        assert_eq!(static_in(&k(2, 16), 0x20), Some(false));
        let lower = Bound::Static { nr: u32::MAX, stride: None };
        assert_eq!(static_in(&lower, 0x6f8), Some(true));
        assert_eq!(static_in(&Bound::Static { nr: 8, stride: None }, 8), None);
        assert_eq!(static_in(&Bound::Static { nr: 0, stride: None }, 0), Some(false));
        assert_eq!(static_in(&k(u32::MAX, 0x3fff), 1u64 << 32), Some(false));
        assert_eq!(static_in(&Bound::Mem, 0), None);
    }

    fn eval_le(olo: u32, ohi: u32, llo: u32, lhi: u32) -> bool {
        let elo = olo.wrapping_add(4);
        let ehi = ohi + (elo < 4) as u32;
        ehi < lhi || (ehi == lhi && elo <= llo)
    }

    fn eval_in(w1: u32, w2: u32, olo: u32, ohi: u32) -> bool {
        let st = (w1 >> 16) & 0x3fff;
        let llo = if st != 0 { w2.wrapping_mul(st) } else { w2 };
        let lhi = if st != 0 { ((w2 >> 16) * st + (((w2 & 0xffff) * st) >> 16)) >> 16 } else { 0 };
        eval_le(olo, ohi, llo, lhi)
    }

    #[test]
    fn hlsl_helper_arithmetic_matches_the_64_bit_rule() {
        for &(w2, st) in &[(0u32, 0u32), (0x20, 0), (2, 16), (u32::MAX, 0x3fff), (0x1234_5678, 0x10), (0x8000_0001, 3), (u32::MAX, 0)] {
            let lim = limit(w2, st);
            for &o in &[0u64, 4, 0x1c, 0x20, 0x24, lim.saturating_sub(4), lim.saturating_sub(3), lim, 0xffff_fffc, 0x1_0000_0000, 0x1_0000_0010] {
                let want = o + 4 <= lim;
                assert_eq!(eval_in(st << 16, w2, o as u32, (o >> 32) as u32), want, "w2={w2:#x} st={st} o={o:#x}");
            }
        }
        assert!(HELPERS.contains(CBFIT_CBUFFER));
    }

    #[test]
    fn constants_flow_across_blocks_and_descriptor_words_stay_fake() {
        let isa: Vec<u32> = vec![
            0xbece03c1,
            0xbecf03ff, 0x00024fac,
            0x884dff41, 0x00100000,
            0xbf820000,
            0xf4280d26, 0xfa000040,
            0xbf810000,
        ];
        let bytes: Vec<u8> = isa.iter().flat_map(|w| w.to_le_bytes()).collect();
        let insns = crate::rdna2::disassemble(&bytes);
        let at = insns.iter().position(|i| i.mnemonic.starts_with("s_buffer_load")).unwrap();
        let br = insns.iter().position(|i| i.mnemonic == "s_branch").unwrap();
        let ranges = vec![0..br + 1, br + 1..insns.len()];
        let succs = vec![vec![1], vec![]];
        let l = lattice(&insns, &ranges, &succs);
        assert_eq!(l.get(&at), Some(&[Lat::Fake, Lat::Const(u32::MAX), Lat::Const(0)]));
    }

    #[test]
    fn pointer_high_dword_or_stride_literal_gives_a_known_stride() {
        let isa: Vec<u32> = vec![
            0xf4041000, 0xfa0000b8,
            0xbf8cc07f,
            0x884dff41, 0x00100000,
            0xbece03c1,
            0xf4280d26, 0xfa000040,
            0xbf810000,
        ];
        let bytes: Vec<u8> = isa.iter().flat_map(|w| w.to_le_bytes()).collect();
        let insns = crate::rdna2::disassemble(&bytes);
        let at = insns.iter().position(|i| i.mnemonic.starts_with("s_buffer_load")).unwrap();
        let l = lattice(&insns, &[0..insns.len()], &[vec![]]);
        assert_eq!(l.get(&at), Some(&[Lat::Hi(0x0010_0000), Lat::Const(u32::MAX), Lat::Const(0)]));
        assert_eq!(join(Lat::Hi(0x10_0000), Lat::Const(0x10_1234)), Lat::Hi(0x10_0000));
        assert_eq!(join(Lat::Hi(0x10_0000), Lat::Real), Lat::Fake);
    }
}
