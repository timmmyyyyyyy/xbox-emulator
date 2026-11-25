use super::fp::*;
use super::*;

type F2 = fn(u32, u32) -> u32;
type F3 = fn(u32, u32, u32) -> u32;
type G2 = fn(u32, u32) -> Option<u32>;
type G3 = fn(u32, u32, u32) -> Option<u32>;

fn s24(x: u32) -> i64 {
    ((x << 8) as i32 >> 8) as i64
}

pub fn mods32(x: u32, abs: bool, neg: bool) -> u32 {
    let x = if abs { x & 0x7fff_ffff } else { x };
    if neg { x ^ 0x8000_0000 } else { x }
}

pub fn mods16(x: u32, abs: bool, neg: bool) -> u32 {
    let x = x & 0xffff;
    let x = if abs { x & 0x7fff } else { x };
    if neg { x ^ 0x8000 } else { x }
}

pub fn out32(r: u32, clamp: bool, omod: u32) -> Option<u32> {
    let mut r = r;
    if omod != 0 && !nan32(r) {
        let k = [1.0f32, 2.0, 4.0, 0.5][omod as usize];
        if r & 0x7fff_ffff < 0x0180_0000 || r & 0x7fff_ffff >= 0x7e80_0000 {
            return None;
        }
        r = (f(r) * k).to_bits();
        if den32(r) {
            return None;
        }
    }
    if clamp {
        if nan32(r) {
            return None;
        }
        let x = f(r);
        if x == 0.0 && r != 0 {
            return None;
        }
        r = if x <= 0.0 { 0 } else if x >= 1.0 { 0x3f80_0000 } else { r };
    }
    Some(r)
}

pub fn out16(r: u32, clamp: bool) -> Option<u32> {
    let r = r & 0xffff;
    if clamp {
        if nan16(r) {
            return None;
        }
        let x = h2f(r);
        if x == 0.0 && r != 0 {
            return None;
        }
        return Some(if x <= 0.0 { 0 } else if x >= 1.0 { 0x3c00 } else { r });
    }
    Some(r)
}

fn fadd(a: u32, b: u32) -> Option<u32> {
    Some(f32q(f(a) + f(b)))
}
fn fsub(a: u32, b: u32) -> Option<u32> {
    Some(f32q(f(a) - f(b)))
}
fn fsubr(a: u32, b: u32) -> Option<u32> {
    Some(f32q(f(b) - f(a)))
}
fn fmul(a: u32, b: u32) -> Option<u32> {
    Some(f32q(f(a) * f(b)))
}
fn lmul(a: u32, b: u32) -> u32 {
    if a & 0x7fff_ffff == 0 || b & 0x7fff_ffff == 0 { 0 } else { f32q(f(a) * f(b)) }
}
fn fmul_legacy(a: u32, b: u32) -> Option<u32> {
    Some(lmul(a, b))
}
fn unfused(a: u32, b: u32, c: u32, legacy: bool) -> Option<u32> {
    if den32(a) || den32(b) || den32(c) {
        return None;
    }
    let p = if legacy { lmul(a, b) } else { f32q(f(a) * f(b)) };
    if den32(p) {
        return None;
    }
    let r = f32q(f(p) + f(c));
    if den32(r) {
        return None;
    }
    Some(r)
}
fn fma32(a: u32, b: u32, c: u32) -> Option<u32> {
    Some(f32q(f(a).mul_add(f(b), f(c))))
}
fn fmin(a: u32, b: u32) -> Option<u32> {
    min_num32(a, b)
}
fn fmax(a: u32, b: u32) -> Option<u32> {
    max_num32(a, b)
}
fn pkrtz(a: u32, b: u32) -> Option<u32> {
    Some(f2h_rtz(f(a)) | (f2h_rtz(f(b)) << 16))
}

fn h(a: u32) -> f64 {
    h2d(a & 0xffff)
}
fn hadd(a: u32, b: u32) -> Option<u32> {
    Some(d2h(h(a) + h(b)))
}
fn hsub(a: u32, b: u32) -> Option<u32> {
    Some(d2h(h(a) - h(b)))
}
fn hsubr(a: u32, b: u32) -> Option<u32> {
    Some(d2h(h(b) - h(a)))
}
fn hmul(a: u32, b: u32) -> Option<u32> {
    Some(d2h(h(a) * h(b)))
}
fn hmin(a: u32, b: u32) -> Option<u32> {
    min_num16(a, b)
}
fn hmax(a: u32, b: u32) -> Option<u32> {
    max_num16(a, b)
}
fn hldexp(a: u32, b: u32) -> Option<u32> {
    let e = (b as u16 as i16) as i32;
    if nan16(a) {
        return Some(QNAN16);
    }
    Some(d2h(ldexp64(h(a), e)))
}
pub fn hfma(a: u32, b: u32, c: u32) -> Option<u32> {
    let p = h(a) * h(b) + h(c);
    if (h(a) * h(b)).is_finite() && (p - h(a) * h(b) - h(c)) != 0.0 {
        return None;
    }
    Some(d2h(p))
}

fn i2(s: &mut Suite, m: &'static str, op: u32, class: Class, f: F2) {
    s.add(m, "VOP2", "e32 v,v", class, &[], &[vop2(op, OUT, v(10), 11)], &[], &[Cmp::X], move |x| o(f(x.d[0], x.d[1])));
    s.add(m, "VOP2", "e32 s,v", class, &[], &[vop2(op, OUT, 12, 11)], &[], &[Cmp::X], move |x| o(f(x.d[2], x.d[1])));
    s.add(m, "VOP2", "e64 v,s", class, &[], &vop3(0x100 + op, OUT, [v(10), 13, 0]), &[], &[Cmp::X], move |x| o(f(x.d[0], x.d[3])));
}

fn f2v(s: &mut Suite, m: &'static str, op: u32, class: Class, g: G2) {
    s.add(m, "VOP2", "e32", class, &[], &[vop2(op, OUT, v(10), 11)], &[], &[Cmp::F32(0)], move |x| on(g(x.d[0], x.d[1])));
    s.add(m, "VOP2", "e32 s,v", class, &[], &[vop2(op, OUT, 12, 11)], &[], &[Cmp::F32(0)], move |x| on(g(x.d[2], x.d[1])));
    for (name, abs, neg, clamp, omod) in [("e64 neg0 abs1", 2u32, 1u32, false, 0u32), ("e64 abs0 neg1", 1, 2, false, 0), ("e64 clamp", 0, 0, true, 0), ("e64 omod*2", 0, 0, false, 1), ("e64 omod/2 neg01", 0, 3, false, 3), ("e64 omod*4 clamp", 0, 0, true, 2)] {
        s.add(m, "VOP2", name, class, &[], &vop3o(0x100 + op, OUT, [v(10), v(11), 0], abs, neg, clamp, omod), &[], &[Cmp::F32(0)], move |x| {
            let a = mods32(x.d[0], abs & 1 != 0, neg & 1 != 0);
            let b = mods32(x.d[1], abs & 2 != 0, neg & 2 != 0);
            on(g(a, b).and_then(|r| out32(r, clamp, omod)))
        });
    }
}

fn h2v(s: &mut Suite, m: &'static str, op: u32, g: G2) {
    s.add(m, "VOP2", "e32", Class::F16, &[vmov(OUT, v(12))], &[vop2(op, OUT, v(10), 11)], &[], &[Cmp::F16(0)], move |x| on(g(x.d[0], x.d[1]).map(|r| (x.d[2] & 0xffff_0000) | (r & 0xffff))));
    for (name, opsel, abs, neg, clamp) in [("e64 opsel=src0hi,src1hi", 0b0011u32, 0u32, 0u32, false), ("e64 opsel=dsthi neg0", 0b1000, 0, 1, false), ("e64 opsel=src1hi abs0 clamp", 0b0010, 1, 0, true), ("e64 opsel=src0hi,dsthi neg1", 0b1001, 0, 2, false)] {
        let c = if opsel & 8 != 0 { Cmp::F16H(0) } else { Cmp::F16(0) };
        s.add(m, "VOP2", name, Class::F16, &[vmov(OUT, v(12))], &vop3m(0x100 + op, OUT, [v(10), v(11), 0], abs, neg, opsel, clamp), &[], &[c], move |x| {
            let a = mods16(if opsel & 1 != 0 { x.d[0] >> 16 } else { x.d[0] }, abs & 1 != 0, neg & 1 != 0);
            let b = mods16(if opsel & 2 != 0 { x.d[1] >> 16 } else { x.d[1] }, abs & 2 != 0, neg & 2 != 0);
            on(g(a, b).and_then(|r| out16(r, clamp)).map(|r| if opsel & 8 != 0 { (x.d[2] & 0xffff) | (r << 16) } else { (x.d[2] & 0xffff_0000) | r }))
        });
    }
}

