use super::*;

const D: u32 = 42;

fn nz(x: u64) -> u32 {
    (x != 0) as u32
}

fn w64(x: &In, k: usize) -> u64 {
    x.d[2 * k] as u64 | ((x.d[2 * k + 1] as u64) << 32)
}

fn scc_in(x: &In) -> u32 {
    x.d[7] & 1
}

fn post32() -> Vec<u32> {
    let mut w = vec![s2v(OUT, D)];
    w.extend(scc_to(OUT + 1));
    w
}

fn post64() -> Vec<u32> {
    let mut w = vec![s2v(OUT, D), s2v(OUT + 1, D + 1)];
    w.extend(scc_to(OUT + 2));
    w
}

fn bfe64(a: u64, b: u32, signed: bool, bits: u32) -> u64 {
    let o = b & (bits - 1);
    let w = (b >> 16) & 127;
    let all = if bits == 32 { 0xffff_ffffu64 } else { u64::MAX };
    let x = if signed {
        let sa = if bits == 32 { a as u32 as i32 as i64 } else { a as i64 };
        (sa >> o) as u64
    } else {
        (a & all) >> o
    };
    if w == 0 {
        return 0;
    }
    if w >= bits {
        return x & all;
    }
    let mask = (1u64 << w) - 1;
    let v = x & mask;
    let r = if signed && (v >> (w - 1)) & 1 != 0 { v | !mask } else { v };
    r & all
}

fn wqm(x: u64, n: u32) -> u64 {
    (0..n / 4).fold(0u64, |acc, i| if (x >> (i * 4)) & 0xf != 0 { acc | (0xf << (i * 4)) } else { acc })
}

fn ff(x: u64, bits: u32) -> u32 {
    let x = if bits == 32 { x & 0xffff_ffff } else { x };
    if x == 0 { u32::MAX } else { x.trailing_zeros() }
}

fn flbit_s(x: u64, bits: u32) -> u32 {
    let all = if bits == 32 { 0xffff_ffff } else { u64::MAX };
    let x = x & all;
    if x == 0 || x == all {
        return u32::MAX;
    }
    let neg = (x >> (bits - 1)) & 1 != 0;
    let y = if neg { !x & all } else { x };
    y.leading_zeros() - (64 - bits)
}

type S2 = fn(u32, u32, u32) -> (u32, Option<u32>);
type S2W = fn(u64, u64, u32) -> (u64, Option<u32>);

