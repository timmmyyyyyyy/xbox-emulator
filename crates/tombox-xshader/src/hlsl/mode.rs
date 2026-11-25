use super::{d64, f, f16_inline_val, hi64, name, r, Emitter};
use crate::rdna2::{Enc, Insn, Opnd};
use std::collections::BTreeSet;

pub(super) const MAD_HELPERS: &str = r#"float tb_ftz(float x) { return ((asuint(x) & 0x7f800000u) == 0u) ? asfloat(asuint(x) & 0x80000000u) : x; }
float tb_madz(float a, float b, float c) { precise float p = tb_ftz(tb_ftz(a) * tb_ftz(b)); precise float s = p + tb_ftz(c); return tb_qn(tb_ftz(s)); }
float tb_madlz(float a, float b, float c) { float x = tb_ftz(a), y = tb_ftz(b); precise float p = tb_ftz((x == 0.0 || y == 0.0) ? 0.0 : x * y); precise float s = p + tb_ftz(c); return tb_qn(tb_ftz(s)); }
float tb_hz(float x) { return (abs(x) < 6.103515625e-05) ? asfloat(asuint(x) & 0x80000000u) : x; }
uint tb_hzb(uint b) { return ((b & 0x7c00u) == 0u) ? (b & 0x8000u) : b; }
"#;

pub(super) const MODE_HELPERS: &str = r#"float tb_om0(float x) { return asfloat((asuint(x) == 0x80000000u) ? 0u : asuint(x)); }
float tb_omh(float x, float k) { float r = tb_om0(x * k); return (abs(r) < 6.103515625e-05) ? asfloat(asuint(r) & 0x80000000u) : r; }
float tb_dz(float x) { return ((asuint(x) & 0x7f800000u) == 0u) ? asfloat(asuint(x) & 0x80000000u) : x; }
float tb_din(float x, uint m) { return ((m & 0x10u) != 0u) ? x : tb_dz(x); }
float tb_dout(float x, uint m) { return ((m & 0x20u) != 0u) ? x : tb_dz(x); }
float tb_hin(float x, uint m) { return ((m & 0x40u) != 0u || !(abs(x) < 6.103515625e-05)) ? x : asfloat(asuint(x) & 0x80000000u); }
uint tb_fi32(uint x, uint m) { return ((m & 0x10u) != 0u || (x & 0x7f800000u) != 0u) ? x : (x & 0x80000000u); }
uint tb_fo32(uint x, uint m) { return ((m & 0x20u) != 0u || (x & 0x7f800000u) != 0u) ? x : (x & 0x80000000u); }
uint tb_f16z(uint x, uint sel) { uint lo = x & 0xffffu, hi = x >> 16; if ((sel & 1u) != 0u && (lo & 0x7c00u) == 0u) lo &= 0x8000u; if ((sel & 2u) != 0u && (hi & 0x7c00u) == 0u) hi &= 0x8000u; return lo | (hi << 16); }
uint tb_fi16(uint x, uint m, uint sel) { return ((m & 0x40u) != 0u) ? x : tb_f16z(x, sel); }
uint tb_fo16(uint x, uint m, uint sel) { return ((m & 0x80u) != 0u) ? x : tb_f16z(x, sel); }
void tb_f64z(inout uint lo, inout uint hi, uint m, uint bit) { if ((m & bit) == 0u && (hi & 0x7ff00000u) == 0u) { lo = 0u; hi &= 0x80000000u; } }
float tb_nup(float r) { uint b = asuint(r); if ((b & 0x7fffffffu) == 0u) return asfloat(1u); if (b == 0x7f800000u) return r; return asfloat((b >> 31) != 0u ? b - 1u : b + 1u); }
float tb_ndn(float r) { uint b = asuint(r); if ((b & 0x7fffffffu) == 0u) return asfloat(0x80000001u); if (b == 0xff800000u) return r; return asfloat((b >> 31) != 0u ? b + 1u : b - 1u); }
float tb_rfix(float r, double e, uint m) { uint k = m & 3u; if (k == 0u || tb_nan(r) || !(abs(e) > 0.0)) return r; if (k == 1u) return e > 0.0 ? tb_nup(r) : r; if (k == 2u) return e < 0.0 ? tb_ndn(r) : r; if ((asuint(r) >> 31) == 0u) return e < 0.0 ? tb_ndn(r) : r; return e > 0.0 ? tb_nup(r) : r; }
double tb_res2(double x, double y, float r) { precise double s = x + y; precise double bb = s - x; precise double er = (x - (s - bb)) + (y - bb); precise double d = s - (double)r; return d + er; }
float tb_m_add(float a, float b, uint m) { precise float r = a + b; precise double e = tb_res2((double)a, (double)b, r); float q = tb_rfix(r, e, m); if ((m & 3u) == 2u && asuint(q) == 0u && !(abs(e) > 0.0) && (asuint(a) | asuint(b)) != 0u) q = asfloat(0x80000000u); return q; }
float tb_m_mul(float a, float b, uint m) { precise double p = (double)a * (double)b; precise float r = (float)p; precise double e = p - (double)r; return tb_rfix(r, e, m); }
float tb_m_mull(float a, float b, uint m) { return (a == 0.0 || b == 0.0) ? 0.0 : tb_m_mul(a, b, m); }
float tb_m_fma(float a, float b, float c, uint m) { precise double p = (double)a * (double)b; precise float r = tb_fma(a, b, c); precise double e = tb_res2(p, (double)c, r); float q = tb_rfix(r, e, m); if ((m & 3u) == 2u && asuint(q) == 0u && !(abs(e) > 0.0) && !(p == 0.0 && ((asuint(a) ^ asuint(b)) >> 31) == 0u && asuint(c) == 0u)) q = asfloat(0x80000000u); return q; }
float tb_m_cvti(int x, uint m) { precise float r = (float)x; double e = (r >= 2147483648.0) ? -1.0 : (((int)r < x) ? 1.0 : (((int)r > x) ? -1.0 : 0.0)); return tb_rfix(r, e, m); }
float tb_m_cvtu(uint x, uint m) { precise float r = tb_u2f(x); double e = (r >= 4294967296.0) ? -1.0 : (((uint)r < x) ? 1.0 : (((uint)r > x) ? -1.0 : 0.0)); return tb_rfix(r, e, m); }
float tb_m_cvtd(double x, uint m) { precise float r = (float)x; precise double e = x - (double)r; return tb_rfix(r, e, m); }
double tb_p2(int n) { return asdouble(0u, (uint)(clamp(n, -1000, 1000) + 1023) << 20); }
float tb_m_ldexp(float a, int n, uint m) { int k = clamp(n, -300, 300); precise double x = (double)a * tb_p2(k >> 1) * tb_p2(k - (k >> 1)); precise float r = (float)x; precise double e = x - (double)r; return tb_rfix(r, e, m); }
uint tb_h_rnd(double x, double er, uint k, uint zs) { if (x != x) return tb_f2h((float)x); if (x == 0.0 && er == 0.0) return zs << 15; bool ng = (x < 0.0) || (x == 0.0 && er < 0.0); double ax = ng ? -x : x; double ea = ng ? -er : er; uint sg = ng ? 0x8000u : 0u; uint lo, hi; asuint(ax, lo, hi); bool ov = (k == 0u) || (k == 1u && !ng) || (k == 2u && ng); int e = (int)((hi >> 20) & 0x7ffu) - 1023; if (e >= 16) return sg | (ov ? 0x7c00u : 0x7bffu); int ue = max(e - 10, -24); double sc = asdouble(0u, (uint)(1023 - ue) << 20); double mm = ax * sc; uint n = (uint)mm; double fr = mm - (double)n; double es = ea * sc; if (fr == 0.0 && es < 0.0) { n -= 1u; fr = 1.0; es = 0.0; } bool nz = fr > 0.0 || es > 0.0; bool up = false; if (k == 0u) up = fr > 0.5 || (fr == 0.5 && (es > 0.0 || (es == 0.0 && (n & 1u) != 0u))); else if (k != 3u) up = nz && ((k == 1u) != ng); if (up) n += 1u; uint b = (uint)((ue + 24) << 10) + n; if (b >= 0x7c00u) b = ov ? 0x7c00u : 0x7bffu; return sg | b; }
uint tb_zs(float a, float b, uint k) { uint sa = asuint(a) >> 31, sb = asuint(b) >> 31; return (sa & sb) | ((k == 2u) ? (sa | sb) : 0u); }
uint tb_h_add(float a, float b, uint m) { uint k = (m >> 2) & 3u; return tb_h_rnd((double)a + (double)b, 0.0, k, tb_zs(a, b, k)); }
uint tb_h_mul(float a, float b, uint m) { return tb_h_rnd((double)a * (double)b, 0.0, (m >> 2) & 3u, (asuint(a) ^ asuint(b)) >> 31); }
uint tb_h_fma(float a, float b, float c, uint m) { uint k = (m >> 2) & 3u; precise double p = (double)a * (double)b; precise double s = p + (double)c; precise double bb = s - p; precise double er = (p - (s - bb)) + ((double)c - bb); uint sp = (asuint(a) ^ asuint(b)) >> 31, sc = asuint(c) >> 31; return tb_h_rnd(s, er, k, (sp & sc) | ((k == 2u) ? (sp | sc) : 0u)); }
uint tb_h_cvt(double x, uint m, uint zs) { return tb_h_rnd(x, 0.0, (m >> 2) & 3u, zs); }
"#;