fn vop2_ops(s: &mut Suite) {
    let ints: &[(&'static str, u32, Class, F2)] = &[
        ("v_mul_i32_i24", 0x09, Class::U32, |a, b| (s24(a) * s24(b)) as u32),
        ("v_mul_hi_i32_i24", 0x0a, Class::U32, |a, b| ((s24(a) * s24(b)) >> 32) as u32),
        ("v_mul_u32_u24", 0x0b, Class::U32, |a, b| ((a as u64 & 0xff_ffff) * (b as u64 & 0xff_ffff)) as u32),
        ("v_mul_hi_u32_u24", 0x0c, Class::U32, |a, b| (((a as u64 & 0xff_ffff) * (b as u64 & 0xff_ffff)) >> 32) as u32),
        ("v_min_i32", 0x11, Class::U32, |a, b| (a as i32).min(b as i32) as u32),
        ("v_max_i32", 0x12, Class::U32, |a, b| (a as i32).max(b as i32) as u32),
        ("v_min_u32", 0x13, Class::U32, |a, b| a.min(b)),
        ("v_max_u32", 0x14, Class::U32, |a, b| a.max(b)),
        ("v_lshrrev_b32", 0x16, Class::Small, |a, b| b >> (a & 31)),
        ("v_ashrrev_i32", 0x18, Class::Small, |a, b| ((b as i32) >> (a & 31)) as u32),
        ("v_lshlrev_b32", 0x1a, Class::Small, |a, b| b << (a & 31)),
        ("v_and_b32", 0x1b, Class::U32, |a, b| a & b),
        ("v_or_b32", 0x1c, Class::U32, |a, b| a | b),
        ("v_xor_b32", 0x1d, Class::U32, |a, b| a ^ b),
        ("v_xnor_b32", 0x1e, Class::U32, |a, b| !(a ^ b)),
        ("v_add_nc_u32", 0x25, Class::U32, |a, b| a.wrapping_add(b)),
        ("v_sub_nc_u32", 0x26, Class::U32, |a, b| a.wrapping_sub(b)),
        ("v_subrev_nc_u32", 0x27, Class::U32, |a, b| b.wrapping_sub(a)),
    ];
    for &(m, op, c, f) in ints {
        i2(s, m, op, c, f);
    }
    s.add("v_add_nc_u32", "VOP2", "e64 clamp", Class::U32, &[], &vop3m(0x125, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o(x.d[0].saturating_add(x.d[1])));
    s.add("v_sub_nc_u32", "VOP2", "e64 clamp", Class::U32, &[], &vop3m(0x126, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o(x.d[0].saturating_sub(x.d[1])));
    s.add("v_subrev_nc_u32", "VOP2", "e64 clamp", Class::U32, &[], &vop3m(0x127, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o(x.d[1].saturating_sub(x.d[0])));
    s.add("v_cndmask_b32", "VOP2", "e32 vcc", Class::U32, &vcc_from(12), &[vop2(0x01, OUT, v(10), 11)], &[], &[Cmp::X], |x| o(if x.d[2] & 1 != 0 { x.d[1] } else { x.d[0] }));
    s.add("v_cndmask_b32", "VOP2", "e32 s0=sgpr", Class::U32, &vcc_from(12), &[vop2(0x01, OUT, 13, 11)], &[], &[Cmp::X], |x| o(if x.d[2] & 1 != 0 { x.d[1] } else { x.d[3] }));
    s.add("v_cndmask_b32", "VOP2", "e64 sgpr mask", Class::U32, &mask_from(42, 12), &vop3(0x101, OUT, [v(10), v(11), 42]), &[], &[Cmp::X], |x| o(if x.d[2] & 1 != 0 { x.d[1] } else { x.d[0] }));
    s.add("v_cndmask_b32", "VOP2", "e64 abs neg f32", Class::F32, &mask_from(42, 12), &vop3o(0x101, OUT, [v(10), v(11), 42], 1, 2, false, 0), &[], &[Cmp::X], |x| o(if x.d[2] & 1 != 0 { x.d[1] ^ 0x8000_0000 } else { x.d[0] & 0x7fff_ffff }));
    let carries: [(&'static str, u32, fn(u32, u32, u32) -> (u32, u32)); 3] = [
        ("v_add_co_ci_u32", 0x28, |a, b, c| {
            let r = a as u64 + b as u64 + c as u64;
            (r as u32, (r >> 32) as u32)
        }),
        ("v_sub_co_ci_u32", 0x29, |a, b, c| (a.wrapping_sub(b).wrapping_sub(c), ((b as u64 + c as u64) > a as u64) as u32)),
        ("v_subrev_co_ci_u32", 0x2a, |a, b, c| (b.wrapping_sub(a).wrapping_sub(c), ((a as u64 + c as u64) > b as u64) as u32)),
    ];
    for (m, op, f) in carries {
        s.add(m, "VOP2", "e32 vcc", Class::U32, &vcc_from(12), &[vop2(op, OUT, v(10), 11)], &vcc_to(OUT + 1), &[Cmp::X, Cmp::X], move |x| {
            let (r, c) = f(x.d[0], x.d[1], x.d[2] & 1);
            o2(r, c)
        });
        s.add(m, "VOP2", "e64 sgpr carry", Class::U32, &mask_from(44, 12), &vop3b(0x100 + op, OUT, 42, [v(10), v(11), 44]), &mask_to(OUT + 1, 42), &[Cmp::X, Cmp::X], move |x| {
            let (r, c) = f(x.d[0], x.d[1], x.d[2] & 1);
            o2(r, c)
        });
    }
    let fl: &[(&'static str, u32, G2)] = &[("v_add_f32", 0x03, fadd), ("v_sub_f32", 0x04, fsub), ("v_subrev_f32", 0x05, fsubr), ("v_mul_legacy_f32", 0x07, fmul_legacy), ("v_mul_f32", 0x08, fmul), ("v_min_f32", 0x0f, fmin), ("v_max_f32", 0x10, fmax)];
    for &(m, op, g) in fl {
        f2v(s, m, op, Class::F32, g);
    }
    s.add("v_cvt_pkrtz_f16_f32", "VOP2", "e32", Class::F32, &[], &[vop2(0x2f, OUT, v(10), 11)], &[], &[Cmp::Pk16(0)], |x| on(pkrtz(x.d[0], x.d[1])));
    s.add("v_cvt_pkrtz_f16_f32", "VOP2", "e64 neg0 abs1", Class::F32, &[], &vop3m(0x12f, OUT, [v(10), v(11), 0], 2, 1, 0, false), &[], &[Cmp::Pk16(0)], |x| on(pkrtz(x.d[0] ^ 0x8000_0000, x.d[1] & 0x7fff_ffff)));
    let accs: [(&'static str, u32, bool, bool); 3] = [("v_mac_f32", 0x1f, false, false), ("v_mac_legacy_f32", 0x06, true, false), ("v_fmac_f32", 0x2b, false, true)];
    for (m, op, legacy, fused) in accs {
        let g = move |a: u32, b: u32, c: u32| if fused { fma32(a, b, c) } else { unfused(a, b, c, legacy) };
        s.add(m, "VOP2", "e32", Class::F32, &[vmov(OUT, v(12))], &[vop2(op, OUT, v(10), 11)], &[], &[Cmp::F32(0)], move |x| on(g(x.d[0], x.d[1], x.d[2])));
        s.add(m, "VOP2", "e64 neg1 abs0", Class::F32, &[vmov(OUT, v(12))], &vop3o(0x100 + op, OUT, [v(10), v(11), 0], 1, 2, false, 0), &[], &[Cmp::F32(0)], move |x| on(g(x.d[0] & 0x7fff_ffff, x.d[1] ^ 0x8000_0000, x.d[2])));
    }
    let ks = [0x3fc0_0000u32, 0xc120_0000, 0x0000_0000, 0x7f80_0000, 0x3eaa_aaab, 0x0040_0000];
    for (i, &k) in ks.iter().enumerate() {
        let tag = format!("K={k:08x} #{i}");
        s.add("v_madmk_f32", "VOP2", &tag, Class::F32, &[], &[vop2(0x20, OUT, v(10), 11), k], &[], &[Cmp::F32(0)], move |x| on(unfused(x.d[0], k, x.d[1], false)));
        s.add("v_madak_f32", "VOP2", &tag, Class::F32, &[], &[vop2(0x21, OUT, v(10), 11), k], &[], &[Cmp::F32(0)], move |x| on(unfused(x.d[0], x.d[1], k, false)));
        s.add("v_fmamk_f32", "VOP2", &tag, Class::F32, &[], &[vop2(0x2c, OUT, v(10), 11), k], &[], &[Cmp::F32(0)], move |x| on(fma32(x.d[0], k, x.d[1])));
        s.add("v_fmaak_f32", "VOP2", &tag, Class::F32, &[], &[vop2(0x2d, OUT, v(10), 11), k], &[], &[Cmp::F32(0)], move |x| on(fma32(x.d[0], x.d[1], k)));
    }
    let hs: &[(&'static str, u32, G2)] = &[("v_add_f16", 0x32, hadd), ("v_sub_f16", 0x33, hsub), ("v_subrev_f16", 0x34, hsubr), ("v_mul_f16", 0x35, hmul), ("v_max_f16", 0x39, hmax), ("v_min_f16", 0x3a, hmin)];
    for &(m, op, g) in hs {
        h2v(s, m, op, g);
    }
    s.add("v_ldexp_f16", "VOP2", "e32", Class::F16, &[vmov(OUT, v(12))], &[vop2(0x3b, OUT, v(10), 11)], &[], &[Cmp::F16(0)], |x| on(hldexp(x.d[0] & 0xffff, x.d[1]).map(|r| (x.d[2] & 0xffff_0000) | r)));
    s.add("v_ldexp_f16", "VOP2", "e32 small exp", Class::Small, &[vmov(OUT, v(12)), vop2(0x26, 32, v(11), 13)], &[vop2(0x3b, OUT, v(10), 32)], &[], &[Cmp::F16(0)], |x| {
        let a = x.d[0] & 0xffff;
        on(hldexp(a, x.d[1].wrapping_sub(x.d[3])).map(|r| (x.d[2] & 0xffff_0000) | r))
    });
    s.add("v_fmac_f16", "VOP2", "e32", Class::F16, &[vmov(OUT, v(12))], &[vop2(0x36, OUT, v(10), 11)], &[], &[Cmp::F16(0)], |x| on(hfma(x.d[0] & 0xffff, x.d[1] & 0xffff, x.d[2] & 0xffff).map(|r| (x.d[2] & 0xffff_0000) | r)));
    let hk = [0x3e00u32, 0xc900, 0x0000, 0x7c00, 0x3555, 0x0200];
    for (i, &k) in hk.iter().enumerate() {
        let tag = format!("K={k:04x} #{i}");
        s.add("v_fmamk_f16", "VOP2", &tag, Class::F16, &[vmov(OUT, v(12))], &[vop2(0x37, OUT, v(10), 11), k], &[], &[Cmp::F16(0)], move |x| on(hfma(x.d[0] & 0xffff, k, x.d[1] & 0xffff).map(|r| (x.d[2] & 0xffff_0000) | r)));
        s.add("v_fmaak_f16", "VOP2", &tag, Class::F16, &[vmov(OUT, v(12))], &[vop2(0x38, OUT, v(10), 11), k], &[], &[Cmp::F16(0)], move |x| on(hfma(x.d[0] & 0xffff, x.d[1] & 0xffff, k).map(|r| (x.d[2] & 0xffff_0000) | r)));
    }
    s.add("v_pk_fmac_f16", "VOP2", "e32", Class::F16, &[vmov(OUT, v(12))], &[vop2(0x3c, OUT, v(10), 11)], &[], &[Cmp::Pk16(0)], |x| {
        let lo = hfma(x.d[0], x.d[1], x.d[2]);
        let hi = hfma(x.d[0] >> 16, x.d[1] >> 16, x.d[2] >> 16);
        on(lo.zip(hi).map(|(l, h)| l | (h << 16)))
    });
}

fn sdwa_sel(x: u32, sel: u32, sext: bool) -> u32 {
    let (v, w) = match sel {
        0..=3 => ((x >> (8 * sel)) & 0xff, 8),
        4 | 5 => ((x >> (16 * (sel - 4))) & 0xffff, 16),
        _ => (x, 32),
    };
    if sext && w < 32 { (((v << (32 - w)) as i32) >> (32 - w)) as u32 } else { v }
}

fn sdwa_dst(r: u32, old: u32, sel: u32, unused: u32) -> u32 {
    let (sh, w) = match sel {
        0..=3 => (8 * sel, 8),
        4 | 5 => (16 * (sel - 4), 16),
        _ => return r,
    };
    let mask = (((1u64 << w) - 1) as u32) << sh;
    let field = (r << sh) & mask;
    match unused {
        0 => field,
        1 => {
            let top = sh + w;
            let sign = (r >> (w - 1)) & 1;
            let hi = if sign != 0 && top < 32 { !0u32 << top } else { 0 };
            field | hi
        }
        _ => (old & !mask) | field,
    }
}

fn sdwa_ops(s: &mut Suite) {
    let cases: &[(&'static str, u32, F2)] = &[
        ("v_add_nc_u32", 0x25, |a, b| a.wrapping_add(b)),
        ("v_sub_nc_u32", 0x26, |a, b| a.wrapping_sub(b)),
        ("v_and_b32", 0x1b, |a, b| a & b),
        ("v_or_b32", 0x1c, |a, b| a | b),
        ("v_lshlrev_b32", 0x1a, |a, b| b << (a & 31)),
        ("v_lshrrev_b32", 0x16, |a, b| b >> (a & 31)),
        ("v_ashrrev_i32", 0x18, |a, b| ((b as i32) >> (a & 31)) as u32),
        ("v_max_i32", 0x12, |a, b| (a as i32).max(b as i32) as u32),
        ("v_min_u32", 0x13, |a, b| a.min(b)),
        ("v_mul_u32_u24", 0x0b, |a, b| ((a as u64 & 0xff_ffff) * (b as u64 & 0xff_ffff)) as u32),
        ("v_mul_i32_i24", 0x09, |a, b| (s24(a) * s24(b)) as u32),
        ("v_xor_b32", 0x1d, |a, b| a ^ b),
    ];
    let combos: [(u32, u32, u32, bool, u32, bool); 8] = [(6, 0, 1, false, 6, false), (4, 0, 5, true, 0, false), (5, 2, 4, false, 5, true), (0, 0, 3, true, 2, true), (1, 1, 6, false, 1, true), (3, 2, 0, true, 4, false), (2, 1, 5, true, 6, false), (6, 2, 2, false, 3, true)];
    for &(m, op, f) in cases {
        for (i, &(dsel, du, s0, x0, s1, x1)) in combos.iter().enumerate() {
            let tag = format!("sdwa#{i} dst_sel={dsel} unused={du} s0_sel={s0}{} s1_sel={s1}{}", if x0 { " sext" } else { "" }, if x1 { " sext" } else { "" });
            let sh = matches!(op, 0x1a | 0x16 | 0x18);
            let class = if sh { Class::Small } else { Class::U32 };
            s.add(m, "VOP2", &tag, class, &[vmov(OUT, v(12))], &[vop2(op, OUT, 249, 11), sdwa(10, dsel, du, s0, x0, s1, x1)], &[], &[Cmp::X], move |x| {
                let a = sdwa_sel(x.d[0], s0, x0);
                let b = sdwa_sel(x.d[1], s1, x1);
                o(sdwa_dst(f(a, b), x.d[2], dsel, du))
            });
        }
    }
    let un: &[(&'static str, u32, fn(u32) -> u32)] = &[("v_mov_b32", 0x01, |a| a), ("v_not_b32", 0x37, |a| !a), ("v_bfrev_b32", 0x38, |a| a.reverse_bits())];
    for &(m, op, f) in un {
        for (i, &(dsel, du, s0, x0, _, _)) in combos.iter().enumerate() {
            let tag = format!("sdwa#{i} dst_sel={dsel} unused={du} s0_sel={s0}{}", if x0 { " sext" } else { "" });
            s.add(m, "VOP1", &tag, Class::U32, &[vmov(OUT, v(12))], &[vop1(op, OUT, 249), sdwa(10, dsel, du, s0, x0, 0, false)], &[], &[Cmp::X], move |x| o(sdwa_dst(f(sdwa_sel(x.d[0], s0, x0)), x.d[2], dsel, du)));
        }
    }
    for (i, &(s0, s1, dsel, du)) in [(5u32, 5u32, 5u32, 2u32), (4, 5, 5, 2), (5, 4, 4, 2), (5, 5, 4, 0), (4, 4, 5, 0)].iter().enumerate() {
        let tag = format!("sdwa#{i} s0_sel={s0} s1_sel={s1} dst_sel={dsel} unused={du}");
        for (m, op, g) in [("v_add_f16", 0x32u32, hadd as G2), ("v_mul_f16", 0x35, hmul), ("v_max_f16", 0x39, hmax)] {
            s.add(m, "VOP2", &tag, Class::F16, &[vmov(OUT, v(12))], &[vop2(op, OUT, 249, 11), sdwa(10, dsel, du, s0, false, s1, false)], &[], &[Cmp::X], move |x| {
                let a = sdwa_sel(x.d[0], s0, false);
                let b = sdwa_sel(x.d[1], s1, false);
                on(g(a, b).filter(|r| !nan16(*r)).map(|r| sdwa_dst(q16(r), x.d[2], dsel, du)).filter(|_| !(nan16(a) || nan16(b))))
            });
        }
    }
    sdwa_float_mods(s);
}

fn sdwa_mod_bits(i: u32, md: u32) -> u32 {
    ((md & 1) << (20 + 8 * i)) | (((md >> 1) & 1) << (21 + 8 * i))
}

fn sdwa_float_mods(s: &mut Suite) {
    let names = ["", "neg", "abs", "neg abs"];
    for i in 0..2u32 {
        for sel in 0..=6u32 {
            for md in 1..=3u32 {
                let (s0, s1) = if i == 0 { (sel, 4) } else { (4, sel) };
                let tag = format!("sdwa src{i}_sel={sel} {}", names[md as usize]);
                for (m, op, g) in [("v_add_f16", 0x32u32, hadd as G2), ("v_sub_f16", 0x33, hsub), ("v_mul_f16", 0x35, hmul)] {
                    s.add(m, "VOP2", &tag, Class::F16, &[vmov(OUT, v(12))], &[vop2(op, OUT, 249, 11), sdwa(10, 4, 0, s0, false, s1, false) | sdwa_mod_bits(i, md)], &[], &[Cmp::X], move |x| {
                        let (m0, m1) = if i == 0 { (md, 0) } else { (0, md) };
                        let a = mods16(sdwa_sel(x.d[0], s0, false), m0 & 2 != 0, m0 & 1 != 0);
                        let b = mods16(sdwa_sel(x.d[1], s1, false), m1 & 2 != 0, m1 & 1 != 0);
                        on(g(a, b).filter(|r| !nan16(*r)).map(|r| sdwa_dst(r, x.d[2], 4, 0)).filter(|_| !(nan16(a) || nan16(b))))
                    });
                }
                let (s0, s1) = if i == 0 { (sel, 6) } else { (6, sel) };
                for (m, op, g) in [("v_add_f32", 0x03u32, fadd as G2), ("v_sub_f32", 0x04, fsub), ("v_mul_f32", 0x08, fmul)] {
                    s.add(m, "VOP2", &tag, Class::F32, &[], &[vop2(op, OUT, 249, 11), sdwa(10, 6, 0, s0, false, s1, false) | sdwa_mod_bits(i, md)], &[], &[Cmp::F32(0)], move |x| {
                        let (m0, m1) = if i == 0 { (md, 0) } else { (0, md) };
                        let a = mods32(sdwa_sel(x.d[0], s0, false), m0 & 2 != 0, m0 & 1 != 0);
                        let b = mods32(sdwa_sel(x.d[1], s1, false), m1 & 2 != 0, m1 & 1 != 0);
                        if den32(a) || den32(b) {
                            return vec![None];
                        }
                        on(g(a, b).filter(|r| !den32(*r)))
                    });
                }
            }
        }
    }
    for md in 1..=3u32 {
        let tag = format!("sdwa src0 {}", names[md as usize]);
        s.add("v_cvt_f16_f32", "VOP1", &tag, Class::F32, &[vmov(OUT, v(12))], &[vop1(0x0a, OUT, 249), sdwa(10, 4, 0, 6, false, 0, false) | sdwa_mod_bits(0, md)], &[], &[Cmp::X], move |x| {
            let a = mods32(x.d[0], md & 2 != 0, md & 1 != 0);
            if nan32(a) || den32(a) {
                return vec![None];
            }
            o(f2h_rne(f(a)))
        });
        for sel in [4u32, 5] {
            let tag = format!("sdwa src0_sel={sel} {}", names[md as usize]);
            s.add("v_cvt_f32_f16", "VOP1", &tag, Class::F16, &[], &[vop1(0x0b, OUT, 249), sdwa(10, 6, 0, sel, false, 0, false) | sdwa_mod_bits(0, md)], &[], &[Cmp::F32(0)], move |x| {
                let a = mods16(sdwa_sel(x.d[0], sel, false), md & 2 != 0, md & 1 != 0);
                on(if nan16(a) { None } else { Some(f32q(h2f(a))) })
            });
        }
    }
}

fn u1(s: &mut Suite, m: &'static str, op: u32, class: Class, f: fn(u32) -> u32) {
    s.add(m, "VOP1", "e32", class, &[], &[vop1(op, OUT, v(10))], &[], &[Cmp::X], move |x| o(f(x.d[0])));
    s.add(m, "VOP1", "e32 sgpr", class, &[], &[vop1(op, OUT, 11)], &[], &[Cmp::X], move |x| o(f(x.d[1])));
    s.add(m, "VOP1", "e64", class, &[], &vop3(0x180 + op, OUT, [v(12), 0, 0]), &[], &[Cmp::X], move |x| o(f(x.d[2])));
}

fn f1(s: &mut Suite, m: &'static str, op: u32, class: Class, cmp: Cmp, g: fn(u32) -> Option<u32>, mods: bool) {
    s.add(m, "VOP1", "e32", class, &[], &[vop1(op, OUT, v(10))], &[], &[cmp], move |x| on(g(x.d[0])));
    if mods {
        s.add(m, "VOP1", "e64 neg", class, &[], &vop3o(0x180 + op, OUT, [v(11), 0, 0], 0, 1, false, 0), &[], &[cmp], move |x| on(g(x.d[1] ^ 0x8000_0000)));
        s.add(m, "VOP1", "e64 abs clamp", class, &[], &vop3o(0x180 + op, OUT, [v(11), 0, 0], 1, 0, true, 0), &[], &[cmp], move |x| on(g(x.d[1] & 0x7fff_ffff).and_then(|r| out32(r, true, 0))));
    }
}

fn h1(s: &mut Suite, m: &'static str, op: u32, class: Class, cmp: Cmp, g: fn(u32) -> Option<u32>) {
    s.add(m, "VOP1", "e32", class, &[vmov(OUT, v(12))], &[vop1(op, OUT, v(10))], &[], &[cmp], move |x| on(g(x.d[0] & 0xffff).map(|r| (x.d[2] & 0xffff_0000) | (r & 0xffff))));
    s.add(m, "VOP1", "e64", class, &[vmov(OUT, v(12))], &vop3(0x180 + op, OUT, [v(11), 0, 0]), &[], &[cmp], move |x| on(g(x.d[1] & 0xffff).map(|r| (x.d[2] & 0xffff_0000) | (r & 0xffff))));
}

fn d1x(s: &mut Suite, m: &'static str, op: u32, i: usize) {
    s.add(m, "VOP1", "e32", Class::F64, &[], &[vop1(op, OUT, v(10))], &[], &[Cmp::X, Cmp::X], move |x| o64(tombox_xshader::trans32::eval64(i, ((x.d[1] as u64) << 32) | x.d[0] as u64)));
    s.add(m, "VOP1", "e64 neg", Class::F64, &[], &vop3o(0x180 + op, OUT, [v(12), 0, 0], 0, 1, false, 0), &[], &[Cmp::X, Cmp::X], move |x| o64(tombox_xshader::trans32::eval64(i, (((x.d[3] as u64) << 32) | x.d[2] as u64) ^ (1 << 63))));
}

fn d1(s: &mut Suite, m: &'static str, op: u32, cmp: Cmp, g: fn(f64) -> Option<u64>) {
    s.add(m, "VOP1", "e32", Class::F64, &[], &[vop1(op, OUT, v(10))], &[], &[cmp, Cmp::F64Hi], move |x| g(d(x.d[0], x.d[1])).map_or(vec![None, None], o64));
    s.add(m, "VOP1", "e64 neg", Class::F64, &[], &vop3o(0x180 + op, OUT, [v(12), 0, 0], 0, 1, false, 0), &[], &[cmp, Cmp::F64Hi], move |x| g(-d(x.d[2], x.d[3])).map_or(vec![None, None], o64));
}

fn frexp32(x: u32) -> (u32, u32) {
    let a = x & 0x7fff_ffff;
    if a == 0 || a >= 0x7f80_0000 {
        return (x, 0);
    }
    let v = f(x) as f64;
    let e = v.abs().log2().floor() as i32 + 1;
    let e = if v.abs() / 2f64.powi(e) >= 1.0 { e + 1 } else if v.abs() / 2f64.powi(e) < 0.5 { e - 1 } else { e };
    (((v / 2f64.powi(e)) as f32).to_bits(), e as u32)
}

fn frexp64(x: u64) -> (u64, u32) {
    let a = x & 0x7fff_ffff_ffff_ffff;
    if a == 0 || a >= 0x7ff0_0000_0000_0000 {
        return (x, 0);
    }
    let v = f64::from_bits(x);
    let ex = ((x >> 52) & 0x7ff) as i32;
    let (m, e) = if ex == 0 {
        let y = v * 2f64.powi(64);
        let ey = ((y.to_bits() >> 52) & 0x7ff) as i32;
        (y, ey - 1022 - 64)
    } else {
        (v, ex - 1022)
    };
    let mant = (m.to_bits() & 0x800f_ffff_ffff_ffff) | (1022u64 << 52);
    (mant, e as u32)
}

fn frexp16(x: u32) -> (u32, u32) {
    let x = x & 0xffff;
    let a = x & 0x7fff;
    if a == 0 || a >= 0x7c00 {
        return (x, 0);
    }
    let (m, e) = frexp32(h2f(x).to_bits());
    (f2h_rne(f(m)), e & 0xffff)
}

fn rpi(x: u32) -> Option<u32> {
    if nan32(x) {
        return Some(if x & 0x8000_0000 != 0 { 0x8000_0000 } else { 0x7fff_ffff });
    }
    Some(cvt_i32((f(x) as f64 + 0.5).floor()))
}

fn fract32(x: u32) -> Option<u32> {
    let v = f(x);
    if !v.is_finite() {
        return Some(QNAN32);
    }
    if x == 0x8000_0000 {
        return None;
    }
    let r = (v as f64 - (v as f64).floor()) as f32;
    Some(r.min(f(0x3f7f_ffff)).to_bits())
}

fn vop1_ops(s: &mut Suite) {
    u1(s, "v_mov_b32", 0x01, Class::U32, |a| a);
    u1(s, "v_not_b32", 0x37, Class::U32, |a| !a);
    u1(s, "v_bfrev_b32", 0x38, Class::U32, |a| a.reverse_bits());
    u1(s, "v_ffbh_u32", 0x39, Class::U32, |a| if a == 0 { u32::MAX } else { a.leading_zeros() });
    u1(s, "v_ffbl_b32", 0x3a, Class::U32, |a| if a == 0 { u32::MAX } else { a.trailing_zeros() });
    u1(s, "v_ffbh_i32", 0x3b, Class::U32, |a| if a == 0 || a == u32::MAX { u32::MAX } else if (a as i32) < 0 { (!a).leading_zeros() } else { a.leading_zeros() });
    u1(s, "v_cvt_f32_ubyte0", 0x11, Class::U32, |a| ((a & 0xff) as f32).to_bits());
    u1(s, "v_cvt_f32_ubyte1", 0x12, Class::U32, |a| (((a >> 8) & 0xff) as f32).to_bits());
    u1(s, "v_cvt_f32_ubyte2", 0x13, Class::U32, |a| (((a >> 16) & 0xff) as f32).to_bits());
    u1(s, "v_cvt_f32_ubyte3", 0x14, Class::U32, |a| ((a >> 24) as f32).to_bits());
    u1(s, "v_cvt_off_f32_i4", 0x0e, Class::U32, |a| ((((a << 28) as i32) >> 28) as f32 / 16.0).to_bits());
    u1(s, "v_cvt_f32_i32", 0x05, Class::U32, |a| (a as i32 as f32).to_bits());
    u1(s, "v_cvt_f32_u32", 0x06, Class::U32, |a| (a as f32).to_bits());
    f1(s, "v_cvt_u32_f32", 0x07, Class::F32, Cmp::X, |a| Some(cvt_u32(f(a) as f64)), false);
    f1(s, "v_cvt_i32_f32", 0x08, Class::F32, Cmp::X, |a| Some(cvt_i32(f(a) as f64)), false);
    f1(s, "v_cvt_rpi_i32_f32", 0x0c, Class::F32, Cmp::X, rpi, false);
    f1(s, "v_cvt_flr_i32_f32", 0x0d, Class::F32, Cmp::X, |a| if den32(a) { None } else if nan32(a) { Some(if a & 0x8000_0000 != 0 { 0x8000_0000 } else { 0x7fff_ffff }) } else { Some(cvt_i32((f(a) as f64).floor())) }, false);
    f1(s, "v_cvt_f32_f16", 0x0b, Class::F16, Cmp::F32(0), |a| Some(f32q(h2f(a & 0xffff))), false);
    s.add("v_cvt_f16_f32", "VOP1", "e32", Class::F32, &[vmov(OUT, v(12))], &[vop1(0x0a, OUT, v(10))], &[], &[Cmp::F16(0)], |x| o((x.d[2] & 0xffff_0000) | f2h_rne(f(x.d[0]))));
    s.add("v_cvt_f16_f32", "VOP1", "e64 opsel dst hi", Class::F32, &[vmov(OUT, v(12))], &vop3m(0x18a, OUT, [v(11), 0, 0], 0, 0, 0b1000, false), &[], &[Cmp::X], |x| {
        let r = f2h_rne(f(x.d[1]));
        on(if nan16(r) { None } else { Some((x.d[2] & 0xffff) | (r << 16)) })
    });
    f1(s, "v_fract_f32", 0x20, Class::F32, Cmp::F32(0), |a| if den32(a) { None } else { fract32(a) }, true);
    f1(s, "v_trunc_f32", 0x21, Class::F32, Cmp::F32(0), |a| Some(f32q(f(a).trunc())), true);
    f1(s, "v_ceil_f32", 0x22, Class::F32, Cmp::F32(0), |a| if den32(a) { None } else { Some(f32q(f(a).ceil())) }, true);
    f1(s, "v_rndne_f32", 0x23, Class::F32, Cmp::F32(0), |a| Some(f32q(rne32(f(a)))), true);
    f1(s, "v_floor_f32", 0x24, Class::F32, Cmp::F32(0), |a| if den32(a) { None } else { Some(f32q(f(a).floor())) }, true);
    f1(s, "v_exp_f32", 0x25, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(5, a)), true);
    f1(s, "v_log_f32", 0x27, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(4, a)), true);
    f1(s, "v_rcp_f32", 0x2a, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(0, a)), true);
    f1(s, "v_rcp_iflag_f32", 0x2b, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(1, a)), false);
    f1(s, "v_rsq_f32", 0x2e, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(2, a)), true);
    f1(s, "v_sqrt_f32", 0x33, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(3, a)), true);
    f1(s, "v_sin_f32", 0x35, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(6, a)), true);
    f1(s, "v_cos_f32", 0x36, Class::Trans, Cmp::X, |a| Some(tombox_xshader::trans32::eval(7, a)), true);
    f1(s, "v_frexp_exp_i32_f32", 0x3f, Class::F32, Cmp::X, |a| Some(frexp32(a).1), false);
    f1(s, "v_frexp_mant_f32", 0x40, Class::F32, Cmp::F32(0), |a| Some(frexp32(a).0), false);
    h1(s, "v_cvt_f16_u16", 0x50, Class::U32, Cmp::F16(0), |a| Some(d2h((a & 0xffff) as f64)));
    h1(s, "v_cvt_f16_i16", 0x51, Class::U32, Cmp::F16(0), |a| Some(d2h((a as u16 as i16) as f64)));
    h1(s, "v_cvt_u16_f16", 0x52, Class::F16, Cmp::X, |a| Some(if nan16(a) { 0 } else { h(a).clamp(0.0, 65535.0).trunc() as u32 }));
    h1(s, "v_cvt_i16_f16", 0x53, Class::F16, Cmp::X, |a| Some(if nan16(a) { 0 } else { (h(a).clamp(-32768.0, 32767.0).trunc() as i32 as u32) & 0xffff }));
    h1(s, "v_rcp_f16", 0x54, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(0, a as u16) as u32));
    h1(s, "v_sqrt_f16", 0x55, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(2, a as u16) as u32));
    h1(s, "v_rsq_f16", 0x56, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(1, a as u16) as u32));
    h1(s, "v_log_f16", 0x57, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(3, a as u16) as u32));
    h1(s, "v_exp_f16", 0x58, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(4, a as u16) as u32));
    h1(s, "v_frexp_mant_f16", 0x59, Class::F16, Cmp::F16(0), |a| Some(frexp16(a).0));
    h1(s, "v_frexp_exp_i16_f16", 0x5a, Class::F16, Cmp::X, |a| Some(frexp16(a).1));
    h1(s, "v_floor_f16", 0x5b, Class::F16, Cmp::F16(0), |a| Some(q16(d2h(h(a).floor()) | if a & 0x8000 != 0 && h(a).floor() == 0.0 { 0x8000 } else { 0 })));
    h1(s, "v_ceil_f16", 0x5c, Class::F16, Cmp::F16(0), |a| Some(q16(d2h(h(a).ceil()) | if a & 0x8000 != 0 && h(a).ceil() == 0.0 { 0x8000 } else { 0 })));
    h1(s, "v_trunc_f16", 0x5d, Class::F16, Cmp::F16(0), |a| Some(q16(d2h(h(a).trunc()) | if a & 0x8000 != 0 { 0x8000 } else { 0 })));
    h1(s, "v_rndne_f16", 0x5e, Class::F16, Cmp::F16(0), |a| Some(q16(d2h(rne64(h(a))) | if a & 0x8000 != 0 { 0x8000 } else { 0 })));
    h1(s, "v_fract_f16", 0x5f, Class::F16, Cmp::F16(0), |a| {
        let v = h(a);
        if !v.is_finite() {
            return Some(QNAN16);
        }
        let r = d2h(v - v.floor());
        Some(if r == 0x3c00 { 0x3bff } else { r })
    });
    h1(s, "v_sin_f16", 0x60, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(5, a as u16) as u32));
    h1(s, "v_cos_f16", 0x61, Class::Trans16, Cmp::X, |a| Some(tombox_xshader::trans16::eval(6, a as u16) as u32));
    s.add("v_sat_pk_u8_i16", "VOP1", "e32 full", Class::U32, &[vmov(OUT, v(12))], &[vop1(0x62, OUT, v(10))], &[], &[Cmp::X], |x| {
        let a = x.d[0];
        o((x.d[2] & 0xffff_0000) | ((a as u16 as i16).clamp(0, 255) as u32) | (((((a >> 16) as u16 as i16).clamp(0, 255)) as u32) << 8))
    });
    h1(s, "v_cvt_norm_i16_f16", 0x63, Class::F16, Cmp::X, |a| norm16(h(a), true, nan16(a)));
    h1(s, "v_cvt_norm_u16_f16", 0x64, Class::F16, Cmp::X, |a| norm16(h(a), false, nan16(a)));
    d1(s, "v_trunc_f64", 0x17, Cmp::F64(0), |a| Some(f64q(a.trunc())));
    d1(s, "v_ceil_f64", 0x18, Cmp::F64(0), |a| Some(f64q(a.ceil())));
    d1(s, "v_rndne_f64", 0x19, Cmp::F64(0), |a| Some(f64q(rne64(a))));
    d1(s, "v_floor_f64", 0x1a, Cmp::F64(0), |a| Some(f64q(a.floor())));
    d1(s, "v_fract_f64", 0x3e, Cmp::F64(0), |a| if !a.is_finite() { Some(QNAN64) } else { Some((a - a.floor()).min(f64::from_bits(0x3fef_ffff_ffff_ffff)).to_bits()) });
    d1x(s, "v_rcp_f64", 0x2f, 0);
    d1x(s, "v_rsq_f64", 0x31, 1);
    d1x(s, "v_sqrt_f64", 0x34, 2);
    d1(s, "v_frexp_mant_f64", 0x3d, Cmp::F64(0), |a| Some(frexp64(a.to_bits()).0));
    s.add("v_frexp_exp_i32_f64", "VOP1", "e32", Class::F64, &[], &[vop1(0x3c, OUT, v(10))], &[], &[Cmp::X], |x| o(frexp64(d(x.d[0], x.d[1]).to_bits()).1));
    s.add("v_cvt_i32_f64", "VOP1", "e32", Class::F64, &[], &[vop1(0x03, OUT, v(10))], &[], &[Cmp::X], |x| o(cvt_i32(d(x.d[0], x.d[1]))));
    s.add("v_cvt_u32_f64", "VOP1", "e32", Class::F64, &[], &[vop1(0x15, OUT, v(10))], &[], &[Cmp::X], |x| o(cvt_u32(d(x.d[0], x.d[1]))));
    s.add("v_cvt_f32_f64", "VOP1", "e32", Class::F64, &[], &[vop1(0x0f, OUT, v(10))], &[], &[Cmp::F32(0)], |x| o(f32q(d(x.d[0], x.d[1]) as f32)));
    s.add("v_cvt_f64_f32", "VOP1", "e32", Class::F32, &[], &[vop1(0x10, OUT, v(10))], &[], &[Cmp::F64(0), Cmp::F64Hi], |x| o64(f64q(f(x.d[0]) as f64)));
    s.add("v_cvt_f64_i32", "VOP1", "e32", Class::U32, &[], &[vop1(0x04, OUT, v(10))], &[], &[Cmp::F64(0), Cmp::F64Hi], |x| o64((x.d[0] as i32 as f64).to_bits()));
    s.add("v_cvt_f64_u32", "VOP1", "e32", Class::U32, &[], &[vop1(0x16, OUT, v(10))], &[], &[Cmp::F64(0), Cmp::F64Hi], |x| o64((x.d[0] as f64).to_bits()));
    for (i, m0) in [0u32, 1, 3].into_iter().enumerate() {
        let tag = format!("m0={m0} #{i}");
        s.add("v_movrels_b32", "VOP1", &tag, Class::U32, &[sop1(3, M0, ic(m0 as i32))], &[vop1(0x43, OUT, v(10))], &[], &[Cmp::X], move |x| o(x.d[m0 as usize]));
        s.add("v_movreld_b32", "VOP1", &tag, Class::U32, &[vmov(32, v(10)), vmov(33, v(11)), vmov(34, v(12)), vmov(35, v(13)), sop1(3, M0, ic(m0 as i32))], &[vop1(0x42, 32, v(14))], &[vmov(OUT, v(32)), vmov(OUT + 1, v(33)), vmov(OUT + 2, v(34)), vmov(OUT + 3, v(35))], &[Cmp::X, Cmp::X, Cmp::X, Cmp::X], move |x| {
            let mut r = [x.d[0], x.d[1], x.d[2], x.d[3]];
            r[m0 as usize] = x.d[4];
            r.iter().map(|&v| Some(v)).collect()
        });
        s.add("v_movrelsd_b32", "VOP1", &tag, Class::U32, &[vmov(32, v(14)), vmov(33, v(14)), vmov(34, v(14)), vmov(35, v(14)), sop1(3, M0, ic(m0 as i32))], &[vop1(0x44, 32, v(10))], &[vmov(OUT, v(32)), vmov(OUT + 1, v(33)), vmov(OUT + 2, v(34)), vmov(OUT + 3, v(35))], &[Cmp::X, Cmp::X, Cmp::X, Cmp::X], move |x| {
            let mut r = [x.d[4]; 4];
            r[m0 as usize] = x.d[m0 as usize];
            r.iter().map(|&v| Some(v)).collect()
        });
    }
    s.add("v_swap_b32", "VOP1", "e32", Class::U32, &[vmov(OUT, v(10)), vmov(OUT + 1, v(11))], &[vop1(0x65, OUT, v(OUT + 1))], &[], &[Cmp::X, Cmp::X], |x| o2(x.d[1], x.d[0]));
    for (m, op) in [("v_nop", 0x00u32), ("v_pipeflush", 0x1b), ("v_clrexcp", 0x41)] {
        s.add(m, "VOP1", "no state change", Class::U32, &[vmov(OUT, v(10)), vmov(OUT + 1, v(11))], &[vop1(op, OUT + 3, 0)], &[], &[Cmp::X, Cmp::X], |x| o2(x.d[0], x.d[1]));
    }
}

pub fn norm16(v: f64, signed: bool, nan: bool) -> Option<u32> {
    if nan {
        return Some(0);
    }
    let r = if signed { v.clamp(-1.0, 1.0) * 32767.0 } else { v.clamp(0.0, 1.0) * 65535.0 };
    Some((r.round_ties_even() as i32 as u32) & 0xffff)
}

const NRM_F32: [u32; 26] = [
    0x3840_0180, 0x38e0_01c0, 0xb840_0180, 0xb8e0_01c0, 0x37c0_00c0, 0x3860_00e0, 0x3eff_ffff, 0x3f00_0001, 0x3f00_0000, 0xbf00_0000, 0x3f80_0000, 0xbf80_0000, 0x3f80_0001,
    0xbf80_0001, 0x7f80_0000, 0xff80_0000, 0x7fc0_0000, 0xffc0_0000, 0x7f80_0001, 0xff80_0001, 0x8000_0000, 0x0000_0001, 0x807f_ffff, 0x3eff_fffe, 0x3f7f_ffff, 0xbf7f_ffff,
];
const NRM_F16: [u32; 24] = [
    0x3801, 0x3802, 0x3803, 0x3804, 0xb801, 0xb802, 0x37ff, 0x3800, 0xb800, 0x3c00, 0xbc00, 0x3c01, 0xbc01, 0x7c00, 0xfc00, 0x7e00, 0xfe00, 0x7c01, 0xfc01, 0x8000, 0x0001, 0x83ff,
    0x3bff, 0xbbff,
];
const NV: u32 = 40;

fn norm_edges(s: &mut Suite) {
    for (m, op, signed) in [("v_cvt_norm_i16_f16", 0x63u32, true), ("v_cvt_norm_u16_f16", 0x64, false)] {
        for &a in NRM_F16.iter() {
            s.add(m, "VOP1", &format!("e32 edge {a:#06x}"), Class::F16, &[vmov(OUT, v(12)), vmov(NV, LIT), 0xabcd_0000 | a], &[vop1(op, OUT, v(NV))], &[], &[Cmp::X], move |x| on(norm16(h(a), signed, nan16(a)).map(|r| (x.d[2] & 0xffff_0000) | r)));
        }
    }
    for (m, op, signed) in [("v_cvt_pknorm_i16_f16", 0x312u32, true), ("v_cvt_pknorm_u16_f16", 0x313, false)] {
        for p in NRM_F16.chunks(2) {
            let (a, b) = (p[0], p[1]);
            s.add(m, "VOP3", &format!("edge {a:#06x},{b:#06x}"), Class::F16, &[vmov(NV, LIT), 0xabcd_0000 | a, vmov(NV + 1, LIT), 0x1234_0000 | b], &vop3(op, OUT, [v(NV), v(NV + 1), 0]), &[], &[Cmp::X], move |_| on(norm16(h(a), signed, nan16(a)).zip(norm16(h(b), signed, nan16(b))).map(|(x, y)| x | (y << 16))));
        }
    }
    for (m, op, signed) in [("v_cvt_pknorm_i16_f32", 0x368u32, true), ("v_cvt_pknorm_u16_f32", 0x369, false)] {
        for p in NRM_F32.chunks(2) {
            let (a, b) = (p[0], p[1]);
            s.add(m, "VOP3", &format!("edge {a:#010x},{b:#010x}"), Class::F32, &[vmov(NV, LIT), a, vmov(NV + 1, LIT), b], &vop3(op, OUT, [v(NV), v(NV + 1), 0]), &[], &[Cmp::X], move |_| on(norm16(f(a) as f64, signed, nan32(a)).zip(norm16(f(b) as f64, signed, nan32(b))).map(|(x, y)| x | (y << 16))));
        }
    }
}

fn i3(s: &mut Suite, m: &'static str, op: u32, class: Class, f: F3) {
    s.add(m, "VOP3", "v,v,v", class, &[], &vop3(op, OUT, [v(10), v(11), v(12)]), &[], &[Cmp::X], move |x| o(f(x.d[0], x.d[1], x.d[2])));
    s.add(m, "VOP3", "s,v,s", class, &[], &vop3(op, OUT, [13, v(11), 14]), &[], &[Cmp::X], move |x| o(f(x.d[3], x.d[1], x.d[4])));
}

fn i16_3(s: &mut Suite, m: &'static str, op: u32, f: F3) {
    s.add(m, "VOP3", "v,v,v", Class::U32, &[vmov(OUT, v(13))], &vop3(op, OUT, [v(10), v(11), v(12)]), &[], &[Cmp::X], move |x| o((x.d[3] & 0xffff_0000) | (f(x.d[0] & 0xffff, x.d[1] & 0xffff, x.d[2] & 0xffff) & 0xffff)));
    s.add(m, "VOP3", "opsel src0hi,src2hi,dsthi", Class::U32, &[vmov(OUT, v(13))], &vop3m(op, OUT, [v(10), v(11), v(12)], 0, 0, 0b1101, false), &[], &[Cmp::X], move |x| o((x.d[3] & 0xffff) | ((f(x.d[0] >> 16, x.d[1] & 0xffff, x.d[2] >> 16) & 0xffff) << 16)));
}

fn f3(s: &mut Suite, m: &'static str, op: u32, g: G3) {
    s.add(m, "VOP3", "v,v,v", Class::F32, &[], &vop3(op, OUT, [v(10), v(11), v(12)]), &[], &[Cmp::F32(0)], move |x| on(g(x.d[0], x.d[1], x.d[2])));
    s.add(m, "VOP3", "neg0 abs1 neg2", Class::F32, &[], &vop3o(op, OUT, [v(10), v(11), v(12)], 2, 5, false, 0), &[], &[Cmp::F32(0)], move |x| on(g(x.d[0] ^ 0x8000_0000, x.d[1] & 0x7fff_ffff, x.d[2] ^ 0x8000_0000)));
    s.add(m, "VOP3", "abs2 clamp", Class::F32, &[], &vop3o(op, OUT, [v(10), v(11), v(12)], 4, 0, true, 0), &[], &[Cmp::F32(0)], move |x| on(g(x.d[0], x.d[1], x.d[2] & 0x7fff_ffff).and_then(|r| out32(r, true, 0))));
}

fn h3(s: &mut Suite, m: &'static str, op: u32, g: G3) {
    s.add(m, "VOP3", "v,v,v", Class::F16, &[vmov(OUT, v(13))], &vop3(op, OUT, [v(10), v(11), v(12)]), &[], &[Cmp::F16(0)], move |x| on(g(x.d[0] & 0xffff, x.d[1] & 0xffff, x.d[2] & 0xffff).map(|r| (x.d[3] & 0xffff_0000) | (r & 0xffff))));
    s.add(m, "VOP3", "opsel 0hi 2hi dsthi neg1", Class::F16, &[vmov(OUT, v(13))], &vop3m(op, OUT, [v(10), v(11), v(12)], 0, 2, 0b1101, false), &[], &[Cmp::F16H(0)], move |x| on(g(x.d[0] >> 16, (x.d[1] & 0xffff) ^ 0x8000, x.d[2] >> 16).map(|r| (x.d[3] & 0xffff) | ((r & 0xffff) << 16))));
}

fn med3u<T: Ord + Copy>(a: T, b: T, c: T) -> T {
    let mut v = [a, b, c];
    v.sort();
    v[1]
}

fn min3f(a: u32, b: u32, c: u32) -> Option<u32> {
    min_num32(a, b).and_then(|t| min_num32(t, c))
}
fn max3f(a: u32, b: u32, c: u32) -> Option<u32> {
    max_num32(a, b).and_then(|t| max_num32(t, c))
}
fn med3f(a: u32, b: u32, c: u32) -> Option<u32> {
    if nan32(a) || nan32(b) || nan32(c) {
        return None;
    }
    let z = |x: u32| x & 0x7fff_ffff == 0;
    if [a, b, c].iter().filter(|&&x| z(x)).count() >= 2 && !(a == b && b == c) {
        return None;
    }
    let mut v = [f(a), f(b), f(c)];
    v.sort_by(|p, q| p.partial_cmp(q).unwrap());
    let m = v[1];
    Some([a, b, c].into_iter().find(|&x| f(x) == m && (m != 0.0 || true)).unwrap())
}
fn min3h(a: u32, b: u32, c: u32) -> Option<u32> {
    min_num16(a, b).and_then(|t| min_num16(t, c))
}
fn max3h(a: u32, b: u32, c: u32) -> Option<u32> {
    max_num16(a, b).and_then(|t| max_num16(t, c))
}
fn med3h(a: u32, b: u32, c: u32) -> Option<u32> {
    if nan16(a) || nan16(b) || nan16(c) {
        return None;
    }
    let z = |x: u32| x & 0x7fff == 0;
    if [a, b, c].iter().filter(|&&x| z(x)).count() >= 2 && !(a == b && b == c) {
        return None;
    }
    let mut v = [h(a), h(b), h(c)];
    v.sort_by(|p, q| p.partial_cmp(q).unwrap());
    Some([a, b, c].into_iter().find(|&x| h(x) == v[1]).unwrap())
}

fn cube(a: u32, b: u32, c: u32) -> Option<(f32, f32, f32, f32)> {
    if nan32(a) || nan32(b) || nan32(c) {
        return None;
    }
    let (x, y, z) = (f(a), f(b), f(c));
    if z.abs() >= x.abs() && z.abs() >= y.abs() {
        Some((if z < 0.0 { 5.0 } else { 4.0 }, if z < 0.0 { -x } else { x }, -y, 2.0 * z))
    } else if y.abs() >= x.abs() {
        Some((if y < 0.0 { 3.0 } else { 2.0 }, x, if y < 0.0 { -z } else { z }, 2.0 * y))
    } else {
        Some((if x < 0.0 { 1.0 } else { 0.0 }, if x < 0.0 { z } else { -z }, -y, 2.0 * x))
    }
}

fn perm_byte(d: u64, sel: u32) -> u32 {
    match sel {
        0..=7 => ((d >> (8 * sel)) & 0xff) as u32,
        8 => if d & 0x8000 != 0 { 0xff } else { 0 },
        9 => if d & 0x8000_0000 != 0 { 0xff } else { 0 },
        10 => if d & 0x8000_0000_0000 != 0 { 0xff } else { 0 },
        11 => if d >> 63 != 0 { 0xff } else { 0 },
        12 => 0,
        _ => 0xff,
    }
}

fn bfe_i(a: u32, off: u32, w: u32) -> u32 {
    let (off, w) = (off & 31, w & 31);
    if w == 0 {
        return 0;
    }
    let t = (a as i32) >> off;
    let sh = 32 - w;
    ((t << sh) >> sh) as u32
}

fn bfe_u(a: u32, off: u32, w: u32) -> u32 {
    let (off, w) = (off & 31, w & 31);
    (a >> off) & (((1u64 << w) - 1) as u32)
}

fn pk_u8(a: u32, sel: u32, base: u32) -> Option<u32> {
    let v = f(a);
    let b = if v.is_nan() {
        0
    } else {
        let c = v.clamp(0.0, 255.0) as f64;
        if (c - c.trunc()) == 0.5 {
            return None;
        }
        rne64(c) as u32
    };
    let sh = 8 * (sel & 3);
    Some((base & !(0xff << sh)) | (b << sh))
}

fn div_fixup(q: f64, den: f64, num: f64) -> Option<u64> {
    let so = (den.is_sign_negative() as u8) ^ (num.is_sign_negative() as u8);
    let sgn = |x: f64| if so != 0 { -x } else { x };
    if num.is_nan() || den.is_nan() {
        return Some(QNAN64);
    }
    if den == 0.0 && num == 0.0 {
        return Some(QNAN64);
    }
    if den.is_infinite() && num.is_infinite() {
        return Some(QNAN64);
    }
    if den == 0.0 || num.is_infinite() {
        return Some(sgn(f64::INFINITY).to_bits());
    }
    if den.is_infinite() || num == 0.0 {
        return Some(sgn(0.0).to_bits());
    }
    let en = ((num.to_bits() >> 52) & 0x7ff) as i32;
    let ed = ((den.to_bits() >> 52) & 0x7ff) as i32;
    if en - ed < -1075 || ed == 2047 || en == 0 || ed == 0 || q.is_nan() {
        return None;
    }
    Some(sgn(q.abs()).to_bits())
}

fn vop3_ops(s: &mut Suite) {
    let ints: &[(&'static str, u32, Class, F3)] = &[
        ("v_mad_i32_i24", 0x142, Class::U32, |a, b, c| ((s24(a) * s24(b)) as u32).wrapping_add(c)),
        ("v_mad_u32_u24", 0x143, Class::U32, |a, b, c| (((a as u64 & 0xff_ffff) * (b as u64 & 0xff_ffff)) as u32).wrapping_add(c)),
        ("v_bfe_u32", 0x148, Class::U32, |a, off, w| bfe_u(a, off, w)),
        ("v_bfe_i32", 0x149, Class::U32, |a, off, w| bfe_i(a, off, w)),
        ("v_bfi_b32", 0x14a, Class::U32, |a, b, c| (a & b) | (!a & c)),
        ("v_lerp_u8", 0x14d, Class::U32, |a, b, c| (0..4).fold(0, |acc, k| acc | (((((a >> (8 * k)) & 0xff) + ((b >> (8 * k)) & 0xff) + ((c >> (8 * k)) & 1)) >> 1) << (8 * k)))),
        ("v_alignbit_b32", 0x14e, Class::Small, |a, b, c| ((((a as u64) << 32) | b as u64) >> (c & 31)) as u32),
        ("v_alignbyte_b32", 0x14f, Class::Small, |a, b, c| ((((a as u64) << 32) | b as u64) >> (8 * (c & 3))) as u32),
        ("v_min3_i32", 0x152, Class::U32, |a, b, c| (a as i32).min(b as i32).min(c as i32) as u32),
        ("v_min3_u32", 0x153, Class::U32, |a, b, c| a.min(b).min(c)),
        ("v_max3_i32", 0x155, Class::U32, |a, b, c| (a as i32).max(b as i32).max(c as i32) as u32),
        ("v_max3_u32", 0x156, Class::U32, |a, b, c| a.max(b).max(c)),
        ("v_med3_i32", 0x158, Class::U32, |a, b, c| med3u(a as i32, b as i32, c as i32) as u32),
        ("v_med3_u32", 0x159, Class::U32, |a, b, c| med3u(a, b, c)),
        ("v_sad_u8", 0x15a, Class::U32, |a, b, c| (0..4).map(|k| (((a >> (8 * k)) & 0xff) as i32 - ((b >> (8 * k)) & 0xff) as i32).unsigned_abs()).sum::<u32>().wrapping_add(c)),
        ("v_sad_hi_u8", 0x15b, Class::U32, |a, b, c| ((0..4).map(|k| (((a >> (8 * k)) & 0xff) as i32 - ((b >> (8 * k)) & 0xff) as i32).unsigned_abs()).sum::<u32>() << 16).wrapping_add(c)),
        ("v_sad_u16", 0x15c, Class::U32, |a, b, c| (0..2).map(|k| (((a >> (16 * k)) & 0xffff) as i32 - ((b >> (16 * k)) & 0xffff) as i32).unsigned_abs()).sum::<u32>().wrapping_add(c)),
        ("v_sad_u32", 0x15d, Class::U32, |a, b, c| ((a as i64 - b as i64).unsigned_abs() as u32).wrapping_add(c)),
        ("v_msad_u8", 0x171, Class::U32, |a, b, c| (0..4).map(|k| if (b >> (8 * k)) & 0xff == 0 { 0 } else { (((a >> (8 * k)) & 0xff) as i32 - ((b >> (8 * k)) & 0xff) as i32).unsigned_abs() }).sum::<u32>().wrapping_add(c)),
        ("v_mul_lo_u32", 0x169, Class::U32, |a, b, _| a.wrapping_mul(b)),
        ("v_mul_hi_u32", 0x16a, Class::U32, |a, b, _| ((a as u64 * b as u64) >> 32) as u32),
        ("v_mul_lo_i32", 0x16b, Class::U32, |a, b, _| (a as i32).wrapping_mul(b as i32) as u32),
        ("v_mul_hi_i32", 0x16c, Class::U32, |a, b, _| ((a as i32 as i64 * b as i32 as i64) >> 32) as u32),
        ("v_xor3_b32", 0x178, Class::U32, |a, b, c| a ^ b ^ c),
        ("v_perm_b32", 0x344, Class::U32, |a, b, c| (0..4).fold(0, |acc, k| acc | (perm_byte(((a as u64) << 32) | b as u64, (c >> (8 * k)) & 0xff) << (8 * k)))),
        ("v_xad_u32", 0x345, Class::U32, |a, b, c| (a ^ b).wrapping_add(c)),
        ("v_lshl_add_u32", 0x346, Class::Small, |a, b, c| (a << (b & 31)).wrapping_add(c)),
        ("v_add_lshl_u32", 0x347, Class::Small, |a, b, c| a.wrapping_add(b) << (c & 31)),
        ("v_bfm_b32", 0x363, Class::Small, |a, b, _| (((1u64 << (a & 31)) - 1) as u32) << (b & 31)),
        ("v_bcnt_u32_b32", 0x364, Class::U32, |a, b, _| a.count_ones().wrapping_add(b)),
        ("v_cvt_pk_u16_u32", 0x36a, Class::U32, |a, b, _| a.min(0xffff) | (b.min(0xffff) << 16)),
        ("v_cvt_pk_i16_i32", 0x36b, Class::U32, |a, b, _| ((a as i32).clamp(-32768, 32767) as u32 & 0xffff) | (((b as i32).clamp(-32768, 32767) as u32) << 16)),
        ("v_add3_u32", 0x36d, Class::U32, |a, b, c| a.wrapping_add(b).wrapping_add(c)),
        ("v_lshl_or_b32", 0x36f, Class::Small, |a, b, c| (a << (b & 31)) | c),
        ("v_and_or_b32", 0x371, Class::U32, |a, b, c| (a & b) | c),
        ("v_or3_b32", 0x372, Class::U32, |a, b, c| a | b | c),
        ("v_mad_u32_u16", 0x373, Class::U32, |a, b, c| (a & 0xffff).wrapping_mul(b & 0xffff).wrapping_add(c)),
        ("v_mad_i32_i16", 0x375, Class::U32, |a, b, c| ((a as i16 as i32) * (b as i16 as i32)).wrapping_add(c as i32) as u32),
        ("v_sub_nc_i32", 0x376, Class::U32, |a, b, _| a.wrapping_sub(b)),
        ("v_add_nc_i32", 0x37f, Class::U32, |a, b, _| a.wrapping_add(b)),
        ("v_pack_b32_f16", 0x311, Class::U32, |a, b, _| (a & 0xffff) | (b << 16)),
    ];
    for &(m, op, c, f) in ints {
        i3(s, m, op, c, f);
    }
    s.add("v_add_nc_i32", "VOP3", "clamp", Class::U32, &[], &vop3m(0x37f, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o((x.d[0] as i32).saturating_add(x.d[1] as i32) as u32));
    s.add("v_sub_nc_i32", "VOP3", "clamp", Class::U32, &[], &vop3m(0x376, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o((x.d[0] as i32).saturating_sub(x.d[1] as i32) as u32));
    s.add("v_pack_b32_f16", "VOP3", "opsel 0hi 1hi", Class::U32, &[], &vop3m(0x311, OUT, [v(10), v(11), 0], 0, 0, 0b0011, false), &[], &[Cmp::X], |x| o((x.d[0] >> 16) | (x.d[1] & 0xffff_0000)));
    s.add("v_pack_b32_f16", "VOP3", "neg0 neg1", Class::F16, &[], &vop3m(0x311, OUT, [v(10), v(11), 0], 0, 3, 0, false), &[], &[Cmp::X], |x| o(((x.d[0] & 0xffff) ^ 0x8000) | (((x.d[1] & 0xffff) ^ 0x8000) << 16)));
    for (m, op, f) in [("v_lshlrev_b64", 0x2ffu32, (|x: u64, n: u32| x << (n & 63)) as fn(u64, u32) -> u64), ("v_lshrrev_b64", 0x300, |x, n| x >> (n & 63)), ("v_ashrrev_i64", 0x301, |x, n| ((x as i64) >> (n & 63)) as u64)] {
        s.add(m, "VOP3", "v,v64", Class::Small, &[], &vop3(op, OUT, [v(10), v(12), 0]), &[], &[Cmp::X, Cmp::X], move |x| o64(f(x.d[2] as u64 | ((x.d[3] as u64) << 32), x.d[0])));
        s.add(m, "VOP3", "s,v64 wide", Class::U64, &[], &vop3(op, OUT, [14, v(12), 0]), &[], &[Cmp::X, Cmp::X], move |x| o64(f(x.d[2] as u64 | ((x.d[3] as u64) << 32), x.d[4])));
    }
    let i16s: &[(&'static str, u32, F3)] = &[
        ("v_add_nc_u16", 0x303, |a, b, _| a.wrapping_add(b)),
        ("v_sub_nc_u16", 0x304, |a, b, _| a.wrapping_sub(b)),
        ("v_mul_lo_u16", 0x305, |a, b, _| a.wrapping_mul(b)),
        ("v_lshrrev_b16", 0x307, |a, b, _| b >> (a & 15)),
        ("v_ashrrev_i16", 0x308, |a, b, _| ((b as u16 as i16) >> (a & 15)) as u16 as u32),
        ("v_max_u16", 0x309, |a, b, _| a.max(b)),
        ("v_max_i16", 0x30a, |a, b, _| (a as u16 as i16).max(b as u16 as i16) as u16 as u32),
        ("v_min_u16", 0x30b, |a, b, _| a.min(b)),
        ("v_min_i16", 0x30c, |a, b, _| (a as u16 as i16).min(b as u16 as i16) as u16 as u32),
        ("v_add_nc_i16", 0x30d, |a, b, _| (a as u16 as i16).wrapping_add(b as u16 as i16) as u16 as u32),
        ("v_sub_nc_i16", 0x30e, |a, b, _| (a as u16 as i16).wrapping_sub(b as u16 as i16) as u16 as u32),
        ("v_lshlrev_b16", 0x314, |a, b, _| b << (a & 15)),
        ("v_mad_u16", 0x340, |a, b, c| a.wrapping_mul(b).wrapping_add(c)),
        ("v_mad_i16", 0x35e, |a, b, c| (a as u16 as i16).wrapping_mul(b as u16 as i16).wrapping_add(c as u16 as i16) as u16 as u32),
        ("v_min3_i16", 0x352, |a, b, c| (a as u16 as i16).min(b as u16 as i16).min(c as u16 as i16) as u16 as u32),
        ("v_min3_u16", 0x353, |a, b, c| a.min(b).min(c)),
        ("v_max3_i16", 0x355, |a, b, c| (a as u16 as i16).max(b as u16 as i16).max(c as u16 as i16) as u16 as u32),
        ("v_max3_u16", 0x356, |a, b, c| a.max(b).max(c)),
        ("v_med3_i16", 0x358, |a, b, c| med3u(a as u16 as i16, b as u16 as i16, c as u16 as i16) as u16 as u32),
        ("v_med3_u16", 0x359, |a, b, c| med3u(a, b, c)),
    ];
    for &(m, op, f) in i16s {
        i16_3(s, m, op, f);
    }
    s.add("v_add_nc_u16", "VOP3", "clamp", Class::U32, &[vmov(OUT, v(13))], &vop3m(0x303, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o((x.d[3] & 0xffff_0000) | ((x.d[0] & 0xffff) + (x.d[1] & 0xffff)).min(0xffff)));
    s.add("v_sub_nc_u16", "VOP3", "clamp", Class::U32, &[vmov(OUT, v(13))], &vop3m(0x304, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o((x.d[3] & 0xffff_0000) | (x.d[0] & 0xffff).saturating_sub(x.d[1] & 0xffff)));
    s.add("v_add_nc_i16", "VOP3", "clamp", Class::U32, &[vmov(OUT, v(13))], &vop3m(0x30d, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o((x.d[3] & 0xffff_0000) | (x.d[0] as u16 as i16).saturating_add(x.d[1] as u16 as i16) as u16 as u32));
    s.add("v_sub_nc_i16", "VOP3", "clamp", Class::U32, &[vmov(OUT, v(13))], &vop3m(0x30e, OUT, [v(10), v(11), 0], 0, 0, 0, true), &[], &[Cmp::X], |x| o((x.d[3] & 0xffff_0000) | (x.d[0] as u16 as i16).saturating_sub(x.d[1] as u16 as i16) as u16 as u32));
    for (m, op, sub, rev) in [("v_add_co_u32", 0x30fu32, false, false), ("v_sub_co_u32", 0x310, true, false), ("v_subrev_co_u32", 0x319, true, true)] {
        let f = move |a: u32, b: u32| -> (u32, u32) {
            let (a, b) = if rev { (b, a) } else { (a, b) };
            if sub { (a.wrapping_sub(b), (b > a) as u32) } else { let r = a as u64 + b as u64; (r as u32, (r >> 32) as u32) }
        };
        s.add(m, "VOP3", "sdst=s42", Class::U32, &[], &vop3b(op, OUT, 42, [v(10), v(11), 0]), &mask_to(OUT + 1, 42), &[Cmp::X, Cmp::X], move |x| {
            let (r, c) = f(x.d[0], x.d[1]);
            o2(r, c)
        });
        s.add(m, "VOP3", "sdst=vcc s,v", Class::U32, &[], &vop3b(op, OUT, VCC, [13, v(11), 0]), &vcc_to(OUT + 1), &[Cmp::X, Cmp::X], move |x| {
            let (r, c) = f(x.d[3], x.d[1]);
            o2(r, c)
        });
        s.add(m, "VOP3", "clamp", Class::U32, &[], &[vop3b(op, OUT, 42, [v(10), v(11), 0])[0] | 0x8000, vop3b(op, OUT, 42, [v(10), v(11), 0])[1]], &mask_to(OUT + 1, 42), &[Cmp::X, Cmp::X], move |x| {
            let (r, c) = f(x.d[0], x.d[1]);
            o2(if c != 0 { if sub { 0 } else { u32::MAX } } else { r }, c)
        });
    }
    s.add("v_mad_u64_u32", "VOP3", "sdst=s42", Class::U32, &[], &vop3b(0x176, OUT, 42, [v(10), v(11), v(12)]), &mask_to(OUT + 2, 42), &[Cmp::X, Cmp::X, Cmp::X], |x| {
        let acc = x.d[2] as u128 | ((x.d[3] as u128) << 32);
        let r = x.d[0] as u128 * x.d[1] as u128 + acc;
        vec![Some(r as u32), Some((r >> 32) as u32), Some((r >> 64) as u32 & 1)]
    });
    s.add("v_mad_i64_i32", "VOP3", "sdst=s42", Class::U32, &[], &vop3b(0x177, OUT, 42, [v(10), v(11), v(12)]), &mask_to(OUT + 2, 42), &[Cmp::X, Cmp::X, Cmp::X], |x| {
        let acc = (x.d[2] as u64 | ((x.d[3] as u64) << 32)) as i64 as i128;
        let r = (x.d[0] as i32 as i128) * (x.d[1] as i32 as i128) + acc;
        vec![Some(r as u64 as u32), Some(((r as u64) >> 32) as u32), Some((r < 0) as u32)]
    });
    s.add("v_ldexp_f32", "VOP3", "v,v", Class::F32, &[], &vop3(0x362, OUT, [v(10), v(11), 0]), &[], &[Cmp::F32(0)], |x| o(if nan32(x.d[0]) { QNAN32 } else { ldexp32(f(x.d[0]), x.d[1] as i32).to_bits() }));
    s.add("v_ldexp_f32", "VOP3", "v,small", Class::Small, &[vop2(0x26, 32, v(11), 13)], &vop3(0x362, OUT, [v(12), v(32), 0]), &[], &[Cmp::F32(0)], |x| {
        let a = x.d[2];
        o(if nan32(a) { QNAN32 } else { ldexp32(f(a), x.d[1].wrapping_sub(x.d[3]) as i32).to_bits() })
    });
    let fl: &[(&'static str, u32, G3)] = &[
        ("v_mad_legacy_f32", 0x140, |a, b, c| unfused(a, b, c, true)),
        ("v_mad_f32", 0x141, |a, b, c| unfused(a, b, c, false)),
        ("v_fma_f32", 0x14b, fma32),
        ("v_min3_f32", 0x151, min3f),
        ("v_max3_f32", 0x154, max3f),
        ("v_med3_f32", 0x157, med3f),
        ("v_cubeid_f32", 0x144, |a, b, c| cube(a, b, c).map(|r| r.0.to_bits())),
        ("v_cubesc_f32", 0x145, |a, b, c| cube(a, b, c).map(|r| r.1.to_bits())),
        ("v_cubetc_f32", 0x146, |a, b, c| cube(a, b, c).map(|r| r.2.to_bits())),
        ("v_cubema_f32", 0x147, |a, b, c| cube(a, b, c).map(|r| f32q(r.3))),
    ];
    for &(m, op, g) in fl {
        f3(s, m, op, g);
    }
    s.add("v_cvt_pk_u8_f32", "VOP3", "v,v,v", Class::F32, &[vop2(0x1b, 32, ic(3), 13)], &vop3(0x15e, OUT, [v(10), v(32), v(12)]), &[], &[Cmp::X], |x| on(pk_u8(x.d[0], x.d[3], x.d[2])));
    s.add("v_cvt_pknorm_i16_f32", "VOP3", "v,v", Class::F32, &[], &vop3(0x368, OUT, [v(10), v(11), 0]), &[], &[Cmp::X], |x| on(norm16(f(x.d[0]) as f64, true, nan32(x.d[0])).zip(norm16(f(x.d[1]) as f64, true, nan32(x.d[1]))).map(|(a, b)| a | (b << 16))));
    s.add("v_cvt_pknorm_u16_f32", "VOP3", "v,v", Class::F32, &[], &vop3(0x369, OUT, [v(10), v(11), 0]), &[], &[Cmp::X], |x| on(norm16(f(x.d[0]) as f64, false, nan32(x.d[0])).zip(norm16(f(x.d[1]) as f64, false, nan32(x.d[1]))).map(|(a, b)| a | (b << 16))));
    s.add("v_cvt_pknorm_i16_f16", "VOP3", "v,v", Class::F16, &[], &vop3(0x312, OUT, [v(10), v(11), 0]), &[], &[Cmp::X], |x| on(norm16(h(x.d[0]), true, nan16(x.d[0])).zip(norm16(h(x.d[1]), true, nan16(x.d[1]))).map(|(a, b)| a | (b << 16))));
    s.add("v_cvt_pknorm_u16_f16", "VOP3", "v,v", Class::F16, &[], &vop3(0x313, OUT, [v(10), v(11), 0]), &[], &[Cmp::X], |x| on(norm16(h(x.d[0]), false, nan16(x.d[0])).zip(norm16(h(x.d[1]), false, nan16(x.d[1]))).map(|(a, b)| a | (b << 16))));
    s.add("v_cvt_pknorm_i16_f16", "VOP3", "opsel hi,hi", Class::F16, &[], &vop3m(0x312, OUT, [v(10), v(11), 0], 0, 0, 0b0011, false), &[], &[Cmp::X], |x| on(norm16(h(x.d[0] >> 16), true, nan16(x.d[0] >> 16)).zip(norm16(h(x.d[1] >> 16), true, nan16(x.d[1] >> 16))).map(|(a, b)| a | (b << 16))));
    let hs: &[(&'static str, u32, G3)] = &[("v_fma_f16", 0x34b, hfma), ("v_min3_f16", 0x351, min3h), ("v_max3_f16", 0x354, max3h), ("v_med3_f16", 0x357, med3h)];
    for &(m, op, g) in hs {
        h3(s, m, op, g);
    }
    let f64s: &[(&'static str, u32, fn(u64, u64) -> Option<u64>)] = &[
        ("v_add_f64", 0x164, |a, b| Some(f64q(f64::from_bits(a) + f64::from_bits(b)))),
        ("v_mul_f64", 0x165, |a, b| Some(f64q(f64::from_bits(a) * f64::from_bits(b)))),
        ("v_min_f64", 0x166, min_num64),
        ("v_max_f64", 0x167, max_num64),
    ];
    for &(m, op, g) in f64s {
        let w = |x: &In, k: usize| x.d[2 * k] as u64 | ((x.d[2 * k + 1] as u64) << 32);
        s.add(m, "VOP3", "v,v", Class::F64, &[], &vop3(op, OUT, [v(10), v(12), 0]), &[], &[Cmp::F64(0), Cmp::F64Hi], move |x| g(w(x, 0), w(x, 1)).map_or(vec![None, None], o64));
        s.add(m, "VOP3", "neg0 abs1", Class::F64, &[], &vop3o(op, OUT, [v(10), v(12), 0], 2, 1, false, 0), &[], &[Cmp::F64(0), Cmp::F64Hi], move |x| g(w(x, 0) ^ (1 << 63), w(x, 1) & !(1 << 63)).map_or(vec![None, None], o64));
        s.add(m, "VOP3", "clamp", Class::F64, &[], &vop3o(op, OUT, [v(10), v(12), 0], 0, 0, true, 0), &[], &[Cmp::F64(0), Cmp::F64Hi], move |x| {
            g(w(x, 0), w(x, 1))
                .and_then(|r| {
                    let y = f64::from_bits(r);
                    if y.is_nan() || (y == 0.0 && r != 0) { None } else { Some(if y <= 0.0 { 0 } else if y >= 1.0 { 1f64.to_bits() } else { r }) }
                })
                .map_or(vec![None, None], o64)
        });
    }
    s.add("v_fma_f64", "VOP3", "v,v,v", Class::F64, &[], &vop3(0x14c, OUT, [v(10), v(12), v(14)]), &[], &[Cmp::F64(0), Cmp::F64Hi], |x| o64(f64q(d(x.d[0], x.d[1]).mul_add(d(x.d[2], x.d[3]), d(x.d[4], x.d[5])))));
    s.add("v_fma_f64", "VOP3", "neg2 abs0", Class::F64, &[], &vop3o(0x14c, OUT, [v(10), v(12), v(14)], 1, 4, false, 0), &[], &[Cmp::F64(0), Cmp::F64Hi], |x| o64(f64q(d(x.d[0], x.d[1]).abs().mul_add(d(x.d[2], x.d[3]), -d(x.d[4], x.d[5])))));
    s.add("v_ldexp_f64", "VOP3", "v,v", Class::F64, &[], &vop3(0x168, OUT, [v(10), v(12), 0]), &[], &[Cmp::F64(0), Cmp::F64Hi], |x| {
        let a = d(x.d[0], x.d[1]);
        o64(if a.is_nan() { QNAN64 } else { ldexp64(a, x.d[2] as i32).to_bits() })
    });
    s.add("v_ldexp_f64", "VOP3", "v,small", Class::F64, &[vop2(0x1b, 32, LIT, 12), 0x7ff, vop2(0x27, 32, LIT, 32), 0x400], &vop3(0x168, OUT, [v(10), v(32), 0]), &[], &[Cmp::F64(0), Cmp::F64Hi], |x| {
        let a = d(x.d[0], x.d[1]);
        o64(if a.is_nan() { QNAN64 } else { ldexp64(a, ((x.d[2] & 0x7ff) as i32) - 0x400).to_bits() })
    });
    s.add("v_div_fixup_f64", "VOP3", "v,v,v", Class::F64, &[], &vop3(0x160, OUT, [v(10), v(12), v(14)]), &[], &[Cmp::F64(0), Cmp::F64Hi], |x| div_fixup(d(x.d[0], x.d[1]), d(x.d[2], x.d[3]), d(x.d[4], x.d[5])).map_or(vec![None, None], o64));
    s.add("v_div_fmas_f64", "VOP3", "vcc=0", Class::F64, &[], &vop3(0x170, OUT, [v(10), v(12), v(14)]), &[], &[Cmp::F64(0), Cmp::F64Hi], |x| o64(f64q(d(x.d[0], x.d[1]).mul_add(d(x.d[2], x.d[3]), d(x.d[4], x.d[5])))));
    s.add("v_div_scale_f64", "VOP3", "normal range, sdst=vcc", Class::F64, &[], &vop3b(0x16e, OUT, VCC, [v(10), v(12), v(14)]), &vcc_to(OUT + 2), &[Cmp::F64(0), Cmp::F64Hi, Cmp::X], |x| {
        let (s0, s1, s2) = (d(x.d[0], x.d[1]), d(x.d[2], x.d[3]), d(x.d[4], x.d[5]));
        let e = |v: f64| ((v.to_bits() >> 52) & 0x7ff) as i32;
        let ok = [s0, s1, s2].iter().all(|v| v.is_normal()) && e(s2) - e(s1) < 700 && e(s2) - e(s1) > -700 && e(s2) > 100 && e(s1) > 100 && e(s1) < 1900 && e(s2) < 1900;
        if ok { vec![Some(x.d[0]), Some(x.d[1]), Some(0)] } else { vec![None, None, None] }
    });
    vop3p_ops(s);
    vop3p_int_ops(s);
}

fn pk_h(a: u32, sel_hi: bool, neg: bool) -> u32 {
    let v = if sel_hi { a >> 16 } else { a & 0xffff };
    if neg { v ^ 0x8000 } else { v }
}

fn vop3p_ops(s: &mut Suite) {
    let ops: &[(&'static str, u32, u32, G3)] = &[
        ("v_pk_fma_f16", 0x0e, 3, hfma),
        ("v_pk_add_f16", 0x0f, 2, |a, b, _| hadd(a, b)),
        ("v_pk_mul_f16", 0x10, 2, |a, b, _| hmul(a, b)),
        ("v_pk_min_f16", 0x11, 2, |a, b, _| hmin(a, b)),
        ("v_pk_max_f16", 0x12, 2, |a, b, _| hmax(a, b)),
    ];
    let mods: [(&str, u32, u32, u32, u32, bool); 4] = [("default", 0, 7, 0, 0, false), ("swap halves", 0b111, 0b000, 0, 0, false), ("neg_lo0 neg_hi1", 0b001, 0b110, 1, 2, false), ("clamp", 0, 7, 0, 0, true)];
    for &(m, op, n, g) in ops {
        for &(name, opsel, opsel_hi, nl, nh, cl) in &mods {
            let srcs = if n == 3 { [v(10), v(11), v(12)] } else { [v(10), v(11), 0] };
            s.add(m, "VOP3P", name, Class::F16, &[], &vop3p(op, OUT, srcs, opsel, opsel_hi, nl, nh, cl), &[], &[Cmp::Pk16(0)], move |x| {
                let lane = |k: u32, hi_half: bool| -> u32 {
                    let (sel, neg) = if hi_half { ((opsel_hi >> k) & 1 != 0, (nh >> k) & 1 != 0) } else { ((opsel >> k) & 1 != 0, (nl >> k) & 1 != 0) };
                    pk_h(x.d[k as usize], sel, neg)
                };
                let lo = g(lane(0, false), lane(1, false), lane(2, false)).and_then(|r| out16(r, cl));
                let hi = g(lane(0, true), lane(1, true), lane(2, true)).and_then(|r| out16(r, cl));
                on(lo.zip(hi).map(|(l, h)| l | (h << 16)))
            });
        }
    }
    let mixes: [(&str, u32, u32, u32); 4] = [("all f32", 0, 0, 0), ("all f16 lo", 0, 0b111, 0), ("f16 hi src0, f32 src1, f16 lo src2 neg1", 0b001, 0b101, 0b010), ("f16 src1 hi src2 hi", 0b110, 0b110, 0)];
    for (name, opsel, opsel_hi, neg) in mixes {
        let src = move |x: &In, k: usize| -> f64 {
            let raw = x.d[k];
            let v = if (opsel_hi >> k) & 1 != 0 { h2d(if (opsel >> k) & 1 != 0 { raw >> 16 } else { raw & 0xffff }) } else { f(raw) as f64 };
            if (neg >> k) & 1 != 0 { -v } else { v }
        };
        let class = Class::F16;
        s.add("v_fma_mix_f32", "VOP3P", name, class, &[], &vop3p(0x20, OUT, [v(10), v(11), v(12)], opsel, opsel_hi, neg, 0, false), &[], &[Cmp::F32(0)], move |x| {
            let (a, b, c) = (src(x, 0), src(x, 1), src(x, 2));
            let r = (a as f32).mul_add(b as f32, c as f32);
            on(if [a, b, c].iter().any(|v| v.is_nan()) { Some(QNAN32) } else if den32(r.to_bits()) || [a, b, c].iter().any(|v| den32((*v as f32).to_bits())) { None } else { Some(f32q(r)) })
        });
        for (m, op, hi) in [("v_fma_mixlo_f16", 0x21u32, false), ("v_fma_mixhi_f16", 0x22, true)] {
            s.add(m, "VOP3P", name, class, &[vmov(OUT, v(13))], &vop3p(op, OUT, [v(10), v(11), v(12)], opsel, opsel_hi, neg, 0, false), &[], &[if hi { Cmp::F16H(0) } else { Cmp::F16(0) }], move |x| {
                let (a, b, c) = (src(x, 0), src(x, 1), src(x, 2));
                if [a, b, c].iter().any(|v| v.is_nan() || den32((*v as f32).to_bits())) {
                    return vec![None];
                }
                let exact = a * b + c;
                if (a * b) - (exact - c) != 0.0 && exact.is_finite() {
                    return vec![None];
                }
                let r = d2h(exact);
                if den16(r) {
                    return vec![None];
                }
                o(if hi { (x.d[3] & 0xffff) | (r << 16) } else { (x.d[3] & 0xffff_0000) | r })
            });
        }
    }
}

fn vop3p_int_ops(s: &mut Suite) {
    type P = fn(i64, i64, i64) -> i64;
    let ops: &[(&'static str, u32, u32, bool, bool, P)] = &[
        ("v_pk_mad_i16", 0x00, 3, true, false, |a, b, c| a * b + c),
        ("v_pk_mul_lo_u16", 0x01, 2, false, false, |a, b, _| a * b),
        ("v_pk_add_i16", 0x02, 2, true, true, |a, b, _| a + b),
        ("v_pk_sub_i16", 0x03, 2, true, true, |a, b, _| a - b),
        ("v_pk_lshlrev_b16", 0x04, 2, false, false, |a, b, _| b << (a & 15)),
        ("v_pk_lshrrev_b16", 0x05, 2, false, false, |a, b, _| b >> (a & 15)),
        ("v_pk_ashrrev_i16", 0x06, 2, true, false, |a, b, _| b >> (a & 15)),
        ("v_pk_max_i16", 0x07, 2, true, true, |a, b, _| a.max(b)),
        ("v_pk_min_i16", 0x08, 2, true, true, |a, b, _| a.min(b)),
        ("v_pk_mad_u16", 0x09, 3, false, false, |a, b, c| a * b + c),
        ("v_pk_add_u16", 0x0a, 2, false, true, |a, b, _| a + b),
        ("v_pk_sub_u16", 0x0b, 2, false, true, |a, b, _| a - b),
        ("v_pk_max_u16", 0x0c, 2, false, true, |a, b, _| a.max(b)),
        ("v_pk_min_u16", 0x0d, 2, false, true, |a, b, _| a.min(b)),
    ];
    let mods: [(&str, u32, u32, bool); 4] = [("default", 0, 7, false), ("swap halves", 0b111, 0b000, false), ("mixed halves", 0b010, 0b101, false), ("clamp", 0, 7, true)];
    for &(m, op, n, signed, clampable, g) in ops {
        for &(name, opsel, opsel_hi, cl) in &mods {
            if cl && !clampable {
                continue;
            }
            let srcs = if n == 3 { [v(10), v(11), v(12)] } else { [v(10), v(11), 0] };
            s.add(m, "VOP3P", name, Class::U32, &[], &vop3p(op, OUT, srcs, opsel, opsel_hi, 0, 0, cl), &[], &[Cmp::X], move |x| {
                let half = |k: usize, hi: bool| -> i64 {
                    let sel = if hi { (opsel_hi >> k) & 1 } else { (opsel >> k) & 1 } != 0;
                    let u = if sel { x.d[k] >> 16 } else { x.d[k] & 0xffff };
                    if signed { u as u16 as i16 as i64 } else { u as i64 }
                };
                let res = |hi: bool| -> u32 {
                    let r = g(half(0, hi), half(1, hi), half(2, hi));
                    let r = match (cl, signed) {
                        (false, _) => r,
                        (true, true) => r.clamp(-32768, 32767),
                        (true, false) => r.clamp(0, 0xffff),
                    };
                    (r as u32) & 0xffff
                };
                o(res(false) | (res(true) << 16))
            });
        }
    }
    s.add("v_pk_add_u16", "VOP3P", "inline -1, op_sel_hi[1]=0", Class::U32, &[], &vop3p(0x0a, OUT, [v(10), 193, 0], 0, 0b001, 0, 0, false), &[], &[Cmp::X], |x| {
        o((x.d[0].wrapping_sub(1) & 0xffff) | (((x.d[0] >> 16).wrapping_sub(1) & 0xffff) << 16))
    });
}

pub fn add(s: &mut Suite) {
    vop2_ops(s);
    vop1_ops(s);
    sdwa_ops(s);
    vop3_ops(s);
    norm_edges(s);
    for (m, why) in [
        ("v_readfirstlane_b32", "cross-lane: per-lane record harness cannot observe; lane model treats it as a copy (used by every SALU case)"),
        ("v_readlane_b32", "cross-lane; covered by tests/exec_vop/lanes.rs"),
        ("v_writelane_b32", "cross-lane; covered by tests/exec_vop/lanes.rs"),
        ("v_permlane16_b32", "cross-lane; covered by tests/exec_vop/lanes.rs"),
        ("v_permlanex16_b32", "cross-lane; covered by tests/exec_vop/lanes.rs"),
        ("v_mbcnt_lo_u32_b32", "depends on the lane index within the wave"),
        ("v_mbcnt_hi_u32_b32", "depends on the lane index within the wave"),
        ("v_trig_preop_f64", "needs the 1201-bit 2/pi ROM table, not modelled"),
        ("v_mullit_f32", "legacy lit semantics undocumented in the RDNA2 ISA"),
    ] {
        s.skip(m, if m.starts_with("v_read") && m.contains("first") { "VOP1" } else { "VOP3" }, why);
    }
}