fn sop2_ops(s: &mut Suite) {
    let ops: &[(&'static str, u32, Class, S2)] = &[
        ("s_add_u32", 0x00, Class::U32, |a, b, _| { let r = a as u64 + b as u64; (r as u32, Some((r >> 32) as u32)) }),
        ("s_sub_u32", 0x01, Class::U32, |a, b, _| (a.wrapping_sub(b), Some((b > a) as u32))),
        ("s_add_i32", 0x02, Class::U32, |a, b, _| (a.wrapping_add(b), Some((a as i32).checked_add(b as i32).is_none() as u32))),
        ("s_sub_i32", 0x03, Class::U32, |a, b, _| (a.wrapping_sub(b), Some((a as i32).checked_sub(b as i32).is_none() as u32))),
        ("s_addc_u32", 0x04, Class::U32, |a, b, c| { let r = a as u64 + b as u64 + c as u64; (r as u32, Some((r >> 32) as u32)) }),
        ("s_subb_u32", 0x05, Class::U32, |a, b, c| (a.wrapping_sub(b).wrapping_sub(c), Some(((b as u64 + c as u64) > a as u64) as u32))),
        ("s_min_i32", 0x06, Class::U32, |a, b, _| ((a as i32).min(b as i32) as u32, Some(((a as i32) < (b as i32)) as u32))),
        ("s_min_u32", 0x07, Class::U32, |a, b, _| (a.min(b), Some((a < b) as u32))),
        ("s_max_i32", 0x08, Class::U32, |a, b, _| ((a as i32).max(b as i32) as u32, Some(((a as i32) > (b as i32)) as u32))),
        ("s_max_u32", 0x09, Class::U32, |a, b, _| (a.max(b), Some((a > b) as u32))),
        ("s_cselect_b32", 0x0a, Class::U32, |a, b, c| (if c != 0 { a } else { b }, None)),
        ("s_and_b32", 0x0e, Class::U32, |a, b, _| (a & b, Some(nz((a & b) as u64)))),
        ("s_or_b32", 0x10, Class::U32, |a, b, _| (a | b, Some(nz((a | b) as u64)))),
        ("s_xor_b32", 0x12, Class::U32, |a, b, _| (a ^ b, Some(nz((a ^ b) as u64)))),
        ("s_andn2_b32", 0x14, Class::U32, |a, b, _| (a & !b, Some(nz((a & !b) as u64)))),
        ("s_orn2_b32", 0x16, Class::U32, |a, b, _| (a | !b, Some(nz((a | !b) as u64)))),
        ("s_nand_b32", 0x18, Class::U32, |a, b, _| (!(a & b), Some(nz(!(a & b) as u64)))),
        ("s_nor_b32", 0x1a, Class::U32, |a, b, _| (!(a | b), Some(nz(!(a | b) as u64)))),
        ("s_xnor_b32", 0x1c, Class::U32, |a, b, _| (!(a ^ b), Some(nz(!(a ^ b) as u64)))),
        ("s_lshl_b32", 0x1e, Class::Small, |a, b, _| { let r = a << (b & 31); (r, Some(nz(r as u64))) }),
        ("s_lshr_b32", 0x20, Class::Small, |a, b, _| { let r = a >> (b & 31); (r, Some(nz(r as u64))) }),
        ("s_ashr_i32", 0x22, Class::Small, |a, b, _| { let r = ((a as i32) >> (b & 31)) as u32; (r, Some(nz(r as u64))) }),
        ("s_bfm_b32", 0x24, Class::Small, |a, b, _| ((((1u64 << (a & 31)) - 1) as u32) << (b & 31), None)),
        ("s_mul_i32", 0x26, Class::U32, |a, b, _| ((a as i32).wrapping_mul(b as i32) as u32, None)),
        ("s_absdiff_i32", 0x2c, Class::U32, |a, b, _| { let r = (a as i32).wrapping_sub(b as i32).wrapping_abs() as u32; (r, Some(nz(r as u64))) }),
        ("s_lshl1_add_u32", 0x2e, Class::U32, |a, b, _| { let r = ((a as u64) << 1) + b as u64; (r as u32, Some((r >> 32 != 0) as u32)) }),
        ("s_lshl2_add_u32", 0x2f, Class::U32, |a, b, _| { let r = ((a as u64) << 2) + b as u64; (r as u32, Some((r >> 32 != 0) as u32)) }),
        ("s_lshl3_add_u32", 0x30, Class::U32, |a, b, _| { let r = ((a as u64) << 3) + b as u64; (r as u32, Some((r >> 32 != 0) as u32)) }),
        ("s_lshl4_add_u32", 0x31, Class::U32, |a, b, _| { let r = ((a as u64) << 4) + b as u64; (r as u32, Some((r >> 32 != 0) as u32)) }),
        ("s_pack_ll_b32_b16", 0x32, Class::U32, |a, b, _| ((a & 0xffff) | (b << 16), None)),
        ("s_pack_lh_b32_b16", 0x33, Class::U32, |a, b, _| ((a & 0xffff) | (b & 0xffff_0000), None)),
        ("s_pack_hh_b32_b16", 0x34, Class::U32, |a, b, _| ((a >> 16) | (b & 0xffff_0000), None)),
        ("s_mul_hi_u32", 0x35, Class::U32, |a, b, _| (((a as u64 * b as u64) >> 32) as u32, None)),
        ("s_mul_hi_i32", 0x36, Class::U32, |a, b, _| (((a as i32 as i64 * b as i32 as i64) >> 32) as u32, None)),
    ];
    for &(m, op, class, f) in ops {
        for (name, a, b) in [("s,s", 10u32, 11u32), ("s,s alt", 12, 13)] {
            let (ia, ib) = ((a - 10) as usize, (b - 10) as usize);
            s.add(m, "SOP2", name, class, &scc_from(17), &[sop2(op, D, a, b)], &post32(), &[Cmp::X, Cmp::X], move |x| {
                let c = scc_in(x);
                let (r, sc) = f(x.d[ia], x.d[ib], c);
                o2(r, sc.unwrap_or(c))
            });
        }
        if class == Class::Small {
            s.add(m, "SOP2", "s,inline 31", class, &scc_from(17), &[sop2(op, D, 10, ic(31))], &post32(), &[Cmp::X, Cmp::X], move |x| {
                let c = scc_in(x);
                let (r, sc) = f(x.d[0], 31, c);
                o2(r, sc.unwrap_or(c))
            });
            s.add(m, "SOP2", "s,s big shift", Class::U32, &scc_from(17), &[sop2(op, D, 10, 11)], &post32(), &[Cmp::X, Cmp::X], move |x| {
                let c = scc_in(x);
                let (r, sc) = f(x.d[0], x.d[1], c);
                o2(r, sc.unwrap_or(c))
            });
        }
    }
    for (m, op, signed) in [("s_bfe_u32", 0x27u32, false), ("s_bfe_i32", 0x28, true)] {
        for (name, k) in [("s,{w,o}", 0x007f_001fu32), ("s,{w,o} reserved bits set", u32::MAX)] {
            s.add(m, "SOP2", name, Class::U32, &[sop2(0x0e, 44, 11, LIT), k], &[sop2(op, D, 10, 44)], &post32(), &[Cmp::X, Cmp::X], move |x| {
                let r = bfe64(x.d[0] as u64, x.d[1] & k, signed, 32);
                o2(r as u32, nz(r))
            });
        }
        for w in [0u32, 1, 31, 32, 33, 63, 64, 65, 127] {
            s.add(m, "SOP2", &format!("s,{{w={w},o=lane}}"), Class::U32, &[sop2(0x0e, 44, 11, ic(31)), sop2(0x10, 44, 44, LIT), w << 16], &[sop2(op, D, 10, 44)], &post32(), &[Cmp::X, Cmp::X], move |x| {
                let r = bfe64(x.d[0] as u64, (x.d[1] & 31) | (w << 16), signed, 32);
                o2(r as u32, nz(r))
            });
        }
    }
    for (m, op, signed) in [("s_bfe_u64", 0x29u32, false), ("s_bfe_i64", 0x2a, true)] {
        for (name, k) in [("s64,{w,o}", 0x007f_003fu32), ("s64,{w,o} reserved bits set", u32::MAX)] {
            s.add(m, "SOP2", name, Class::U64, &[sop2(0x0e, 44, 12, LIT), k], &[sop2(op, D, 10, 44)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], move |x| {
                let r = bfe64(w64(x, 0), x.d[2] & k, signed, 64);
                vec![Some(r as u32), Some((r >> 32) as u32), Some(nz(r))]
            });
        }
        for w in [0u32, 1, 31, 32, 33, 63, 64, 65, 127] {
            s.add(m, "SOP2", &format!("s64,{{w={w},o=lane}}"), Class::U64, &[sop2(0x0e, 44, 12, LIT), 0x3f, sop2(0x10, 44, 44, LIT), w << 16], &[sop2(op, D, 10, 44)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], move |x| {
                let r = bfe64(w64(x, 0), (x.d[2] & 63) | (w << 16), signed, 64);
                vec![Some(r as u32), Some((r >> 32) as u32), Some(nz(r))]
            });
        }
    }
    let w: &[(&'static str, u32, Class, S2W)] = &[
        ("s_cselect_b64", 0x0b, Class::U64, |a, b, c| (if c != 0 { a } else { b }, None)),
        ("s_and_b64", 0x0f, Class::U64, |a, b, _| (a & b, Some(nz(a & b)))),
        ("s_or_b64", 0x11, Class::U64, |a, b, _| (a | b, Some(nz(a | b)))),
        ("s_xor_b64", 0x13, Class::U64, |a, b, _| (a ^ b, Some(nz(a ^ b)))),
        ("s_andn2_b64", 0x15, Class::U64, |a, b, _| (a & !b, Some(nz(a & !b)))),
        ("s_orn2_b64", 0x17, Class::U64, |a, b, _| (a | !b, Some(nz(a | !b)))),
        ("s_nand_b64", 0x19, Class::U64, |a, b, _| (!(a & b), Some(nz(!(a & b))))),
        ("s_nor_b64", 0x1b, Class::U64, |a, b, _| (!(a | b), Some(nz(!(a | b))))),
        ("s_xnor_b64", 0x1d, Class::U64, |a, b, _| (!(a ^ b), Some(nz(!(a ^ b))))),
    ];
    for &(m, op, class, f) in w {
        s.add(m, "SOP2", "s64,s64", class, &scc_from(17), &[sop2(op, D, 10, 12)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], move |x| {
            let c = scc_in(x);
            let (r, sc) = f(w64(x, 0), w64(x, 1), c);
            vec![Some(r as u32), Some((r >> 32) as u32), Some(sc.unwrap_or(c))]
        });
    }
    for (m, op) in [("s_lshl_b64", 0x1fu32), ("s_lshr_b64", 0x21), ("s_ashr_i64", 0x23)] {
        let f = move |a: u64, b: u32| -> u64 {
            match op {
                0x1f => a << (b & 63),
                0x21 => a >> (b & 63),
                _ => ((a as i64) >> (b & 63)) as u64,
            }
        };
        s.add(m, "SOP2", "s64,s", Class::U64, &scc_from(17), &[sop2(op, D, 10, 12)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], move |x| {
            let r = f(w64(x, 0), x.d[2]);
            vec![Some(r as u32), Some((r >> 32) as u32), Some(nz(r))]
        });
        s.add(m, "SOP2", "s64,small", Class::U64, &[sop2(0x0e, 44, 14, ic(63))], &[sop2(op, D, 10, 44)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], move |x| {
            let r = f(w64(x, 0), x.d[4] & 63);
            vec![Some(r as u32), Some((r >> 32) as u32), Some(nz(r))]
        });
    }
    s.add("s_bfm_b64", "SOP2", "s,s", Class::U64, &scc_from(17), &[sop2(0x25, D, 10, 11)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], |x| {
        let r = (((1u128 << (x.d[0] & 63)) - 1) as u64) << (x.d[1] & 63);
        vec![Some(r as u32), Some((r >> 32) as u32), Some(scc_in(x))]
    });
}

type S1 = fn(u32, u32, u32) -> (u32, Option<u32>);

fn sop1_ops(s: &mut Suite) {
    let ops: &[(&'static str, u32, S1)] = &[
        ("s_mov_b32", 0x03, |a, _, _| (a, None)),
        ("s_cmov_b32", 0x05, |a, d, c| (if c != 0 { a } else { d }, None)),
        ("s_not_b32", 0x07, |a, _, _| (!a, Some(nz((!a) as u64)))),
        ("s_wqm_b32", 0x09, |a, _, _| { let r = wqm(a as u64, 32) as u32; (r, Some(nz(r as u64))) }),
        ("s_brev_b32", 0x0b, |a, _, _| (a.reverse_bits(), None)),
        ("s_bcnt0_i32_b32", 0x0d, |a, _, _| (a.count_zeros(), Some(nz(a.count_zeros() as u64)))),
        ("s_bcnt1_i32_b32", 0x0f, |a, _, _| (a.count_ones(), Some(nz(a.count_ones() as u64)))),
        ("s_ff0_i32_b32", 0x11, |a, _, _| (ff(!a as u64, 32), None)),
        ("s_ff1_i32_b32", 0x13, |a, _, _| (ff(a as u64, 32), None)),
        ("s_flbit_i32_b32", 0x15, |a, _, _| (if a == 0 { u32::MAX } else { a.leading_zeros() }, None)),
        ("s_flbit_i32", 0x17, |a, _, _| (flbit_s(a as u64, 32), None)),
        ("s_sext_i32_i8", 0x19, |a, _, _| (a as u8 as i8 as i32 as u32, None)),
        ("s_sext_i32_i16", 0x1a, |a, _, _| (a as u16 as i16 as i32 as u32, None)),
        ("s_bitset0_b32", 0x1b, |a, d, _| (d & !(1 << (a & 31)), None)),
        ("s_bitset1_b32", 0x1d, |a, d, _| (d | (1 << (a & 31)), None)),
        ("s_abs_i32", 0x34, |a, _, _| { let r = (a as i32).wrapping_abs() as u32; (r, Some(nz(r as u64))) }),
    ];
    for &(m, op, f) in ops {
        let class = if matches!(op, 0x1b | 0x1d) { Class::Small } else { Class::U32 };
        s.add(m, "SOP1", "s", class, &[sop1(3, D, 11), sop2(0x0e, 41, 17, ic(1))], &[sop1(op, D, 10)], &post32(), &[Cmp::X, Cmp::X], move |x| {
            let c = scc_in(x);
            let (r, sc) = f(x.d[0], x.d[1], c);
            o2(r, sc.unwrap_or(c))
        });
    }
    s.add("s_mov_b32", "SOP1", "literal", Class::U32, &[], &[sop1(3, D, LIT), 0x89ab_cdef], &[s2v(OUT, D)], &[Cmp::X], |_| o(0x89ab_cdef));
    s.add("s_mov_b32", "SOP1", "inline -16", Class::U32, &[], &[sop1(3, D, ic(-16))], &[s2v(OUT, D)], &[Cmp::X], |_| o(0xffff_fff0));
    type S1W = fn(u64, u64, u32) -> (u64, Option<u32>);
    let wops: &[(&'static str, u32, S1W)] = &[
        ("s_mov_b64", 0x04, |a, _, _| (a, None)),
        ("s_cmov_b64", 0x06, |a, d, c| (if c != 0 { a } else { d }, None)),
        ("s_not_b64", 0x08, |a, _, _| (!a, Some(nz(!a)))),
        ("s_wqm_b64", 0x0a, |a, _, _| { let r = wqm(a, 64); (r, Some(nz(r))) }),
        ("s_brev_b64", 0x0c, |a, _, _| (a.reverse_bits(), None)),
        ("s_bitset0_b64", 0x1c, |a, d, _| (d & !(1u64 << (a & 63)), None)),
        ("s_bitset1_b64", 0x1e, |a, d, _| (d | (1u64 << (a & 63)), None)),
    ];
    for &(m, op, f) in wops {
        let bit = matches!(op, 0x1c | 0x1e);
        let src = if bit { 14 } else { 10 };
        let class = if bit { Class::Small } else { Class::U64 };
        s.add(m, "SOP1", "s64", class, &[sop1(4, D, 12), sop2(0x0e, 41, 17, ic(1))], &[sop1(op, D, src)], &post64(), &[Cmp::X, Cmp::X, Cmp::X], move |x| {
            let c = scc_in(x);
            let a = if bit { x.d[4] as u64 } else { w64(x, 0) };
            let (r, sc) = f(a, w64(x, 1), c);
            vec![Some(r as u32), Some((r >> 32) as u32), Some(sc.unwrap_or(c))]
        });
    }
    type S1N = fn(u64) -> (u32, bool);
    let nops: &[(&'static str, u32, S1N)] = &[
        ("s_bcnt0_i32_b64", 0x0e, |a| (a.count_zeros(), true)),
        ("s_bcnt1_i32_b64", 0x10, |a| (a.count_ones(), true)),
        ("s_ff0_i32_b64", 0x12, |a| (ff(!a, 64), false)),
        ("s_ff1_i32_b64", 0x14, |a| (ff(a, 64), false)),
        ("s_flbit_i32_b64", 0x16, |a| (if a == 0 { u32::MAX } else { a.leading_zeros() }, false)),
        ("s_flbit_i32_i64", 0x18, |a| (flbit_s(a, 64), false)),
    ];
    for &(m, op, f) in nops {
        s.add(m, "SOP1", "s64", Class::U64, &scc_from(17), &[sop1(op, D, 10)], &post32(), &[Cmp::X, Cmp::X], move |x| {
            let (r, sets) = f(w64(x, 0));
            o2(r, if sets { nz(r as u64) } else { scc_in(x) })
        });
    }
}

fn sopk_ops(s: &mut Suite) {
    let imms: [u16; 8] = [0, 1, 5, 0x7fff, 0x8000, 0xffff, 0xfff0, 0x1234];
    let cmps: [(&'static str, u32, fn(u32, u16) -> bool); 12] = [
        ("s_cmpk_eq_i32", 0x03, |a, k| a as i32 == k as i16 as i32),
        ("s_cmpk_lg_i32", 0x04, |a, k| a as i32 != k as i16 as i32),
        ("s_cmpk_gt_i32", 0x05, |a, k| a as i32 > k as i16 as i32),
        ("s_cmpk_ge_i32", 0x06, |a, k| a as i32 >= k as i16 as i32),
        ("s_cmpk_lt_i32", 0x07, |a, k| (a as i32) < k as i16 as i32),
        ("s_cmpk_le_i32", 0x08, |a, k| a as i32 <= k as i16 as i32),
        ("s_cmpk_eq_u32", 0x09, |a, k| a == k as u32),
        ("s_cmpk_lg_u32", 0x0a, |a, k| a != k as u32),
        ("s_cmpk_gt_u32", 0x0b, |a, k| a > k as u32),
        ("s_cmpk_ge_u32", 0x0c, |a, k| a >= k as u32),
        ("s_cmpk_lt_u32", 0x0d, |a, k| a < k as u32),
        ("s_cmpk_le_u32", 0x0e, |a, k| a <= k as u32),
    ];
    for (i, &k) in imms.iter().enumerate() {
        let tag = format!("imm={k:04x} #{i}");
        for &(m, op, f) in &cmps {
            s.add(m, "SOPK", &tag, Class::U32, &[sop1(3, D, 10)], &[sopk(op, D, k)], &scc_to(OUT), &[Cmp::X], move |x| o(f(x.d[0], k) as u32));
        }
        s.add("s_movk_i32", "SOPK", &tag, Class::U32, &[sop1(3, D, 10)], &[sopk(0x00, D, k)], &[s2v(OUT, D)], &[Cmp::X], move |_| o(k as i16 as i32 as u32));
        s.add("s_cmovk_i32", "SOPK", &tag, Class::U32, &[sop1(3, D, 10), sop2(0x0e, 41, 17, ic(1))], &[sopk(0x02, D, k)], &post32(), &[Cmp::X, Cmp::X], move |x| o2(if scc_in(x) != 0 { k as i16 as i32 as u32 } else { x.d[0] }, scc_in(x)));
        s.add("s_addk_i32", "SOPK", &tag, Class::U32, &[sop1(3, D, 10)], &[sopk(0x0f, D, k)], &post32(), &[Cmp::X, Cmp::X], move |x| {
            let (a, b) = (x.d[0] as i32, k as i16 as i32);
            o2(a.wrapping_add(b) as u32, a.checked_add(b).is_none() as u32)
        });
        s.add("s_mulk_i32", "SOPK", &tag, Class::U32, &[sop1(3, D, 10), sop2(0x0e, 41, 17, ic(1))], &[sopk(0x10, D, k)], &post32(), &[Cmp::X, Cmp::X], move |x| o2((x.d[0] as i32).wrapping_mul(k as i16 as i32) as u32, scc_in(x)));
        for (m, op) in [("s_version", 0x01u32), ("s_waitcnt_vscnt", 0x17), ("s_waitcnt_vmcnt", 0x18), ("s_waitcnt_expcnt", 0x19), ("s_waitcnt_lgkmcnt", 0x1a)] {
            let sd = 125;
            s.add(m, "SOPK", &tag, Class::U32, &[sop1(3, D, 10), sop2(0x0e, 41, 17, ic(1))], &[sopk(op, sd, k)], &post32(), &[Cmp::X, Cmp::X], move |x| o2(x.d[0], scc_in(x)));
        }
    }
}

fn sopc_ops(s: &mut Suite) {
    let c32: [(&'static str, u32, fn(u32, u32) -> bool); 14] = [
        ("s_cmp_eq_i32", 0x00, |a, b| a == b),
        ("s_cmp_lg_i32", 0x01, |a, b| a != b),
        ("s_cmp_gt_i32", 0x02, |a, b| (a as i32) > b as i32),
        ("s_cmp_ge_i32", 0x03, |a, b| a as i32 >= b as i32),
        ("s_cmp_lt_i32", 0x04, |a, b| (a as i32) < b as i32),
        ("s_cmp_le_i32", 0x05, |a, b| a as i32 <= b as i32),
        ("s_cmp_eq_u32", 0x06, |a, b| a == b),
        ("s_cmp_lg_u32", 0x07, |a, b| a != b),
        ("s_cmp_gt_u32", 0x08, |a, b| a > b),
        ("s_cmp_ge_u32", 0x09, |a, b| a >= b),
        ("s_cmp_lt_u32", 0x0a, |a, b| a < b),
        ("s_cmp_le_u32", 0x0b, |a, b| a <= b),
        ("s_bitcmp0_b32", 0x0c, |a, b| (a >> (b & 31)) & 1 == 0),
        ("s_bitcmp1_b32", 0x0d, |a, b| (a >> (b & 31)) & 1 == 1),
    ];
    for &(m, op, f) in &c32 {
        let class = if op >= 0x0c { Class::Small } else { Class::U32 };
        s.add(m, "SOPC", "s,s", class, &[], &[sopc(op, 10, 11)], &scc_to(OUT), &[Cmp::X], move |x| o(f(x.d[0], x.d[1]) as u32));
        s.add(m, "SOPC", "s,s swapped", class, &[], &[sopc(op, 11, 10)], &scc_to(OUT), &[Cmp::X], move |x| o(f(x.d[1], x.d[0]) as u32));
        s.add(m, "SOPC", "s,lit", Class::U32, &[], &[sopc(op, 10, LIT), 0x8000_0010], &scc_to(OUT), &[Cmp::X], move |x| o(f(x.d[0], 0x8000_0010) as u32));
    }
    for (m, op, bit) in [("s_bitcmp0_b64", 0x0eu32, 0u64), ("s_bitcmp1_b64", 0x0f, 1)] {
        s.add(m, "SOPC", "s64,s", Class::U64, &[], &[sopc(op, 10, 12)], &scc_to(OUT), &[Cmp::X], move |x| o(((w64(x, 0) >> (x.d[2] & 63)) & 1 == bit) as u32));
        s.add(m, "SOPC", "s64,small", Class::U64, &[sop2(0x0e, 44, 14, ic(63))], &[sopc(op, 10, 44)], &scc_to(OUT), &[Cmp::X], move |x| o(((w64(x, 0) >> (x.d[4] & 63)) & 1 == bit) as u32));
    }
    for (m, op, eq) in [("s_cmp_eq_u64", 0x12u32, true), ("s_cmp_lg_u64", 0x13, false)] {
        s.add(m, "SOPC", "s64,s64", Class::U64, &[], &[sopc(op, 10, 12)], &scc_to(OUT), &[Cmp::X], move |x| o(((w64(x, 0) == w64(x, 1)) == eq) as u32));
        s.add(m, "SOPC", "s64,s64 same", Class::U64, &[], &[sopc(op, 10, 10)], &scc_to(OUT), &[Cmp::X], move |_| o(eq as u32));
    }
}

pub fn add(s: &mut Suite) {
    sop2_ops(s);
    sop1_ops(s);
    sopk_ops(s);
    sopc_ops(s);
    for m in ["s_and_saveexec_b64", "s_or_saveexec_b64", "s_xor_saveexec_b64", "s_andn2_saveexec_b64", "s_orn2_saveexec_b64", "s_nand_saveexec_b64", "s_andn1_saveexec_b64", "s_orn1_saveexec_b64", "s_and_saveexec_b32", "s_or_saveexec_b32", "s_xor_saveexec_b32", "s_andn2_saveexec_b32", "s_orn2_saveexec_b32"] {
        s.skip(m, "SOP1", "wave-level EXEC/SCC semantics; covered by tests/exec.rs scalar_saveexec_lane_model");
    }
    for m in ["s_getpc_b64", "s_setpc_b64", "s_swappc_b64"] {
        s.skip(m, "SOP1", "program-counter control flow, not ALU");
    }
    for m in ["s_getreg_b32", "s_setreg_b32", "s_setreg_imm32_b32"] {
        s.skip(m, "SOPK", "hardware MODE/status registers; see src/hlsl/mode.rs tests");
    }
}