pub(super) const MODE_F64_HELPERS: &str = r#"bool tb_dneg(double r) { uint lo, hi; asuint(r, lo, hi); return (hi >> 31) != 0u; }
double tb_dnext(double r, bool up) { uint lo, hi; asuint(r, lo, hi); bool ng = (hi >> 31) != 0u; if (((hi & 0x7fffffffu) | lo) == 0u) return up ? asdouble(1u, 0u) : asdouble(1u, 0x80000000u); if ((hi & 0x7fffffffu) == 0x7ff00000u && lo == 0u && ng != up) return r; if (ng != up) { lo += 1u; if (lo == 0u) hi += 1u; } else { if (lo == 0u) hi -= 1u; lo -= 1u; } return asdouble(lo, hi); }
double tb_drfix(double r, double e, uint m) { uint k = (m >> 2) & 3u; if (k == 0u || r != r || !(abs(e) > 0.0L)) return r; if (k == 1u) return e > 0.0L ? tb_dnext(r, true) : r; if (k == 2u) return e < 0.0L ? tb_dnext(r, false) : r; if (!tb_dneg(r)) return e < 0.0L ? tb_dnext(r, false) : r; return e > 0.0L ? tb_dnext(r, true) : r; }
double tb_dz0(double q, double e, uint m, bool pz) { if (((m >> 2) & 3u) == 2u && q == 0.0L && !tb_dneg(q) && e == 0.0L && !pz) return asdouble(0u, 0x80000000u); return q; }
double tb_d_add(double a, double b, uint m) { precise double s = a + b; precise double bb = s - a; precise double e = (a - (s - bb)) + (b - bb); if (tb_isinf64(s) && !tb_isinf64(a) && !tb_isinf64(b)) e = -s; return tb_dz0(tb_drfix(s, e, m), e, m, a == 0.0L && !tb_dneg(a) && b == 0.0L && !tb_dneg(b)); }
double tb_d_mul(double a, double b, uint m) { precise double p = a * b; precise double e = fma(a, b, -p); if (abs(p) < asdouble(0u, 0x03500000u)) { bool sa = abs(a) <= abs(b); double sc = asdouble(0u, 0x46300000u); e = fma((sa ? a : b) * sc, sa ? b : a, -(p * sc)); } if (tb_isinf64(p) && !tb_isinf64(a) && !tb_isinf64(b)) e = -p; return tb_drfix(p, e, m); }
double tb_d_fmae(double a, double b, double c, double r) { precise double p = a * b; precise double pe = fma(a, b, -p); precise double s = p + c; precise double bb = s - p; precise double se = (p - (s - bb)) + (c - bb); precise double e = ((s - r) + se) + pe; if (tb_isinf64(r) && !tb_isinf64(a) && !tb_isinf64(b) && !tb_isinf64(c)) e = -r; return e; }
double tb_d_fma(double a, double b, double c, uint m) { precise double r = fma(a, b, c); double e = tb_d_fmae(a, b, c, r); return tb_dz0(tb_drfix(r, e, m), e, m, a * b == 0.0L && tb_dneg(a) == tb_dneg(b) && c == 0.0L && !tb_dneg(c)); }
double tb_d_ldexp(double a, int n, uint m) { double r = tb_ldexp64(a, n); double e = (tb_isinf64(r) && !tb_isinf64(a)) ? -r : (a - tb_ldexp64(r, -n)); return tb_drfix(r, e, m); }
double tb_d_fmas(double a, double b, double c, uint vcc, uint m) { double r = tb_div_fmas64(a, b, c, vcc); double e = tb_d_fmae(a, b, c, fma(a, b, c)); return tb_drfix(r, e, m); }
"#;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct ModeUse {
    pub sp: bool,
    pub dp: bool,
    pub spr: bool,
    pub spd: bool,
    pub dpr: bool,
    pub dpd: bool,
}

fn write_of(i: &Insn) -> Option<Option<(u32, u32)>> {
    match (i.enc, i.mnemonic.as_str()) {
        (Enc::Sopp, "s_round_mode") => Some(Some((0x0f, i.aux.imm as u32 & 0x0f))),
        (Enc::Sopp, "s_denorm_mode") => Some(Some((0xf0, (i.aux.imm as u32 & 0x0f) << 4))),
        (Enc::Sopk, "s_setreg_imm32_b32" | "s_setreg_b32") => {
            let sim = i.aux.imm as u32 & 0xffff;
            let (id, off, size) = (sim & 63, (sim >> 6) & 31, ((sim >> 11) & 31) + 1);
            if id != 1 || off > 7 {
                return None;
            }
            let fmask = if size >= 32 { u32::MAX } else { (1u32 << size) - 1 };
            let mask = (fmask << off) & 0xff;
            if mask == 0 {
                return None;
            }
            if i.mnemonic == "s_setreg_b32" {
                return Some(None);
            }
            let v = i.raw.get(1).copied().unwrap_or(0);
            Some(Some((mask, (v << off) & mask)))
        }
        _ => None,
    }
}

pub(super) fn reachable(insns: &[Insn], init: u8) -> BTreeSet<u8> {
    let mut writes = Vec::new();
    for i in insns {
        match write_of(i) {
            Some(Some(w)) => writes.push(w),
            Some(None) => return (0..=255u8).collect(),
            None => {}
        }
    }
    let mut set: BTreeSet<u8> = [init].into_iter().collect();
    let mut work = vec![init as u32];
    while let Some(s) = work.pop() {
        for &(mask, v) in &writes {
            let n = ((s & !mask) | v) as u8;
            if set.insert(n) {
                work.push(n as u32);
            }
        }
    }
    set
}

pub(super) fn analyze(insns: &[Insn], init: u8) -> ModeUse {
    let set = reachable(insns, init);
    let spr = set.iter().any(|&s| s & 3 != 0);
    let spd = set.iter().any(|&s| (s >> 4) & 3 != 3);
    let dpr = set.iter().any(|&s| (s >> 2) & 3 != 0);
    let dpd = set.iter().any(|&s| (s >> 6) & 3 != 3);
    ModeUse { sp: spr || spd, dp: dpr || dpd, spr, spd, dpr, dpd }
}

pub const MODE_DEFAULT: u32 = 0xf0;

const PGM_RSRC1_AT: &[(u32, usize)] = &[];

pub fn mode_from_pgm_rsrc1(rsrc1: u32) -> u32 {
    ((rsrc1 >> 12) & 0xff) | (((rsrc1 >> 21) & 1) << 8) | (((rsrc1 >> 23) & 1) << 9)
}

pub(crate) fn container_pgm_rsrc1(ex: &crate::ShaderExtract) -> Option<u32> {
    pgm_rsrc1_in(&ex.fssm, ex.isa_offset_in_fssm, PGM_RSRC1_AT)
}

fn pgm_rsrc1_in(fssm: &[u8], isa_off: usize, table: &[(u32, usize)]) -> Option<u32> {
    if crate::gcn::is_durango(fssm) {
        return None;
    }
    let rd = |o: usize| fssm.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let sub = rd(0x10)?;
    let &(_, at) = table.iter().find(|&&(v, _)| v == sub)?;
    if at + 4 > isa_off {
        return None;
    }
    rd(at)
}

pub(super) fn f16_omod(i: &Insn) -> bool {
    let m = i.mnemonic.as_str();
    i.enc == Enc::Vop3 && i.aux.imm2 != 0 && !m.starts_with("v_cmp") && (m.ends_with("_f16") || m == "v_cvt_f16_f32")
}

pub(super) fn uses_mad(insns: &[Insn]) -> bool {
    insns.iter().any(|i| is_mad(&i.mnemonic))
}

pub(super) fn is_mad(m: &str) -> bool {
    matches!(
        m,
        "v_mac_f32" | "v_mac_legacy_f32" | "v_mad_f32" | "v_mad_legacy_f32" | "v_madmk_f32" | "v_madak_f32"
    )
}

pub(super) fn din(x: String) -> String {
    format!("tb_din({x}, tb_hw1)")
}

pub(super) fn dout(e: String) -> String {
    format!("tb_dout({e}, tb_hw1)")
}

pub(super) fn arith(m: &str, legacy: bool, a: &[String], dn: &str, k: &str) -> Option<String> {
    let acc = din(format!("asfloat({dn})"));
    let mul = |x: &str, y: &str| if legacy { format!("tb_m_mull({x}, {y}, tb_hw1)") } else { format!("tb_m_mul({x}, {y}, tb_hw1)") };
    let add = |x: &str, y: &str| format!("tb_m_add({x}, {y}, tb_hw1)");
    let mad = |x: &str, y: &str, z: &str| format!("tb_ftz({})", add(&format!("tb_ftz({})", mul(&format!("tb_ftz({x})"), &format!("tb_ftz({y})"))), &format!("tb_ftz({z})")));
    let fma = |x: &str, y: &str, z: &str| format!("tb_m_fma({x}, {y}, {z}, tb_hw1)");
    let g = |n: usize| a.get(n).cloned().unwrap_or_default();
    Some(match m {
        "v_add_f32" => add(&g(0), &g(1)),
        "v_sub_f32" => add(&g(0), &format!("(-{})", g(1))),
        "v_subrev_f32" => add(&g(1), &format!("(-{})", g(0))),
        "v_mul_f32" => mul(&g(0), &g(1)),
        "v_mac_f32" => mad(&g(0), &g(1), &acc),
        "v_madmk_f32" => mad(&g(0), k, &g(1)),
        "v_madak_f32" => mad(&g(0), &g(1), k),
        "v_mad_f32" => mad(&g(0), &g(1), &g(2)),
        "v_fma_f32" => fma(&g(0), &g(1), &g(2)),
        "v_fmac_f32" => fma(&g(0), &g(1), &acc),
        "v_fmamk_f32" => fma(&g(0), k, &g(1)),
        "v_fmaak_f32" => fma(&g(0), &g(1), k),
        _ => return None,
    })
}

pub(super) fn cvt(m: &str, a: &str) -> Option<String> {
    match m {
        "v_cvt_f32_i32" => Some(format!("tb_m_cvti(asint({a}), tb_hw1)")),
        "v_cvt_f32_u32" => Some(format!("tb_m_cvtu({a}, tb_hw1)")),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum P {
    S,
    D,
    H(u32),
}

fn ty(t: &str) -> Option<P> {
    match t {
        "f16" => Some(P::H(1)),
        "f32" => Some(P::S),
        "f64" => Some(P::D),
        _ => None,
    }
}

fn is_reg(o: &Opnd) -> bool {
    matches!(o, Opnd::Sgpr(_) | Opnd::Vgpr(_) | Opnd::VccLo | Opnd::VccHi | Opnd::M0 | Opnd::ExecLo | Opnd::ExecHi | Opnd::Temp(_))
}

fn pk_sel(ins: &Insn) -> [u32; 2] {
    let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
    [(w0 >> 11) & 7, ((w1 >> 27) & 3) | (((w0 >> 14) & 1) << 2)]
}

fn hsel(ins: &Insn, i: usize) -> u32 {
    if let Some(w) = ins.aux.sdwa {
        let sel = if i == 0 { (w >> 16) & 7 } else { (w >> 24) & 7 };
        return if sel == 5 { 2 } else { 1 };
    }
    if ins.enc == Enc::Vop3 && i < 3 && (ins.raw[0] >> (11 + i)) & 1 != 0 { 2 } else { 1 }
}

impl Emitter<'_> {
    fn mode_dst_half(&self, ins: &Insn) -> Option<u32> {
        let m = ins.mnemonic.as_str();
        if m.starts_with("v_pk_") || m == "v_cvt_pkrtz_f16_f32" {
            return Some(3);
        }
        if m == "v_fma_mixlo_f16" {
            return Some(1);
        }
        if m == "v_fma_mixhi_f16" {
            return Some(2);
        }
        if let Some((_, hi)) = &self.d16 {
            return Some(if *hi { 2 } else { 1 });
        }
        if let Some(w) = ins.aux.sdwa {
            return match (w >> 8) & 7 {
                5 => Some(2),
                4 | 6 => Some(1),
                _ => None,
            };
        }
        Some(1)
    }

    fn mode_spec(&self, ins: &Insn) -> Option<(Vec<(Opnd, P)>, Option<(Opnd, P)>)> {
        let m = ins.mnemonic.as_str();
        if !m.starts_with("v_") || !(m.contains("f16") || m.contains("f32") || m.contains("f64")) {
            return None;
        }
        if m.contains("class") || m == "v_pack_b32_f16" || m.starts_with("v_interp") {
            return None;
        }
        let s = &ins.src;
        let d = ins.dst.first().cloned();
        let half = |p: P, i: usize| match p {
            P::H(_) => P::H(hsel(ins, i)),
            o => o,
        };
        let dhalf = |p: P| match p {
            P::H(_) => self.mode_dst_half(ins).map(P::H),
            o => Some(o),
        };
        let all = |p: P, n: usize| -> Vec<(Opnd, P)> { s.iter().take(n).enumerate().map(|(i, o)| (o.clone(), half(p, i))).collect() };
        let suffix = ty(m.rsplit('_').next().unwrap_or(""));
        if let Some(rest) = m.strip_prefix("v_cvt_") {
            let parts: Vec<&str> = rest.split('_').collect();
            let st = ty(parts.last().copied().unwrap_or(""));
            let first = parts[0];
            let dt = match first {
                "pkrtz" => Some(P::H(3)),
                "pknorm" | "norm" | "flr" | "rpi" | "pk" => None,
                t => ty(t),
            };
            let n = match first {
                "pkrtz" | "pknorm" => 2,
                _ => 1,
            };
            let srcs = st.map(|p| all(p, n)).unwrap_or_default();
            let dst = dt.and_then(|p| if p == P::H(3) { Some(p) } else { dhalf(p) }).and_then(|p| d.clone().map(|o| (o, p)));
            return Some((srcs, dst));
        }
        let p = suffix?;
        if m.starts_with("v_cmp") {
            return Some((all(p, 2), None));
        }
        if m.starts_with("v_frexp_exp") {
            return Some((all(p, 1), None));
        }
        if m.starts_with("v_ldexp") || m == "v_trig_preop_f64" {
            return Some((all(p, 1), dhalf(p).and_then(|q| d.clone().map(|o| (o, q)))));
        }
        if m.starts_with("v_fma_mix") {
            let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
            let (opsel, oph) = ((w0 >> 11) & 7, ((w1 >> 27) & 3) | (((w0 >> 14) & 1) << 2));
            let srcs = s.iter().take(3).enumerate().map(|(i, o)| (o.clone(), if (oph >> i) & 1 != 0 { P::H(if (opsel >> i) & 1 != 0 { 2 } else { 1 }) } else { P::S })).collect();
            let dp = if m == "v_fma_mix_f32" { P::S } else { P::H(self.mode_dst_half(ins)?) };
            return Some((srcs, d.map(|o| (o, dp))));
        }
        if m.starts_with("v_pk_") {
            let sel = pk_sel(ins);
            let n = if m == "v_pk_fma_f16" { 3 } else { 2 };
            let mut srcs: Vec<(Opnd, P)> = s
                .iter()
                .take(n)
                .enumerate()
                .map(|(i, o)| {
                    let mk = if m == "v_pk_fmac_f16" { 3 } else { (if (sel[0] >> i) & 1 != 0 { 2 } else { 1 }) | (if (sel[1] >> i) & 1 != 0 { 2 } else { 1 }) };
                    (o.clone(), P::H(mk))
                })
                .collect();
            if m == "v_pk_fmac_f16" {
                if let Some(o) = d.clone() {
                    srcs.push((o, P::H(3)));
                }
            }
            return Some((srcs, d.map(|o| (o, P::H(3)))));
        }
        let mut srcs = all(p, s.len().min(3));
        let dp = dhalf(p)?;
        if matches!(m, "v_mac_f32" | "v_fmac_f32" | "v_mac_legacy_f32" | "v_fmac_f16") {
            if let Some(o) = d.clone() {
                srcs.push((o, dp));
            }
        }
        Some((srcs, d.map(|o| (o, dp))))
    }

    fn mode_active(&self, p: P) -> bool {
        match p {
            P::S => self.mode_use.spd,
            _ => self.mode_use.dpd,
        }
    }

    pub(super) fn emit_vec_mode(&mut self, ins: &Insn) -> Result<(), String> {
        if !(self.mode_use.sp || self.mode_use.dp) {
            return self.emit_vec(ins);
        }
        let Some((srcs, dst)) = self.mode_spec(ins) else {
            return self.emit_vec(ins);
        };
        let mut saved: Vec<(String, String, P)> = Vec::new();
        let add = |saved: &mut Vec<(String, String, P)>, n: String, p: P| {
            if let Some(e) = saved.iter_mut().find(|e| e.0 == n) {
                if let (P::H(a), P::H(b)) = (e.2, p) {
                    e.2 = P::H(a | b);
                }
                return;
            }
            saved.push((n, String::new(), p));
        };
        for (o, p) in &srcs {
            if let Opnd::Literal(v) = o {
                let den = match p {
                    P::S => v & 0x7f80_0000 == 0 && v & 0x7fff_ffff != 0,
                    P::H(_) => v & 0x7c00 == 0 && v & 0x7fff != 0,
                    P::D => false,
                };
                if den && self.mode_active(*p) && !matches!(ins.mnemonic.as_str(), "v_madmk_f32" | "v_madak_f32" | "v_fmamk_f32" | "v_fmaak_f32") {
                    return Err(format!("{}: denormal literal operand {v:#x} under a shader-written denormal MODE", ins.mnemonic));
                }
                continue;
            }
            if !is_reg(o) || !self.mode_active(*p) {
                continue;
            }
            if *p != P::D {
                add(&mut saved, name(o)?, *p);
            }
        }
        let t = self.tmp;
        self.tmp += 1;
        for (k, (n, sv, p)) in saved.iter_mut().enumerate() {
            *sv = format!("tbm{t}_{k}");
            match p {
                P::S => self.line(&format!("uint {sv} = {n}; {n} = tb_fi32({n}, tb_hw1);")),
                P::H(sel) => self.line(&format!("uint {sv} = {n}; {n} = tb_fi16({n}, tb_hw1, {sel}u);")),
                P::D => {}
            }
            self.regs.insert(n.clone());
        }
        let mut pairs: Vec<(String, String, String, String)> = Vec::new();
        for (o, p) in &srcs {
            if *p == P::D && is_reg(o) && self.mode_active(P::D) {
                let (lo, hi) = (name(o)?, name(&hi64(o))?);
                if pairs.iter().any(|e| e.0 == lo) {
                    continue;
                }
                let k = pairs.len();
                let (sl, sh) = (format!("tbm{t}_dl{k}"), format!("tbm{t}_dh{k}"));
                self.touch(o);
                self.touch(&hi64(o));
                self.line(&format!("uint {sl} = {lo}, {sh} = {hi}; tb_f64z({lo}, {hi}, tb_hw1, 0x40u);"));
                pairs.push((lo, hi, sl, sh));
            }
        }
        self.emit_vec(ins)?;
        let mut dregs: Vec<(String, u32)> = Vec::new();
        if let Some((o, p)) = &dst {
            if is_reg(o) {
                match p {
                    P::S => dregs.push((name(o)?, 0xffff_ffff)),
                    P::H(sel) => dregs.push((name(o)?, (if sel & 1 != 0 { 0xffff } else { 0 }) | (if sel & 2 != 0 { 0xffff_0000 } else { 0 }))),
                    P::D => {
                        dregs.push((name(o)?, 0xffff_ffff));
                        dregs.push((name(&hi64(o))?, 0xffff_ffff));
                    }
                }
                if self.mode_active(*p) {
                    match p {
                        P::S => {
                            let n = name(o)?;
                            self.line(&format!("if (exec_lo & 1u) {n} = tb_fo32({n}, tb_hw1);"));
                        }
                        P::H(sel) => {
                            let n = name(o)?;
                            self.line(&format!("if (exec_lo & 1u) {n} = tb_fo16({n}, tb_hw1, {sel}u);"));
                        }
                        P::D => {
                            let (lo, hi) = (name(o)?, name(&hi64(o))?);
                            self.line(&format!("if (exec_lo & 1u) tb_f64z({lo}, {hi}, tb_hw1, 0x80u);"));
                        }
                    }
                }
            }
        }
        let restore = |me: &mut Self, n: &str, sv: &str| {
            match dregs.iter().find(|e| e.0 == n) {
                Some(&(_, mask)) if mask == 0xffff_ffff => me.line(&format!("if ((exec_lo & 1u) == 0u) {n} = {sv};")),
                Some(&(_, mask)) => me.line(&format!("{n} = ((exec_lo & 1u) == 0u) ? {sv} : (({n} & {mask:#x}u) | ({sv} & {:#x}u));", !mask)),
                None => me.line(&format!("{n} = {sv};")),
            }
        };
        for (n, sv, _) in &saved {
            restore(self, n, sv);
        }
        for (lo, hi, sl, sh) in &pairs {
            restore(self, lo, sl);
            restore(self, hi, sh);
        }
        Ok(())
    }

    fn hval(&self, ins: &Insn, i: usize, sel: Option<u32>) -> Result<String, String> {
        let o = ins.src.get(i).ok_or("missing operand")?;
        let base = match o {
            Opnd::ConstF32(x) => f16_inline_val(*x, sel.unwrap_or_else(|| hsel(ins, i)) == 2),
            Opnd::Literal(v) => format!("f16tof32({:#x}u)", v & 0xffff),
            _ => {
                let hi = sel.unwrap_or_else(|| hsel(ins, i)) == 2;
                if hi { format!("f16tof32(({}) >> 16)", r(o)?) } else { format!("f16tof32(({}) & 0xffffu)", r(o)?) }
            }
        };
        Ok(format!("tb_hin({base}, tb_hw1)"))
    }

    fn vop3_mods(&self, ins: &Insn, i: usize, e: String) -> String {
        let mut e = e;
        if ins.enc == Enc::Vop3 {
            if (ins.aux.mask >> i) & 1 != 0 {
                e = format!("abs({e})");
            }
            if (ins.aux.mask >> (3 + i)) & 1 != 0 {
                e = format!("(-{e})");
            }
        }
        e
    }

    fn hres_mode(&self, ins: &Insn, bits: String) -> String {
        if ins.enc != Enc::Vop3 || (ins.aux.imm2 == 0 && ins.aux.imm == 0) {
            return bits;
        }
        let mut e = format!("f16tof32({bits})");
        let k = match ins.aux.imm2 {
            1 => Some("2.0"),
            2 => Some("4.0"),
            3 => Some("0.5"),
            _ => None,
        };
        if let Some(k) = k {
            e = format!("(((tb_hw1 & 0x280u) == 0u) ? tb_omh(({e}), {k}) : ({e}))");
        }
        if ins.aux.imm != 0 {
            e = format!("saturate({e})");
        }
        format!("tb_f2h({e})")
    }

    pub(super) fn mode_own(&mut self, ins: &Insn) -> Result<bool, String> {
        let m = ins.mnemonic.as_str();
        let u = self.mode_use;
        let f16_ops = matches!(
            m,
            "v_add_f16" | "v_sub_f16" | "v_subrev_f16" | "v_mul_f16" | "v_fma_f16" | "v_fmac_f16" | "v_fmamk_f16" | "v_fmaak_f16" | "v_ldexp_f16" | "v_cvt_f16_f32"
                | "v_cvt_f16_i16" | "v_cvt_f16_u16" | "v_pk_add_f16" | "v_pk_mul_f16" | "v_pk_fma_f16" | "v_pk_fmac_f16" | "v_fma_mixlo_f16" | "v_fma_mixhi_f16"
        );
        let f64_ops = matches!(m, "v_add_f64" | "v_mul_f64" | "v_fma_f64" | "v_ldexp_f64" | "v_div_fmas_f64");
        let f32_ops = matches!(m, "v_fma_mix_f32" | "v_cvt_f32_f64" | "v_mullit_f32" | "v_ldexp_f32");
        if !((f16_ops && u.dp) || (f64_ops && u.dpr) || (f32_ops && u.sp)) {
            return Ok(false);
        }
        if ins.aux.sdwa.is_some() {
            return Err(format!("{m}: SDWA operand selection under a shader-written MODE is not supported"));
        }
        self.hw_regs.insert(1);
        let s = &ins.src;
        let d = ins.dst.first().cloned().ok_or("no dst")?;
        let t = self.tmp;
        self.tmp += 1;
        if f64_ops {
            self.uses_f64 = true;
            for o in s.iter().chain(ins.dst.iter()) {
                if matches!(o, Opnd::Vgpr(_) | Opnd::Sgpr(_) | Opnd::VccLo | Opnd::ExecLo) {
                    self.touch(o);
                    self.touch(&hi64(o));
                }
            }
            let a = |i: usize| -> Result<String, String> { Ok(self.vop3_mods(ins, i, d64(s.get(i).ok_or("missing operand")?)?)) };
            let e = match m {
                "v_add_f64" => format!("tb_d_add({}, {}, tb_hw1)", a(0)?, a(1)?),
                "v_mul_f64" => format!("tb_d_mul({}, {}, tb_hw1)", a(0)?, a(1)?),
                "v_fma_f64" => format!("tb_d_fma({}, {}, {}, tb_hw1)", a(0)?, a(1)?, a(2)?),
                "v_ldexp_f64" => format!("tb_d_ldexp({}, asint({}), tb_hw1)", a(0)?, r(&s[1])?),
                _ => format!("tb_d_fmas({}, {}, {}, vcc_lo, tb_hw1)", a(0)?, a(1)?, a(2)?),
            };
            let mut e = e;
            if ins.enc == Enc::Vop3 {
                e = match ins.aux.imm2 {
                    1 => format!("(({e}) * 2.0L)"),
                    2 => format!("(({e}) * 4.0L)"),
                    3 => format!("(({e}) * 0.5L)"),
                    _ => e,
                };
                if ins.aux.imm != 0 {
                    e = format!("clamp({e}, 0.0L, 1.0L)");
                }
            }
            self.line(&format!("double t{t} = {e}; uint t{t}l, t{t}h; asuint(t{t}, t{t}l, t{t}h);"));
            self.wv(&d, &format!("t{t}l"))?;
            self.wv(&hi64(&d), &format!("t{t}h"))?;
            return Ok(true);
        }
        if f32_ops {
            let e = match m {
                "v_cvt_f32_f64" => {
                    self.uses_f64 = true;
                    self.touch(&s[0]);
                    self.touch(&hi64(&s[0]));
                    let x = self.vop3_mods(ins, 0, d64(&s[0])?);
                    format!("tb_m_cvtd({x}, tb_hw1)")
                }
                "v_ldexp_f32" => format!("tb_m_ldexp({}, asint({}), tb_hw1)", din(self.fm(ins, 0)?), r(&s[1])?),
                "v_mullit_f32" => {
                    let (a, b, c) = (din(self.fm(ins, 0)?), din(self.fm(ins, 1)?), din(self.fm(ins, 2)?));
                    self.line(&format!("float t{t}a = {a}, t{t}b = {b}, t{t}c = {c};"));
                    format!("((asuint(t{t}b) == 0xff7fffffu || asuint(t{t}b) == 0xff800000u || tb_nan(t{t}b) || (asuint(t{t}c) & 0x7fffffffu) == 0u || (asuint(t{t}c) >> 31) != 0u || tb_nan(t{t}c)) ? -3.402823466e+38 : tb_m_mul(t{t}a, t{t}b, tb_hw1))")
                }
                _ => {
                    let v = self.mix_vals(ins)?;
                    let e = format!("tb_m_fma({}, {}, {}, tb_hw1)", v[0], v[1], v[2]);
                    let clamp = (ins.raw[0] >> 15) & 1 != 0;
                    return self.wv(&d, &format!("asuint({})", if clamp { format!("saturate({e})") } else { e })).map(|_| true);
                }
            };
            let e = self.fres(ins, dout(e));
            self.wv(&d, &e)?;
            return Ok(true);
        }
        let h = |me: &Self, i: usize| -> Result<String, String> { Ok(me.vop3_mods(ins, i, me.hval(ins, i, None)?)) };
        let bits = match m {
            "v_add_f16" => format!("tb_h_add({}, {}, tb_hw1)", h(self, 0)?, h(self, 1)?),
            "v_sub_f16" => format!("tb_h_add({}, (-{}), tb_hw1)", h(self, 0)?, h(self, 1)?),
            "v_subrev_f16" => format!("tb_h_add({}, (-{}), tb_hw1)", h(self, 1)?, h(self, 0)?),
            "v_mul_f16" => format!("tb_h_mul({}, {}, tb_hw1)", h(self, 0)?, h(self, 1)?),
            "v_fma_f16" => format!("tb_h_fma({}, {}, {}, tb_hw1)", h(self, 0)?, h(self, 1)?, h(self, 2)?),
            "v_ldexp_f16" => format!("tb_h_cvt((double)({}) * tb_p2(clamp(asint(({}) << 16) >> 16, -64, 64)), tb_hw1, asuint({}) >> 31)", h(self, 0)?, r(&s[1])?, h(self, 0)?),
            "v_cvt_f16_f32" => {
                let x = din(self.fm(ins, 0)?);
                format!("tb_h_cvt((double)({x}), tb_hw1, asuint({x}) >> 31)")
            }
            "v_cvt_f16_i16" => format!("tb_h_cvt((double)(asint(({}) << 16) >> 16), tb_hw1, 0u)", r(&s[0])?),
            "v_cvt_f16_u16" => format!("tb_h_cvt((double)(({}) & 0xffffu), tb_hw1, 0u)", r(&s[0])?),
            "v_fmac_f16" => {
                self.touch(&d);
                let hi = matches!(self.d16, Some((_, true)));
                let acc = format!("tb_hin(f16tof32(({}){}), tb_hw1)", r(&d)?, if hi { " >> 16" } else { " & 0xffffu" });
                format!("tb_h_fma({}, {}, {acc}, tb_hw1)", h(self, 0)?, h(self, 1)?)
            }
            "v_fmamk_f16" | "v_fmaak_f16" => {
                let k = format!("tb_hin(f16tof32({:#x}u), tb_hw1)", ins.raw.last().copied().unwrap_or(0) & 0xffff);
                let (a, b) = (h(self, 0)?, h(self, 1)?);
                if m == "v_fmaak_f16" { format!("tb_h_fma({a}, {b}, {k}, tb_hw1)") } else { format!("tb_h_fma({a}, {k}, {b}, tb_hw1)") }
            }
            "v_fma_mixlo_f16" | "v_fma_mixhi_f16" => {
                let v = self.mix_vals(ins)?;
                let mut b = format!("tb_h_fma({}, {}, {}, tb_hw1)", v[0], v[1], v[2]);
                if (ins.raw[0] >> 15) & 1 != 0 {
                    b = format!("tb_f2h(saturate(f16tof32({b})))");
                }
                self.touch(&d);
                let dn = r(&d)?;
                let e = if m == "v_fma_mixlo_f16" { format!("(({dn}) & 0xffff0000u) | {b}") } else { format!("(({dn}) & 0xffffu) | (({b}) << 16)") };
                self.wv(&d, &e)?;
                return Ok(true);
            }
            _ => {
                let sel = pk_sel(ins);
                let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
                let neg = [(w1 >> 29) & 7, (w0 >> 8) & 7];
                let nsrc = if m == "v_pk_fma_f16" { 3 } else { 2 };
                if m == "v_pk_fmac_f16" {
                    self.touch(&d);
                }
                let mut halves = Vec::new();
                for hh in 0..2usize {
                    let mut v = Vec::new();
                    for i in 0..nsrc {
                        let o = &s[i];
                        let hi = if m == "v_pk_fmac_f16" { hh == 1 } else { (sel[hh] >> i) & 1 != 0 };
                        let base = match o {
                            Opnd::ConstF32(_) => f(o)?,
                            _ => {
                                let x = r(o)?;
                                if hi { format!("f16tof32(({x}) >> 16)") } else { format!("f16tof32(({x}) & 0xffffu)") }
                            }
                        };
                        let mut e = format!("tb_hin({base}, tb_hw1)");
                        if m != "v_pk_fmac_f16" && (neg[hh] >> i) & 1 != 0 {
                            e = format!("(-{e})");
                        }
                        v.push(e);
                    }
                    if m == "v_pk_fmac_f16" {
                        let x = r(&d)?;
                        v.push(format!("tb_hin({}, tb_hw1)", if hh == 1 { format!("f16tof32(({x}) >> 16)") } else { format!("f16tof32(({x}) & 0xffffu)") }));
                    }
                    let mut b = match m {
                        "v_pk_add_f16" => format!("tb_h_add({}, {}, tb_hw1)", v[0], v[1]),
                        "v_pk_mul_f16" => format!("tb_h_mul({}, {}, tb_hw1)", v[0], v[1]),
                        _ => format!("tb_h_fma({}, {}, {}, tb_hw1)", v[0], v[1], v[2]),
                    };
                    if ins.enc == Enc::Vop3 && (w0 >> 15) & 1 != 0 {
                        b = format!("tb_f2h(saturate(f16tof32({b})))");
                    }
                    halves.push(b);
                }
                self.wv(&d, &format!("(({}) | (({}) << 16))", halves[0], halves[1]))?;
                return Ok(true);
            }
        };
        let b = self.hres_mode(ins, bits);
        self.wv(&d, &b)?;
        Ok(true)
    }

    fn mix_vals(&self, ins: &Insn) -> Result<Vec<String>, String> {
        let (w0, w1) = (ins.raw[0], ins.raw.get(1).copied().unwrap_or(0));
        let opsel = (w0 >> 11) & 7;
        let opsel_hi = ((w1 >> 27) & 3) | (((w0 >> 14) & 1) << 2);
        let (neg, abs) = ((w1 >> 29) & 7, (w0 >> 8) & 7);
        let mut v = Vec::new();
        for i in 0..3 {
            let x = r(&ins.src[i])?;
            let mut e = if (opsel_hi >> i) & 1 != 0 {
                let b = if (opsel >> i) & 1 != 0 { format!("f16tof32(({x}) >> 16)") } else { format!("f16tof32(({x}) & 0xffffu)") };
                format!("tb_hin({b}, tb_hw1)")
            } else {
                din(format!("asfloat({x})"))
            };
            if (abs >> i) & 1 != 0 {
                e = format!("abs({e})");
            }
            if (neg >> i) & 1 != 0 {
                e = format!("(-{e})");
            }
            v.push(e);
        }
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    fn container(sub: u32, at: usize, rsrc1: u32, len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0x10..0x14].copy_from_slice(&sub.to_le_bytes());
        b[at..at + 4].copy_from_slice(&rsrc1.to_le_bytes());
        b
    }

    #[test]
    fn pgm_rsrc1_maps_float_mode_dx10_clamp_and_ieee_into_mode() {
        assert_eq!(mode_from_pgm_rsrc1(0), 0);
        assert_eq!(mode_from_pgm_rsrc1(0xf0 << 12), 0xf0);
        assert_eq!(mode_from_pgm_rsrc1(0x30 << 12), 0x30);
        assert_eq!(mode_from_pgm_rsrc1(1 << 21), 0x100);
        assert_eq!(mode_from_pgm_rsrc1(1 << 23), 0x200);
        assert_eq!(mode_from_pgm_rsrc1((1 << 20) | (1 << 22) | 0xfff | (0xff << 24)), 0);
        assert_eq!(mode_from_pgm_rsrc1((0x5a << 12) | (1 << 21) | (1 << 23) | 0xfff), 0x35a);
    }

    #[test]
    fn pgm_rsrc1_reader_follows_the_location_table_and_refuses_everything_else() {
        let t: &[(u32, usize)] = &[(5, 0x40), (6, 0x48)];
        assert_eq!(pgm_rsrc1_in(&container(5, 0x40, 0x0030_0000, 0x100), 0x80, t), Some(0x0030_0000));
        assert_eq!(pgm_rsrc1_in(&container(6, 0x48, 0x0080_0000, 0x100), 0x80, t), Some(0x0080_0000));
        assert_eq!(pgm_rsrc1_in(&container(7, 0x40, 0x0030_0000, 0x100), 0x80, t), None);
        assert_eq!(pgm_rsrc1_in(&container(5, 0x40, 0x0030_0000, 0x100), 0x42, t), None);
        assert_eq!(pgm_rsrc1_in(&container(5, 0x40, 0x0030_0000, 0x44), 0x200, t), Some(0x0030_0000));
        assert_eq!(pgm_rsrc1_in(&[0u8; 0x12], 0x80, t), None);
        for sub in [5, 6] {
            assert_eq!(pgm_rsrc1_in(&container(sub, 0x40, 0x0030_0000, 0x200), 0x180, PGM_RSRC1_AT), None);
        }
        let mut dg = vec![0u8; 0x100];
        dg[0..8].copy_from_slice(&0x100u64.to_le_bytes());
        dg[0x10..0x14].copy_from_slice(&5u32.to_le_bytes());
        dg[0x18..0x1c].copy_from_slice(&5u32.to_le_bytes());
        dg[0x28..0x30].copy_from_slice(&0x80u64.to_le_bytes());
        assert!(crate::gcn::is_durango(&dg));
        assert_eq!(pgm_rsrc1_in(&dg, 0x80, &[(5, 0x40)]), None);
    }

    #[test]
    fn initial_mode_seeds_the_reachable_mode_set() {
        assert_eq!(reachable(&[], 0xf0).into_iter().collect::<Vec<_>>(), vec![0xf0]);
        assert_eq!(reachable(&[], 0x30).into_iter().collect::<Vec<_>>(), vec![0x30]);
        assert_eq!(analyze(&[], 0xf0), ModeUse::default());
        let u = analyze(&[], 0x30);
        assert!(u.dp && u.dpd && !u.sp);
        let u = analyze(&[], 0x00);
        assert!(u.sp && u.spd && u.dp && u.dpd && !u.spr && !u.dpr);
    }

    const RSRC1_RESERVED_ALL: u32 = 0x0050_0c00;
    const RSRC1_RESERVED_CS: u32 = 0x1b00_0000;
    const RSRC1_ROUND: u32 = 0x0000_f000;
    const RSRC2_ADJACENT: [(u32, usize); 2] = [(5, 0x12c), (6, 0x134)];

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RegUse {
        vgprs: u32,
        sgprs: u32,
        wave: u32,
        compute: bool,
    }

    fn le32(b: &[u8], o: usize) -> Option<u32> {
        b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    }

    fn reg_use(ex: &crate::ShaderExtract) -> RegUse {
        let insns = crate::rdna2::disassemble(&ex.isa);
        let mut v = 0u32;
        let mut sg = 0u32;
        for o in insns.iter().flat_map(|i| i.dst.iter().chain(i.src.iter())) {
            match o {
                Opnd::Vgpr(n) => v = v.max(*n as u32 + 1),
                Opnd::Sgpr(n) => sg = sg.max(*n as u32 + 1),
                _ => {}
            }
        }
        RegUse { vgprs: v, sgprs: sg, wave: super::super::mesh::wave_size(&insns), compute: ex.stage == crate::Stage::Compute }
    }

    fn vgpr_granule(wave: u32) -> u32 {
        if wave == 32 {
            8
        } else {
            4
        }
    }

    fn reserved_mask(u: &RegUse) -> u32 {
        RSRC1_RESERVED_ALL | RSRC1_ROUND | if u.compute { RSRC1_RESERVED_CS } else { 0 }
    }

    fn rsrc1_fits(w: u32, u: &RegUse) -> bool {
        let sg = (w >> 6) & 0xf;
        w & reserved_mask(u) == 0 && ((w & 0x3f) + 1) * vgpr_granule(u.wave) >= u.vgprs && (sg == 0 || (sg + 1) * 8 >= u.sgprs)
    }

    fn show_rsrc1(w: u32, u: &RegUse) -> String {
        format!(
            "{w:#010x} (VGPRS {} = {} regs at wave{}, SGPRS {}, FLOAT_MODE {:#04x}, DX10_CLAMP {}, IEEE_MODE {}, reserved {:#x}, fits {})",
            w & 0x3f,
            ((w & 0x3f) + 1) * vgpr_granule(u.wave),
            u.wave,
            (w >> 6) & 0xf,
            (w >> 12) & 0xff,
            (w >> 21) & 1,
            (w >> 23) & 1,
            w & reserved_mask(u),
            rsrc1_fits(w, u)
        )
    }

    fn rsrc1_candidates(ex: &crate::ShaderExtract, u: &RegUse) -> BTreeSet<usize> {
        let end = ex.isa_offset_in_fssm.saturating_sub(16).min(ex.fssm.len());
        (0..end / 4).map(|k| k * 4).filter(|&o| o != 0x10 && le32(&ex.fssm, o).is_some_and(|w| rsrc1_fits(w, u))).collect()
    }

    fn common(sets: &[BTreeSet<usize>]) -> BTreeSet<usize> {
        let mut it = sets.iter();
        let Some(first) = it.next() else { return BTreeSet::new() };
        it.fold(first.clone(), |a, s| a.intersection(s).copied().collect())
    }

    fn synth(fssm: Vec<u8>, isa_off: usize, words: &[u32], stage: crate::Stage) -> crate::ShaderExtract {
        let isa: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        crate::ShaderExtract {
            stage,
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
        b[0x10..0x14].copy_from_slice(&sub.to_le_bytes());
        b[at..at + 4].copy_from_slice(&w.to_le_bytes());
        b
    }

    const V20_S30: [u32; 3] = [0x7e00_0000 | (20 << 17) | (1 << 9) | 256, 0xbe80_0000 | (30 << 16) | (3 << 8) | 0x80, 0xbf81_0000];
    const V20_S30_W64: [u32; 4] = [0xbe80_0000 | (0x24 << 8) | 2, 0x7e00_0000 | (20 << 17) | (1 << 9) | 256, 0xbe80_0000 | (30 << 16) | (3 << 8) | 0x80, 0xbf81_0000];

    #[test]
    fn rsrc1_scan_checks_register_granules_and_reserved_bits_on_synthetic_containers() {
        let cs = crate::Stage::Compute;
        let u = reg_use(&synth(Vec::new(), 0, &V20_S30, cs));
        assert_eq!(u, RegUse { vgprs: 21, sgprs: 31, wave: 32, compute: true });
        let u64 = reg_use(&synth(Vec::new(), 0, &V20_S30_W64, cs));
        assert_eq!((u64.vgprs, u64.wave), (21, 64));
        assert!(rsrc1_fits(2, &u));
        assert!(!rsrc1_fits(1, &u));
        assert!(!rsrc1_fits(4, &u64));
        assert!(rsrc1_fits(5, &u64));
        assert!(rsrc1_fits(2 | (3 << 6), &u));
        assert!(!rsrc1_fits(2 | (2 << 6), &u));
        assert!(rsrc1_fits(2 | (0xf0 << 12) | (1 << 21) | (1 << 23) | (1 << 29) | (1 << 30) | (1 << 31), &u));
        assert!(!rsrc1_fits(2 | (1 << 12), &u));
        for bit in [10, 11, 20, 22, 24, 25, 27, 28] {
            assert!(!rsrc1_fits(2 | (1 << bit), &u), "bit {bit}");
        }
        let ps = RegUse { compute: false, ..u };
        assert!(rsrc1_fits(2 | (1 << 24) | (1 << 25), &ps));
        assert!(!rsrc1_fits(2 | (1 << 20), &ps));
        let w = 2 | (0xf0 << 12) | (1 << 23);
        let a = synth(header(0x180, 5, 0x12c, w), 0x190, &V20_S30, cs);
        let b = synth(header(0x280, 5, 0x12c, 3 | (0x30 << 12)), 0x290, &V20_S30, cs);
        let sets = [rsrc1_candidates(&a, &reg_use(&a)), rsrc1_candidates(&b, &reg_use(&b))];
        assert_eq!(common(&sets), BTreeSet::from([0x12c]));
        let c = synth(header(0x180, 5, 0x12c, 1 | (0xf0 << 12)), 0x190, &V20_S30, cs);
        assert!(rsrc1_candidates(&c, &reg_use(&c)).is_empty());
        let d = synth(header(0x190, 5, 0x184, w), 0x190, &V20_S30, cs);
        assert!(rsrc1_candidates(&d, &reg_use(&d)).is_empty());
        let s = show_rsrc1(w, &u);
        assert!(s.contains("VGPRS 2 = 24 regs at wave32") && s.contains("fits true"), "{s}");
    }

    const NO_TABLE: u64 = 0xdead_beef_dead_beef;
    const STAGE_SLOTS: std::ops::Range<usize> = 0x18..0x60;
    const USER_SGPRS_AT: usize = 0xd6;
    const RSRC2_SKIP: [(usize, usize); 3] = [(0x10, 0x14), (0xd6, 0xd7), (0xe8, 0x100)];
    const RSRC2_OF_257: [(u32, usize); 2] = [(5, 0x130), (6, 0x138)];
    const LDS_GRANULE_BYTES: u32 = 512;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum HwReg {
        Cs,
        Ps,
        Vs,
        Gs,
        Hs,
    }

    impl HwReg {
        fn must_be_zero(self) -> u32 {
            match self {
                HwReg::Cs => 0xff00_6040,
                HwReg::Ps => 0x01ff_0040,
                HwReg::Vs => 0x0680_0000 | 0x003f_e040,
                HwReg::Gs => 0x0000_ffc0,
                HwReg::Hs => 0x0003_fe40,
            }
        }

        fn has_user_sgpr_msb(self) -> bool {
            self != HwReg::Cs
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SgprMatch {
        Count,
        CountPlusSystem,
    }

    #[derive(Clone, Copy, Debug)]
    struct Rsrc2Ctx {
        reg: HwReg,
        user_sgprs: u32,
        system: u32,
        lds_reach: u32,
    }

    struct Fixture {
        name: String,
        sub: Option<u32>,
        stage: String,
        slots: Vec<(usize, u64)>,
        slot: Option<usize>,
        ctx: Option<Rsrc2Ctx>,
        u: RegUse,
        ex: crate::ShaderExtract,
        rsrc1: BTreeSet<usize>,
        rsrc2: BTreeMap<usize, SgprMatch>,
    }

    type Checks = Vec<(&'static str, bool)>;
    type Tally = BTreeMap<(Option<u32>, String, &'static str, usize), (u32, BTreeMap<&'static str, u32>)>;

    fn le16(b: &[u8], o: usize) -> Option<u16> {
        b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]))
    }

    fn le64(b: &[u8], o: usize) -> Option<u64> {
        b.get(o..o + 8).map(|x| u64::from_le_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]]))
    }

    fn lds_bytes(w: u32) -> u32 {
        ((w >> 15) & 0x1ff) * LDS_GRANULE_BYTES
    }

    fn user_sgpr_field(w: u32, reg: HwReg) -> u32 {
        ((w >> 1) & 0x1f) | if reg.has_user_sgpr_msb() { ((w >> 27) & 1) << 5 } else { 0 }
    }

    fn stage_slots(fssm: &[u8]) -> Vec<(usize, u64)> {
        STAGE_SLOTS.step_by(8).filter_map(|o| le64(fssm, o).filter(|&v| v != 0 && v != NO_TABLE).map(|v| (o, v))).collect()
    }

    fn merged_system_sgprs(slot: Option<usize>) -> u32 {
        if matches!(slot, Some(0x28 | 0x30 | 0x58)) {
            8
        } else {
            0
        }
    }

    fn hw_reg(stage: crate::Stage, slot: Option<usize>) -> HwReg {
        match (slot, stage) {
            (Some(0x40), _) => HwReg::Ps,
            (Some(0x38), _) => HwReg::Vs,
            (Some(0x28 | 0x30 | 0x58), _) => HwReg::Gs,
            (_, crate::Stage::Pixel) => HwReg::Ps,
            (_, crate::Stage::Vertex) => HwReg::Vs,
            (_, crate::Stage::Hull) => HwReg::Hs,
            (_, crate::Stage::Compute | crate::Stage::Amplification) => HwReg::Cs,
            _ => HwReg::Gs,
        }
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

    fn static_lds_reach(ex: &crate::ShaderExtract) -> u32 {
        crate::rdna2::disassemble(&ex.isa).iter().filter(|i| i.enc == Enc::Ds && !super::super::gds::is_gds(i)).filter_map(ds_reach).max().unwrap_or(0)
    }

    fn rsrc2_fits(w: u32, c: &Rsrc2Ctx) -> Option<SgprMatch> {
        if w & c.reg.must_be_zero() != 0 || (c.reg == HwReg::Cs && lds_bytes(w) < c.lds_reach) {
            return None;
        }
        let n = user_sgpr_field(w, c.reg);
        if n == c.user_sgprs {
            Some(SgprMatch::Count)
        } else if c.system != 0 && n == c.user_sgprs + c.system {
            Some(SgprMatch::CountPlusSystem)
        } else {
            None
        }
    }

    fn rsrc2_candidates(ex: &crate::ShaderExtract, c: &Rsrc2Ctx) -> BTreeMap<usize, SgprMatch> {
        let end = ex.isa_offset_in_fssm.saturating_sub(16).min(ex.fssm.len());
        (0..end / 4)
            .map(|k| k * 4)
            .filter(|&o| !RSRC2_SKIP.iter().any(|&(a, b)| o < b && o + 4 > a))
            .filter_map(|o| le32(&ex.fssm, o).and_then(|w| rsrc2_fits(w, c)).map(|m| (o, m)))
            .collect()
    }

    fn show_rsrc2(w: u32, c: &Rsrc2Ctx) -> String {
        format!(
            "{w:#010x} ({:?}: USER_SGPR {}, LDS_SIZE {} bytes, must-be-zero bits {:#x}, match {:?})",
            c.reg,
            user_sgpr_field(w, c.reg),
            lds_bytes(w),
            w & c.reg.must_be_zero(),
            rsrc2_fits(w, c)
        )
    }

    fn fixture(name: String, ex: crate::ShaderExtract) -> Fixture {
        let u = reg_use(&ex);
        let slots = stage_slots(&ex.fssm);
        let slot = if slots.len() == 1 { Some(slots[0].0) } else { None };
        let ctx = ex.fssm.get(USER_SGPRS_AT).map(|&b| Rsrc2Ctx {
            reg: hw_reg(ex.stage, slot),
            user_sgprs: b as u32,
            system: merged_system_sgprs(slot),
            lds_reach: static_lds_reach(&ex),
        });
        let rsrc1 = rsrc1_candidates(&ex, &u);
        let rsrc2 = ctx.map(|c| rsrc2_candidates(&ex, &c)).unwrap_or_default();
        Fixture { name, sub: le32(&ex.fssm, 0x10), stage: format!("{:?}", ex.stage), slots, slot, ctx, u, ex, rsrc1, rsrc2 }
    }

    fn group_common<K: Ord>(fx: &[Fixture], key: impl Fn(&Fixture) -> K, set: impl Fn(&Fixture) -> BTreeSet<usize>) -> BTreeMap<K, (usize, BTreeSet<usize>)> {
        let mut g: BTreeMap<K, Vec<BTreeSet<usize>>> = BTreeMap::new();
        for f in fx {
            g.entry(key(f)).or_default().push(set(f));
        }
        g.into_iter().map(|(k, v)| (k, (v.len(), common(&v)))).collect()
    }

    fn back_from_isa(f: &Fixture) -> BTreeSet<usize> {
        f.rsrc1.iter().map(|o| f.ex.isa_offset_in_fssm - o).collect()
    }

    fn rsrc2_probes(f: &Fixture, by_stage: &BTreeMap<(Option<u32>, String), (usize, BTreeSet<usize>)>) -> BTreeSet<usize> {
        let mut rs: BTreeSet<usize> = by_stage.get(&(f.sub, f.stage.clone())).map(|e| e.1.iter().take(8).copied().collect()).unwrap_or_default();
        if f.ctx.is_some_and(|c| c.reg == HwReg::Cs) {
            if let Some(&(_, at)) = RSRC2_OF_257.iter().find(|e| Some(e.0) == f.sub) {
                rs.insert(at);
            }
        }
        rs
    }

    fn rsrc2_checks(f: &Fixture, r: usize) -> Option<(Checks, String)> {
        let c = f.ctx?;
        let w = le32(&f.ex.fssm, r)?;
        let before = r.checked_sub(4).and_then(|o| le32(&f.ex.fssm, o));
        let checks = vec![
            ("R is an RSRC2 candidate of this fixture", f.rsrc2.contains_key(&r)),
            ("dword at R-4 fits RSRC1", before.is_some_and(|b| rsrc1_fits(b, &f.u))),
        ];
        let s = format!("RSRC2 at {r:#x} = {}; dword at R-4 = {}", show_rsrc2(w, &c), before.map_or("none".to_string(), |b| show_rsrc1(b, &f.u)));
        Some((checks, s))
    }

    fn amdhsa_checks(f: &Fixture, r: usize) -> Option<(Checks, String)> {
        let b = &f.ex.fssm;
        let base = r.checked_sub(52)?;
        let w2 = le32(b, r)?;
        let group = le32(b, base)?;
        let entry = le64(b, base + 16)? as i64;
        let target = base as i64 + entry;
        let isa = f.ex.isa_offset_in_fssm as i64;
        let reserved = b.get(base + 12..base + 16)?.iter().chain(b.get(base + 24..base + 44)?).all(|&x| x == 0);
        let rsrc3 = le32(b, base + 44)?;
        let rsrc1 = le32(b, base + 48)?;
        let props = le16(b, base + 56)?;
        let preload = le16(b, base + 58)?;
        let tail = le32(b, base + 60)?;
        let checks = vec![
            ("GROUP_SEGMENT_FIXED_SIZE rounds up to LDS_SIZE", (group as u64).div_ceil(LDS_GRANULE_BYTES as u64) * LDS_GRANULE_BYTES as u64 == lds_bytes(w2) as u64),
            ("GROUP_SEGMENT_FIXED_SIZE covers the static LDS reach", group >= f.ctx.map_or(0, |c| c.lds_reach)),
            ("KERNEL_CODE_ENTRY_BYTE_OFFSET reaches the ISA", target == isa),
            ("KERNEL_CODE_ENTRY_BYTE_OFFSET reaches the ISA preamble", target == isa - 16),
            ("reserved descriptor bytes are zero", reserved),
            ("COMPUTE_PGM_RSRC1 at R-4 fits", rsrc1_fits(rsrc1, &f.u)),
            ("ENABLE_WAVEFRONT_SIZE32 matches the wave size", ((props >> 10) & 1 == 1) == (f.u.wave == 32)),
            ("descriptor clear of the 0xe8..0x100 table of #289", !(base < 0x100 && base + 64 > 0xe8)),
        ];
        let s = format!(
            "AMDHSA descriptor at {base:#x}: GROUP_SEGMENT_FIXED_SIZE {group} bytes (LDS_SIZE {} bytes, static reach {} bytes), KERNEL_CODE_ENTRY_BYTE_OFFSET {entry:#x} -> {target:#x} (ISA at {isa:#x}, an entry of {:#x} would reach it), RSRC3 {rsrc3:#010x}, RSRC1 {}, KERNEL_CODE_PROPERTIES {props:#06x}, KERNARG_PRELOAD {preload:#06x}, last dword {tail:#010x}",
            lds_bytes(w2),
            f.ctx.map_or(0, |c| c.lds_reach),
            isa - base as i64,
            show_rsrc1(rsrc1, &f.u)
        );
        Some((checks, s))
    }

    fn code_region_checks(f: &Fixture, r: usize) -> Option<(Checks, String)> {
        let b = &f.ex.fssm;
        let off = le64(b, 0xc0)?;
        let size = le64(b, 0xc8)?;
        let start = off as i128 + 0x10;
        let isa = f.ex.isa_offset_in_fssm as i128;
        let before = usize::try_from(start - 4).ok().and_then(|o| le32(b, o));
        let after = [le32(b, r + 4), le32(b, r + 8), le32(b, r + 12)];
        let mut checks = vec![
            ("code region starts at R", start == r as i128),
            ("code region starts at the ISA", start == isa),
            ("code region starts at the ISA preamble", start == isa - 16),
            ("dword before the code region fits RSRC1", before.is_some_and(|w| rsrc1_fits(w, &f.u))),
        ];
        if f.ctx.is_some_and(|c| c.reg == HwReg::Cs) {
            checks.push(("three dwords after R are the group size", f.ex.psv_numthreads.is_some_and(|t| after == t.map(Some))));
        }
        let s = format!(
            "code region: u64 at 0xc0 {off:#x}, at 0xc8 {size:#x}, starts at {start:#x} (R {:+}, ISA {:+}), ends at {:#x} (FSSM {:#x} bytes, ISA ends at {:#x}); dword before it {}; three dwords after R [{}], group size {:?}",
            start - r as i128,
            start - isa,
            start + size as i128,
            b.len(),
            f.ex.isa_offset_in_fssm + f.ex.isa.len(),
            before.map_or("none".to_string(), |w| show_rsrc1(w, &f.u)),
            after.iter().map(|w| w.map_or("none".to_string(), |w| format!("{w:#x}"))).collect::<Vec<_>>().join(", "),
            f.ex.psv_numthreads
        );
        Some((checks, s))
    }

    fn hx(v: Option<u32>) -> String {
        v.map_or("none".to_string(), |w| format!("{w:#x}"))
    }

    fn hs<'a>(it: impl IntoIterator<Item = &'a usize>) -> String {
        format!("[{}]", it.into_iter().map(|o| format!("{o:#x}")).collect::<Vec<_>>().join(", "))
    }

    fn check(c: &Checks, n: &str) -> bool {
        c.iter().any(|&(k, ok)| k == n && ok)
    }

    fn tally(t: &mut Tally, key: (Option<u32>, String, &'static str, usize), checks: &Checks) {
        let e = t.entry(key).or_default();
        e.0 += 1;
        for &(n, ok) in checks {
            *e.1.entry(n).or_default() += ok as u32;
        }
    }

    fn csfx(sub: u32, user_sgprs: u8, isa_off: usize, words: &[(usize, u32)]) -> Vec<u8> {
        let mut b = vec![0xffu8; isa_off];
        b[0x10..0x14].copy_from_slice(&sub.to_le_bytes());
        for o in STAGE_SLOTS.step_by(8) {
            b[o..o + 8].copy_from_slice(&NO_TABLE.to_le_bytes());
        }
        b[USER_SGPRS_AT] = user_sgprs;
        for &(o, w) in words {
            b[o..o + 4].copy_from_slice(&w.to_le_bytes());
        }
        b
    }

    #[test]
    fn rsrc2_scan_matches_user_sgprs_and_must_be_zero_bits_and_groups_by_stage() {
        assert_eq!(user_sgpr_field(9 << 1, HwReg::Cs), 9);
        assert_eq!(user_sgpr_field((1 << 1) | (1 << 27), HwReg::Gs), 33);
        assert_eq!(user_sgpr_field((1 << 1) | (1 << 27), HwReg::Cs), 1);
        let cs = Rsrc2Ctx { reg: HwReg::Cs, user_sgprs: 9, system: 0, lds_reach: 1024 };
        let w = (9 << 1) | (7 << 7) | (2 << 11) | (2 << 15);
        assert_eq!(rsrc2_fits(w, &cs), Some(SgprMatch::Count));
        assert_eq!(rsrc2_fits((w & !(0x1ff << 15)) | (1 << 15), &cs), None);
        assert_eq!(rsrc2_fits(w | (1 << 31), &cs), None);
        assert_eq!(rsrc2_fits(w | (1 << 6), &cs), None);
        assert_eq!(rsrc2_fits((w & !0x3e) | (8 << 1), &cs), None);
        let gs = Rsrc2Ctx { reg: HwReg::Gs, user_sgprs: 1, system: 8, lds_reach: 0 };
        assert_eq!(rsrc2_fits(1 << 1, &gs), Some(SgprMatch::Count));
        assert_eq!(rsrc2_fits(9 << 1, &gs), Some(SgprMatch::CountPlusSystem));
        assert_eq!(rsrc2_fits(10 << 1, &gs), None);
        let gs33 = Rsrc2Ctx { user_sgprs: 33, system: 0, ..gs };
        assert_eq!(rsrc2_fits((1 << 1) | (1 << 27), &gs33), Some(SgprMatch::Count));
        assert_eq!(rsrc2_fits(1 << 1, &gs33), None);
        let vs = Rsrc2Ctx { reg: HwReg::Vs, user_sgprs: 4, system: 0, lds_reach: 0 };
        assert_eq!(rsrc2_fits(4 << 1, &vs), Some(SgprMatch::Count));
        assert_eq!(rsrc2_fits((4 << 1) | (1 << 23), &vs), None);
        let ps = Rsrc2Ctx { reg: HwReg::Ps, ..vs };
        assert_eq!(rsrc2_fits((4 << 1) | (0xf << 28), &ps), Some(SgprMatch::Count));
        assert_eq!(rsrc2_fits((4 << 1) | (1 << 16), &ps), None);
        assert_eq!(hw_reg(crate::Stage::Vertex, Some(0x28)), HwReg::Gs);
        assert_eq!(hw_reg(crate::Stage::Vertex, None), HwReg::Vs);
        assert_eq!(hw_reg(crate::Stage::Compute, None), HwReg::Cs);

        let r1 = |v: u32| 2 | (v << 12);
        let c = crate::Stage::Compute;
        let a = fixture("a".into(), synth(csfx(6, 9, 0x150, &[(0x134, r1(0xf0)), (0x138, 9 << 1)]), 0x150, &V20_S30, c));
        let b = fixture("b".into(), synth(csfx(6, 9, 0x150, &[(0x134, r1(0x30)), (0x138, (9 << 1) | (7 << 7))]), 0x150, &V20_S30, c));
        let mut pfssm = csfx(6, 9, 0x160, &[(0x140, 9 << 1)]);
        pfssm[0x40..0x48].copy_from_slice(&0x200u64.to_le_bytes());
        let p = fixture("p".into(), synth(pfssm, 0x160, &V20_S30, crate::Stage::Pixel));
        assert_eq!(p.slot, Some(0x40));
        assert_eq!(p.ctx.map(|c| c.reg), Some(HwReg::Ps));
        assert_eq!(a.rsrc2.keys().copied().collect::<Vec<_>>(), vec![0x138]);
        assert_eq!(p.rsrc2.keys().copied().collect::<Vec<_>>(), vec![0x140]);
        let fx = [a, b, p];
        let by_stage = group_common(&fx, |f| (f.sub, f.stage.clone()), |f| f.rsrc2.keys().copied().collect());
        assert_eq!(by_stage[&(Some(6), "Compute".to_string())], (2, BTreeSet::from([0x138])));
        assert_eq!(by_stage[&(Some(6), "Pixel".to_string())], (1, BTreeSet::from([0x140])));
        let by_sub = group_common(&fx, |f| f.sub, |f| f.rsrc2.keys().copied().collect());
        assert!(by_sub[&Some(6)].1.is_empty());
        let rsrc1_by_stage = group_common(&fx, |f| (f.sub, f.stage.clone()), |f| f.rsrc1.clone());
        assert!(rsrc1_by_stage[&(Some(6), "Compute".to_string())].1.contains(&0x134));
        let by_slot = group_common(&fx, |f| (f.sub, f.slot), |f| f.rsrc2.keys().copied().collect());
        assert_eq!(by_slot[&(Some(6), Some(0x40))].1, BTreeSet::from([0x140]));
        assert_eq!(rsrc2_probes(&fx[0], &by_stage), BTreeSet::from([0x138]));
        let (rc, _) = rsrc2_checks(&fx[0], 0x138).unwrap();
        assert!(check(&rc, "R is an RSRC2 candidate of this fixture") && check(&rc, "dword at R-4 fits RSRC1"));

        let other = fixture("c".into(), synth(csfx(6, 8, 0x150, &[(0x134, r1(0xf0)), (0x138, 9 << 1)]), 0x150, &V20_S30, c));
        assert!(!other.rsrc2.contains_key(&0x138));
        let lds = fixture("d".into(), synth(csfx(6, 9, 0x150, &[(0x138, 9 << 1)]), 0x150, &[0xd834_0000 | 0x100, 0, 0xbf81_0000], c));
        assert_eq!(lds.ctx.map(|c| c.lds_reach), Some(0x104));
        assert!(!lds.rsrc2.contains_key(&0x138));
    }

    #[test]
    fn amdhsa_descriptor_and_code_region_probes_on_synthetic_containers() {
        let c = crate::Stage::Compute;
        let w2 = (9 << 1) | (7 << 7) | (2 << 15);
        let mut f = csfx(6, 9, 0x150, &[(0x104, 1000), (0x108, 0), (0x10c, 0), (0x110, 0), (0x130, 0), (0x134, 2 | (0xf0 << 12)), (0x138, w2), (0x13c, 1 << 10), (0x140, 0)]);
        f[0x114..0x11c].copy_from_slice(&0x4cu64.to_le_bytes());
        f[0x11c..0x130].fill(0);
        let hsa = fixture("hsa".into(), synth(f, 0x150, &V20_S30, c));
        let (checks, s) = amdhsa_checks(&hsa, 0x138).unwrap();
        assert!(checks.iter().all(|e| e.1 != (e.0 == "KERNEL_CODE_ENTRY_BYTE_OFFSET reaches the ISA preamble")), "{checks:?}\n{s}");
        assert!(s.contains("GROUP_SEGMENT_FIXED_SIZE 1000 bytes (LDS_SIZE 1024 bytes") && s.contains("OFFSET 0x4c -> 0x150"), "{s}");
        let (checks, _) = amdhsa_checks(&hsa, 0x130).unwrap();
        assert!(!check(&checks, "descriptor clear of the 0xe8..0x100 table of #289"));
        let (cr, _) = code_region_checks(&hsa, 0x138).unwrap();
        assert!(!check(&cr, "code region starts at R") && !check(&cr, "three dwords after R are the group size"));

        let mut g = csfx(6, 9, 0x160, &[(0x134, 2), (0x138, w2), (0x13c, 64), (0x140, 1), (0x144, 1)]);
        g[0xc0..0xc8].copy_from_slice(&0x128u64.to_le_bytes());
        g[0xc8..0xd0].copy_from_slice(&0x40u64.to_le_bytes());
        let mut ex = synth(g, 0x160, &V20_S30, c);
        ex.psv_numthreads = Some([64, 1, 1]);
        let region = fixture("region".into(), ex);
        let (cr, s) = code_region_checks(&region, 0x138).unwrap();
        assert!(check(&cr, "code region starts at R") && check(&cr, "three dwords after R are the group size") && check(&cr, "dword before the code region fits RSRC1"), "{cr:?}\n{s}");
        assert!(!check(&cr, "code region starts at the ISA"));
        let (hc, _) = amdhsa_checks(&region, 0x138).unwrap();
        assert!(!check(&hc, "reserved descriptor bytes are zero"));

        let mut gfssm = csfx(6, 1, 0x160, &[(0x140, 9 << 1)]);
        gfssm[0x28..0x30].copy_from_slice(&0x180u64.to_le_bytes());
        let gs = fixture("gs".into(), synth(gfssm, 0x160, &V20_S30, crate::Stage::Vertex));
        assert_eq!(gs.ctx.map(|c| (c.reg, c.system)), Some((HwReg::Gs, 8)));
        assert_eq!(gs.rsrc2.get(&0x140), Some(&SgprMatch::CountPlusSystem));
        let by_sub = summarize(&[hsa, region, gs], &BTreeMap::new());
        assert_eq!(by_sub[&Some(6)].0, 3);
    }

    fn summarize(fx: &[Fixture], adjacent: &BTreeMap<Option<u32>, (usize, usize)>) -> BTreeMap<Option<u32>, (usize, BTreeSet<usize>)> {
        let by_sub = group_common(fx, |f| f.sub, |f| f.rsrc1.clone());
        let back_by_sub = group_common(fx, |f| f.sub, back_from_isa);
        let r1_stage = group_common(fx, |f| (f.sub, f.stage.clone()), |f| f.rsrc1.clone());
        let r1_back_stage = group_common(fx, |f| (f.sub, f.stage.clone()), back_from_isa);
        let r1_slot = group_common(fx, |f| (f.sub, f.slot), |f| f.rsrc1.clone());
        let r2_stage = group_common(fx, |f| (f.sub, f.stage.clone()), |f| f.rsrc2.keys().copied().collect());
        let r2_slot = group_common(fx, |f| (f.sub, f.slot), |f| f.rsrc2.keys().copied().collect());
        let mut tallies: Tally = BTreeMap::new();
        let mut slot_diffs: BTreeMap<(Option<u32>, usize, &'static str, usize), BTreeSet<i128>> = BTreeMap::new();
        for x in fx {
            let probes = rsrc2_probes(x, &r2_stage);
            eprintln!("{}: {} sub-version {}, probing RSRC2 at {}", x.name, x.stage, hx(x.sub), hs(&probes));
            for &r in &probes {
                for (kind, got) in [("RSRC2", rsrc2_checks(x, r)), ("code region", code_region_checks(x, r))]
                    .into_iter()
                    .chain(x.ctx.filter(|c| c.reg == HwReg::Cs).map(|_| ("AMDHSA", amdhsa_checks(x, r))))
                {
                    if let Some((checks, s)) = got {
                        eprintln!("  {s}");
                        tally(&mut tallies, (x.sub, x.stage.clone(), kind, r), &checks);
                    }
                }
            }
            for &(so, v) in &x.slots {
                let r1c = r1_stage.get(&(x.sub, x.stage.clone())).map(|e| e.1.clone()).unwrap_or_default();
                let d = |o: usize| v as i128 - o as i128;
                eprintln!(
                    "  stage slot {so:#x} = {v:#x}: minus ISA offset {:+}; minus RSRC2 probes [{}]; minus common RSRC1 offsets [{}]",
                    d(x.ex.isa_offset_in_fssm),
                    probes.iter().map(|&r| format!("{r:#x}: {:+}", d(r))).collect::<Vec<_>>().join(", "),
                    r1c.iter().take(8).map(|&o| format!("{o:#x}: {:+}", d(o))).collect::<Vec<_>>().join(", ")
                );
                if x.slots.len() == 1 {
                    for &r in &probes {
                        slot_diffs.entry((x.sub, so, "RSRC2", r)).or_default().insert(d(r));
                    }
                    for &o in r1c.iter().take(8) {
                        slot_diffs.entry((x.sub, so, "RSRC1", o)).or_default().insert(d(o));
                    }
                    slot_diffs.entry((x.sub, so, "ISA", 0)).or_default().insert(d(x.ex.isa_offset_in_fssm));
                }
            }
        }
        for (sub, (n, set)) in &by_sub {
            let (an, fit) = adjacent.get(sub).copied().unwrap_or_default();
            eprintln!(
                "FSSM sub-version {}: {n} fixtures; RSRC1 offsets that fit all of those: fixed {}, counted back from the ISA {}; the dword before COMPUTE_PGM_RSRC2 of #257 fits in {fit} of {an}",
                hx(*sub),
                hs(set),
                hs(&back_by_sub[sub].1)
            );
        }
        for (k, (n, set)) in &r1_stage {
            eprintln!(
                "FSSM sub-version {}, stage {}: {n} fixtures; RSRC1 offsets that fit all: fixed {}, counted back from the ISA {}; RSRC2 offsets that fit all: {}",
                hx(k.0),
                k.1,
                hs(set),
                hs(&r1_back_stage[k].1),
                hs(&r2_stage[k].1)
            );
        }
        for (k, (n, set)) in &r1_slot {
            eprintln!("FSSM sub-version {}, stage slot {}: {n} fixtures; RSRC1 offsets that fit all {}; RSRC2 offsets that fit all {}", hx(k.0), k.1.map_or("none".to_string(), |o| format!("{o:#x}")), hs(set), hs(&r2_slot[k].1));
        }
        for ((sub, so, what, o), d) in &slot_diffs {
            eprintln!(
                "FSSM sub-version {}, stage slot {so:#x}: slot value minus {what} {o:#x} takes {} distinct value(s) {:?}",
                hx(*sub),
                d.len(),
                d.iter().take(8).collect::<Vec<_>>()
            );
        }
        for ((sub, stage, kind, r), (n, counts)) in &tallies {
            eprintln!("FSSM sub-version {}, stage {stage}, {kind} hypothesis at R = {r:#x}: {n} fixtures probed", hx(*sub));
            for (c, k) in counts {
                eprintln!("    {k} of {n}: {c}");
            }
        }
        by_sub
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
    fn pgm_rsrc1_candidates_in_every_fssm_fixture() {
        let mut files = Vec::new();
        dxbc_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../analysis/shaders"), &mut files);
        dxbc_files(&std::env::var_os("TOMBOX_SHADER_DUMPS").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("tombox_shaders")), &mut files);
        files.sort();
        let (mut fssm_n, mut durango_n) = (0, 0);
        let mut bad = Vec::new();
        let mut adjacent: BTreeMap<Option<u32>, (usize, usize)> = Default::default();
        let (mut dfixed, mut dback) = (Vec::new(), Vec::new());
        let mut fx: Vec<Fixture> = Vec::new();
        for f in &files {
            let Ok(bytes) = std::fs::read(f) else { continue };
            let Ok(ex) = crate::extract(&bytes) else { continue };
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let durango = crate::gcn::is_durango(&ex.fssm);
            let x = fixture(name, ex);
            let (ex, u, sub, name) = (&x.ex, &x.u, x.sub, &x.name);
            let rel = back_from_isa(&x);
            let adj = RSRC2_ADJACENT.iter().find(|e| Some(e.0) == sub).map(|e| e.1);
            let adj_w = adj.filter(|o| o + 4 <= ex.isa_offset_in_fssm.saturating_sub(16)).and_then(|o| le32(&ex.fssm, o));
            eprintln!(
                "{name}: {} {:?}, sub-version at 0x10 {}, ISA at {:#x}, {} VGPRs and {} SGPRs used (lower bound), wave{}, reader {}, {} dwords before the ISA fit",
                if durango { "Durango" } else { "FSSM" },
                ex.stage,
                sub.map_or("none".to_string(), |w| format!("{w:#x}")),
                ex.isa_offset_in_fssm,
                u.vgprs,
                u.sgprs,
                u.wave,
                container_pgm_rsrc1(ex).map_or("none".to_string(), |w| format!("{w:#010x}")),
                x.rsrc1.len()
            );
            if let (Some(o), Some(w)) = (adj, adj_w) {
                eprintln!("  dword at {o:#x}, 4 bytes before COMPUTE_PGM_RSRC2 of #257: {}", show_rsrc1(w, u));
            }
            eprintln!("  fixed offsets {}", hs(x.rsrc1.iter().take(32)));
            eprintln!("  counted back from the ISA {}", hs(rel.iter().take(32)));
            if durango {
                durango_n += 1;
                if container_pgm_rsrc1(ex).is_some() {
                    bad.push(format!("{name}: a Durango container is read while no Durango location is proven"));
                }
                dfixed.push(x.rsrc1.clone());
                dback.push(rel);
                continue;
            }
            fssm_n += 1;
            if let Some(w) = container_pgm_rsrc1(ex) {
                if !rsrc1_fits(w, u) {
                    bad.push(format!("{name}: sub-version {sub:#x?}, the reader's dword is {}", show_rsrc1(w, u)));
                }
            }
            let e = adjacent.entry(sub).or_default();
            e.0 += 1;
            if adj_w.is_some_and(|w| rsrc1_fits(w, u)) {
                e.1 += 1;
            }
            eprintln!(
                "  byte 0xd6 (user SGPRs, #289) {:?}, u64 at 0xe8 {}, stage slots {:x?}, RSRC2 register {:?}, static LDS reach {} bytes",
                x.ctx.map(|c| c.user_sgprs),
                le64(&ex.fssm, 0xe8).map_or("none".to_string(), |p| if p == NO_TABLE { "no table".to_string() } else { format!("{p:#x}") }),
                x.slots,
                x.ctx.map(|c| (c.reg, c.system)),
                x.ctx.map_or(0, |c| c.lds_reach)
            );
            eprintln!("  RSRC2 candidates {}", x.rsrc2.iter().take(32).map(|(o, m)| format!("{o:#x} {m:?}")).collect::<Vec<_>>().join(", "));
            fx.push(x);
        }
        let by_sub = summarize(&fx, &adjacent);
        eprintln!("FSSM: {fssm_n} fixtures; Durango: {durango_n} fixtures, offsets that fit all of those: fixed {}, counted back from the code {}", hs(&common(&dfixed)), hs(&common(&dback)));
        assert!(bad.is_empty(), "PGM_RSRC1 is not where the reader looks:\n{}", bad.join("\n"));
        assert!(fssm_n >= 2, "not proven: {fssm_n} FSSM fixtures; at least two are needed");
        for (sub, (_, set)) in &by_sub {
            if let Some(&(_, at)) = PGM_RSRC1_AT.iter().find(|e| Some(e.0) == *sub) {
                assert!(set.contains(&at), "sub-version {sub:#x?}: the reader's offset {at:#x} is not among the offsets that fit every fixture");
            }
        }
    }
}
