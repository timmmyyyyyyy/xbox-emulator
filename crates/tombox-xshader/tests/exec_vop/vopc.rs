use super::super::*;

const FC: [&str; 16] = ["f", "lt", "eq", "le", "gt", "lg", "ge", "o", "u", "nge", "nlg", "ngt", "nle", "neq", "nlt", "tru"];
const IC: [&str; 8] = ["f", "lt", "eq", "le", "gt", "ne", "ge", "t"];
const SENT: u32 = 0x5a5a_5a5a;

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    F16,
    F32,
    F64,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
}

static NAMES: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    let mut n = Vec::new();
    for t in [Ty::F16, Ty::F32, Ty::F64, Ty::I16, Ty::U16, Ty::I32, Ty::U32, Ty::I64, Ty::U64] {
        for i in 0..16 {
            let cc = if t.int() { IC[i % 8] } else { FC[i] };
            n.extend(["", "x"].map(|x| format!("v_cmp{x}_{cc}_{}", t.name())));
        }
    }
    n
});

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::F16 => "f16",
            Ty::F32 => "f32",
            Ty::F64 => "f64",
            Ty::I16 => "i16",
            Ty::U16 => "u16",
            Ty::I32 => "i32",
            Ty::U32 => "u32",
            Ty::I64 => "i64",
            Ty::U64 => "u64",
        }
    }
    fn int(self) -> bool {
        !matches!(self, Ty::F16 | Ty::F32 | Ty::F64)
    }
    fn conds(self) -> std::ops::Range<usize> {
        match self {
            Ty::I16 | Ty::U16 => 1..7,
            _ if self.int() => 0..8,
            _ => 0..16,
        }
    }
    fn op(self, i: usize, x: bool) -> u32 {
        let i = i as u32;
        let x = if x { 0x10 } else { 0 };
        match self {
            Ty::F32 => x | i,
            Ty::F64 => 0x20 | x | i,
            Ty::I32 => 0x80 | x | i,
            Ty::U32 => 0xc0 | x | i,
            Ty::I16 => (0x88 + i) | x,
            Ty::U16 => (0xa8 + i) | x,
            Ty::I64 => 0xa0 | x | i,
            Ty::U64 => 0xe0 | x | i,
            Ty::F16 => (if i < 8 { 0xc8 + i } else { 0xe8 + i - 8 }) | x,
        }
    }
    fn mnem(self, i: usize, x: bool) -> &'static str {
        &NAMES[(self as usize * 16 + i) * 2 + x as usize]
    }
    fn val(self, b: u64) -> f64 {
        match self {
            Ty::F16 => h2f(b as u32 & 0xffff) as f64,
            Ty::F32 => f32::from_bits(b as u32) as f64,
            _ => f64::from_bits(b),
        }
    }
    fn ival(self, b: u64) -> i128 {
        match self {
            Ty::I16 => b as u16 as i16 as i128,
            Ty::U16 => b as u16 as i128,
            Ty::I32 => b as u32 as i32 as i128,
            Ty::U32 => b as u32 as i128,
            Ty::I64 => b as i64 as i128,
            _ => b as i128,
        }
    }
    fn sign(self) -> u64 {
        match self {
            Ty::F16 => 0x8000,
            Ty::F32 => 0x8000_0000,
            _ => 1 << 63,
        }
    }
    fn wide(self) -> bool {
        matches!(self, Ty::F64 | Ty::I64 | Ty::U64)
    }
    fn half(self) -> bool {
        matches!(self, Ty::F16 | Ty::I16 | Ty::U16)
    }
    fn iconst(self, k: i32) -> u64 {
        if self.wide() { k as i64 as u64 } else if self.half() { k as u16 as u64 } else { k as u32 as u64 }
    }
    fn literal(self, l: u32) -> u64 {
        match self {
            Ty::I64 => l as i32 as i64 as u64,
            _ if self.half() => (l & 0xffff) as u64,
            _ => l as u64,
        }
    }
}

fn want(t: Ty, i: usize, a: u64, b: u64) -> bool {
    if t.int() {
        let (x, y) = (t.ival(a), t.ival(b));
        return match i {
            0 => false,
            1 => x < y,
            2 => x == y,
            3 => x <= y,
            4 => x > y,
            5 => x != y,
            6 => x >= y,
            _ => true,
        };
    }
    let (x, y) = (t.val(a), t.val(b));
    match i {
        0 => false,
        1 => x < y,
        2 => x == y,
        3 => x <= y,
        4 => x > y,
        5 => x < y || x > y,
        6 => x >= y,
        7 => !x.is_nan() && !y.is_nan(),
        8 => x.is_nan() || y.is_nan(),
        9 => !(x >= y),
        10 => !(x < y || x > y),
        11 => !(x > y),
        12 => !(x <= y),
        13 => !(x == y),
        14 => !(x < y),
        _ => true,
    }
}

fn inputs(t: Ty, a: u64, b: u64) -> Prog {
    let mut p = pr();
    if t.wide() {
        p.vv(2, a as u32).vv(3, (a >> 32) as u32).vv(4, b as u32).vv(5, (b >> 32) as u32);
    } else if t.half() {
        p.vv(1, 0xdead_0000 | a as u32).vv(2, 0xbeef_0000 | b as u32);
    } else {
        p.vv(1, a as u32).vv(2, b as u32);
    }
    p
}

fn srcs(t: Ty) -> (u32, u32) {
    if t.wide() { (v(2), 4) } else { (v(1), 2) }
}

fn post_cmpx() -> Prog {
    let mut p = pr();
    p.raw(&[sop1(3, 30, 126), sop1(3, 126, ic(1)), vop1(1, 0, v(10))]).s2v(1, 30).s2v(2, 106);
    p
}

fn add_src0(c: &mut Cases, t: Ty, i: usize, pre: &Prog, src0: u32, extra: &[u32], r: bool) {
    let r = r as u32;
    let s1 = srcs(t).1;
    let mut p = pr();
    p.raw(&pre.w).sv(106, SENT);
    let mut post = pr();
    post.vcc_to_v(0);
    let mut op = vec![vopc(t.op(i, false), src0, s1)];
    op.extend_from_slice(extra);
    c.add(t.mnem(i, false), &p, &op, &post, &[r]);
    p.vv(10, 0x1234);
    let mut op = vec![vopc(t.op(i, true), src0, s1)];
    op.extend_from_slice(extra);
    op.push(vop1(1, 10, ic(7)));
    c.add(t.mnem(i, true), &p, &op, &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
}

fn add_e32(c: &mut Cases, t: Ty, i: usize, a: u64, b: u64) {
    let (s0, s1) = srcs(t);
    let r = want(t, i, a, b) as u32;
    let mut pre = inputs(t, a, b);
    pre.sv(106, SENT);
    let mut post = pr();
    post.vcc_to_v(0);
    c.add(t.mnem(i, false), &pre, &[vopc(t.op(i, false), s0, s1)], &post, &[r]);
    let mut pre = inputs(t, a, b);
    pre.sv(106, SENT).vv(10, 0x1234);
    c.add(t.mnem(i, true), &pre, &[vopc(t.op(i, true), s0, s1), vop1(1, 10, ic(7))], &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
}

fn add_e64(c: &mut Cases, t: Ty, i: usize, a: u64, b: u64, abs: u32, neg: u32) {
    let (s0, _) = srcs(t);
    let s1 = if t.wide() { v(4) } else { v(2) };
    let md = |x: u64, k: u32| {
        let x = if (abs >> k) & 1 != 0 { x & !t.sign() } else { x };
        if (neg >> k) & 1 != 0 { x ^ t.sign() } else { x }
    };
    let r = want(t, i, md(a, 0), md(b, 1)) as u32;
    let mut pre = inputs(t, a, b);
    pre.sv(20, 0xdead_beef).sv(106, SENT);
    let mut post = pr();
    post.s2v(0, 20).s2v(1, 106);
    c.add(t.mnem(i, false), &pre, &vop3m(t.op(i, false), 20, [s0, s1, 0], abs, neg, 0, false), &post, &[r, SENT]);
    let mut pre = inputs(t, a, b);
    pre.sv(106, SENT).vv(10, 0x1234);
    let mut op = vop3m(t.op(i, true), 126, [s0, s1, 0], abs, neg, 0, false).to_vec();
    op.push(vop1(1, 10, ic(7)));
    c.add(t.mnem(i, true), &pre, &op, &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
}

fn add_opsel(c: &mut Cases, t: Ty, i: usize, a: u64, b: u64, opsel: u32) {
    let r = want(t, i, a, b) as u32;
    let place = |x: u64, k: u32| if (opsel >> k) & 1 != 0 { ((x as u32) << 16) | 0x7e5a } else { 0xc3a5_0000 | x as u32 };
    let mut pre = pr();
    pre.vv(1, place(a, 0)).vv(2, place(b, 1)).sv(20, 0xdead_beef).sv(106, SENT);
    let mut post = pr();
    post.s2v(0, 20).s2v(1, 106);
    c.add(t.mnem(i, false), &pre, &vop3m(t.op(i, false), 20, [v(1), v(2), 0], 0, 0, opsel, false), &post, &[r, SENT]);
    pre.vv(10, 0x1234);
    let mut op = vop3m(t.op(i, true), 126, [v(1), v(2), 0], 0, 0, opsel, false).to_vec();
    op.push(vop1(1, 10, ic(7)));
    c.add(t.mnem(i, true), &pre, &op, &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
}

fn add_dst(c: &mut Cases, t: Ty, i: usize, a: u64, b: u64) {
    let (s0, _) = srcs(t);
    let s1 = if t.wide() { v(4) } else { v(2) };
    let r = want(t, i, a, b) as u32;
    let mut pre = inputs(t, a, b);
    pre.sv(106, SENT);
    let mut post = pr();
    post.vcc_to_v(0);
    c.add(t.mnem(i, false), &pre, &vop3m(t.op(i, false), 106, [s0, s1, 0], 0, 0, 0, false), &post, &[r]);
    let mut pre = inputs(t, a, b);
    pre.sv(20, 0xdead_beef).sv(106, SENT);
    let mut post = pr();
    post.s2v(0, 20).s2v(1, 106);
    c.add(t.mnem(i, false), &pre, &vop3m(t.op(i, false), 125, [s0, s1, 0], 0, 0, 0, false), &post, &[0xdead_beef, SENT]);
}

fn add_masked(c: &mut Cases, t: Ty, i: usize, a: u64, b: u64) {
    let (s0, s1) = srcs(t);
    let mut pre = inputs(t, a, b);
    pre.sv(106, SENT).vv(10, 0x1234).raw(&[sop1(3, 126, ic(0))]);
    let mut post = pr();
    post.raw(&[sop1(3, 126, ic(1))]).vcc_to_v(0);
    c.add(t.mnem(i, false), &pre, &[vopc(t.op(i, false), s0, s1)], &post, &[0]);
    c.add(t.mnem(i, true), &pre, &[vopc(t.op(i, true), s0, s1), vop1(1, 10, ic(7))], &post_cmpx(), &[0x1234, 0, SENT]);
}

fn add_inline(c: &mut Cases, t: Ty, i: usize, b: u64) {
    let one = match t {
        Ty::F16 => 0x3c00,
        Ty::F32 => fb(1.0) as u64,
        _ => 1f64.to_bits(),
    };
    let s1 = srcs(t).1;
    let r = want(t, i, one, b) as u32;
    let pre = inputs(t, 0, b);
    let mut post = pr();
    post.vcc_to_v(0);
    c.add(t.mnem(i, false), &pre, &[vopc(t.op(i, false), 242, s1)], &post, &[r]);
}

fn add_iconsts(c: &mut Cases, t: Ty, i: usize, pairs: &[(u64, u64)]) {
    for (n, k) in [-16, -1, 0, 7, 64].into_iter().enumerate() {
        let b = pairs[n * 2 % pairs.len()].1;
        add_src0(c, t, i, &inputs(t, 0, b), ic(k), &[], want(t, i, t.iconst(k), b));
    }
    for (n, l) in [0x8000_0005u32, 0x7fff_fff0, 0x1234_8001, 0xffff_ffff].into_iter().enumerate() {
        let b = pairs[(n * 3 + 1) % pairs.len()].1;
        add_src0(c, t, i, &inputs(t, 0, b), LIT, &[l], want(t, i, t.literal(l), b));
        add_src0(c, t, i, &inputs(t, 0, t.literal(l)), LIT, &[l], want(t, i, t.literal(l), t.literal(l)));
    }
    if t.wide() {
        for &(a, b) in pairs.iter().step_by(4) {
            let mut pre = inputs(t, 0, b);
            pre.sv(20, a as u32).sv(21, (a >> 32) as u32);
            add_src0(c, t, i, &pre, 20, &[], want(t, i, a, b));
        }
    }
}

fn fpairs(t: Ty) -> Vec<(u64, u64)> {
    let (one, two, m1, z, nz, inf, ninf, qn, sn, nn, max, m25) = match t {
        Ty::F16 => (0x3c00, 0x4000, 0xbc00, 0, 0x8000, 0x7c00, 0xfc00, 0x7e00, 0x7c01, 0xfe00, 0x7bff, 0xc100),
        Ty::F32 => (0x3f80_0000, 0x4000_0000, 0xbf80_0000, 0, 0x8000_0000, 0x7f80_0000, 0xff80_0000, 0x7fc0_0000, 0x7f80_0001, 0xffc0_0000, 0x7f7f_ffff, 0xc020_0000),
        _ => (
            1f64.to_bits(),
            2f64.to_bits(),
            (-1f64).to_bits(),
            0,
            1 << 63,
            f64::INFINITY.to_bits(),
            f64::NEG_INFINITY.to_bits(),
            0x7ff8_0000_0000_0000,
            0x7ff0_0000_0000_0001,
            0xfff8_0000_0000_0000,
            f64::MAX.to_bits(),
            (-2.5f64).to_bits(),
        ),
    };
    let mut v = vec![
        (one, two),
        (two, one),
        (one, one),
        (z, nz),
        (nz, z),
        (m1, m25),
        (inf, inf),
        (ninf, inf),
        (inf, max),
        (qn, one),
        (one, sn),
        (nn, nn),
        (ninf, nn),
    ];
    match t {
        Ty::F16 => v.extend([(1, 0), (1, 2), (0x8001, 0)]),
        Ty::F64 => v.extend([((1e-310f64).to_bits(), 0), (0x3ff0_0000_0000_0001, 0x3ff0_0000_0000_0000), (0x7ff0_0000_0000_0001, 0x7ff0_0000_0000_0001)]),
        _ => v.push((0x3f80_0001, 0x3f80_0000)),
    }
    v
}

fn ipairs(t: Ty) -> Vec<(u64, u64)> {
    let v: Vec<(u64, u64)> = match t {
        Ty::I16 | Ty::U16 => vec![(0x8000, 0x7fff), (0x7fff, 0x8000), (0xffff, 0), (0, 0xffff), (5, 5), (0xfff9, 0xfff9), (0x8000, 0x8000), (1, 2), (2, 1), (0, 0), (0xffff, 0xfffe)],
        Ty::I64 | Ty::U64 => vec![
            (i64::MIN as u64, i64::MAX as u64),
            (i64::MAX as u64, i64::MIN as u64),
            (u64::MAX, 0),
            (0, u64::MAX),
            (5, 5),
            ((-7i64) as u64, (-7i64) as u64),
            (0x1_0000_0000, 0xffff_ffff),
            (0x1_8000_0000, 0x1_7fff_ffff),
            (0xffff_ffff_0000_0001, 0xffff_ffff_0000_0002),
            (0x5_0000_0003, 0x3_0000_0005),
            (0x8000_0000, 0xffff_ffff_8000_0000),
            (1, 2),
            (2, 1),
            (0, 0),
        ],
        _ => [(i32::MIN as u32, i32::MAX as u32), (i32::MAX as u32, i32::MIN as u32), (u32::MAX, 0), (0, u32::MAX), (5, 5), ((-7i32) as u32, (-7i32) as u32), (0x8000_0000, 0x7fff_ffff), (1, 2), (2, 1), (0, 0)]
            .iter()
            .map(|&(a, b)| (a as u64, b as u64))
            .collect(),
    };
    v
}

fn group(t: Ty) -> Cases {
    let mut c = Cases::new();
    let pairs = if t.int() { ipairs(t) } else { fpairs(t) };
    for i in t.conds() {
        for &(a, b) in &pairs {
            add_e32(&mut c, t, i, a, b);
        }
        for &(a, b) in pairs.iter().step_by(3) {
            add_e64(&mut c, t, i, a, b, 0, 0);
        }
        add_dst(&mut c, t, i, pairs[0].0, pairs[0].1);
        add_masked(&mut c, t, i, pairs[2].0, pairs[2].1);
        if t.half() {
            for (k, &(a, b)) in pairs.iter().enumerate().skip(1).step_by(3) {
                add_opsel(&mut c, t, i, a, b, 1 + k as u32 % 3);
            }
        }
        if t.int() {
            if !matches!(t, Ty::I32 | Ty::U32) {
                add_iconsts(&mut c, t, i, &pairs);
            }
            continue;
        }
        let sg = t.sign();
        let (nn, one, z) = (pairs[11].0, pairs[2].0, 0u64);
        add_e64(&mut c, t, i, nn, nn, 3, 0);
        add_e64(&mut c, t, i, z, z, 0, 2);
        add_e64(&mut c, t, i, one | sg, one, 1, 0);
        add_e64(&mut c, t, i, one, one | sg, 0, 2);
        add_e64(&mut c, t, i, one, one, 1, 1);
        add_inline(&mut c, t, i, one);
        add_inline(&mut c, t, i, pairs[0].1);
    }
    c
}

fn class16(b: u32) -> u32 {
    let (e, m, neg) = ((b >> 10) & 0x1f, b & 0x3ff, b & 0x8000 != 0);
    match (e, m) {
        (0x1f, 0) => if neg { 2 } else { 9 },
        (0x1f, _) => (m >> 9) & 1,
        (0, 0) => if neg { 5 } else { 6 },
        (0, _) => if neg { 4 } else { 7 },
        _ => if neg { 3 } else { 8 },
    }
}

fn class_group() -> Cases {
    let mut c = Cases::new();
    let mn = |x: bool| if x { "v_cmpx_class_f16" } else { "v_cmp_class_f16" };
    let op = |x: bool| if x { 0x9f } else { 0x8f };
    let vals: [u32; 14] = [0x7c01, 0x7e00, 0xfc00, 0xc000, 0x8001, 0x8000, 0, 0x03ff, 0x3c00, 0x7c00, 0xfdff, 0xfe00, 0x7bff, 0x0400];
    let pass = |b: u32, mask: u32| (mask >> class16(b & 0xffff)) & 1;
    for (k, &b) in vals.iter().enumerate() {
        let bit = 1 << class16(b);
        for mask in [bit, 0x3ff & !bit, if k % 2 == 0 { 0x155 } else { 0x2aa }, bit | 0xa5a5_0000] {
            let r = pass(b, mask);
            let mut pre = pr();
            pre.vv(1, 0xdead_0000 | b).vv(2, mask).sv(106, SENT);
            let mut post = pr();
            post.vcc_to_v(0);
            c.add(mn(false), &pre, &[vopc(op(false), v(1), 2)], &post, &[r]);
            pre.vv(10, 0x1234);
            c.add(mn(true), &pre, &[vopc(op(true), v(1), 2), vop1(1, 10, ic(7))], &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
        }
        for (abs, neg, opsel) in [(1u32, 0u32, 0u32), (0, 1, 0), (1, 1, 0), (0, 0, 1), (0, 1, 1)] {
            let x = if abs != 0 { b & 0x7fff } else { b };
            let x = if neg != 0 { x ^ 0x8000 } else { x };
            let mask = 1 << class16(x) | if k % 2 == 0 { 0x0f0 } else { 0x00f };
            let mask = if k % 3 == 0 { mask & !(1 << class16(x)) } else { mask };
            let r = pass(x, mask);
            let src = if opsel != 0 { (b << 16) | 0x3c00 } else { 0x7c00_0000 | b };
            let mut pre = pr();
            pre.vv(1, src).vv(2, mask).sv(20, 0xdead_beef).sv(106, SENT);
            let mut post = pr();
            post.s2v(0, 20).s2v(1, 106);
            c.add(mn(false), &pre, &vop3m(op(false), 20, [v(1), v(2), 0], abs, neg, opsel, false), &post, &[r, SENT]);
            pre.vv(10, 0x1234);
            let mut w = vop3m(op(true), 126, [v(1), v(2), 0], abs, neg, opsel, false).to_vec();
            w.push(vop1(1, 10, ic(7)));
            c.add(mn(true), &pre, &w, &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
        }
    }
    for (src, bits) in [(242u32, 0x3c00u32), (241, 0xb800), (248, 0x3118), (ic(0), 0), (ic(-1), 0xffff), (ic(5), 5), (ic(-16), 0xfff0)] {
        for mask in [1 << class16(bits), 0x3ff & !(1 << class16(bits))] {
            let r = pass(bits, mask);
            let mut pre = pr();
            pre.vv(2, mask).sv(106, SENT);
            let mut post = pr();
            post.vcc_to_v(0);
            c.add(mn(false), &pre, &[vopc(op(false), src, 2)], &post, &[r]);
        }
    }
    let mut pre = pr();
    pre.vv(1, 0x3c00).vv(2, 0x3ff).sv(106, SENT).vv(10, 0x1234).raw(&[sop1(3, 126, ic(0))]);
    let mut post = pr();
    post.raw(&[sop1(3, 126, ic(1))]).vcc_to_v(0);
    c.add(mn(false), &pre, &[vopc(op(false), v(1), 2)], &post, &[0]);
    c.add(mn(true), &pre, &[vopc(op(true), v(1), 2), vop1(1, 10, ic(7))], &post_cmpx(), &[0x1234, 0, SENT]);
    c
}

#[test]
fn vopc_f32() {
    group(Ty::F32).run_both("vopc_f32");
}

#[test]
fn vopc_f16() {
    group(Ty::F16).run_both("vopc_f16");
}

#[test]
fn vopc_f64() {
    group(Ty::F64).run_both("vopc_f64");
}

#[test]
fn vopc_i32() {
    group(Ty::I32).run_both("vopc_i32");
}

#[test]
fn vopc_u32() {
    group(Ty::U32).run_both("vopc_u32");
}

#[test]
fn vopc_i16() {
    group(Ty::I16).run_both("vopc_i16");
}

#[test]
fn vopc_u16() {
    group(Ty::U16).run_both("vopc_u16");
}

#[test]
fn vopc_i64() {
    group(Ty::I64).run_both("vopc_i64");
}

#[test]
fn vopc_u64() {
    group(Ty::U64).run_both("vopc_u64");
}

#[test]
fn vopc_class_f16() {
    class_group().run_both("vopc_class_f16");
}

fn class32(b: u32) -> u32 {
    let (e, m, neg) = ((b >> 23) & 0xff, b & 0x7f_ffff, b >> 31 != 0);
    match (e, m) {
        (0xff, 0) => if neg { 2 } else { 9 },
        (0xff, _) => (m >> 22) & 1,
        (0, 0) => if neg { 5 } else { 6 },
        (0, _) => if neg { 4 } else { 7 },
        _ => if neg { 3 } else { 8 },
    }
}

fn class32_group() -> Cases {
    let mut c = Cases::new();
    let mn = |x: bool| if x { "v_cmpx_class_f32" } else { "v_cmp_class_f32" };
    let op = |x: bool| if x { 0x98 } else { 0x88 };
    let vals: [u32; 16] = [
        0x7f80_0001, 0x7fc0_0000, 0xff80_0000, 0xc000_0000, 0x8000_0001, 0x8000_0000, 0, 0x007f_ffff, 0x3f80_0000, 0x7f80_0000, 0xffbf_ffff, 0xffc0_0000, 0x7f7f_ffff, 0x0080_0000, 0x7fff_ffff,
        0x807f_ffff,
    ];
    let pass = |b: u32, mask: u32| (mask >> class32(b)) & 1;
    for (k, &b) in vals.iter().enumerate() {
        let bit = 1 << class32(b);
        for mask in [bit, 0x3ff & !bit, if k % 2 == 0 { 0x155 } else { 0x2aa }, bit | 0xa5a5_0000, 0, 0x3ff] {
            let r = pass(b, mask);
            let mut pre = pr();
            pre.vv(1, b).vv(2, mask).sv(106, SENT);
            let mut post = pr();
            post.vcc_to_v(0);
            c.add(mn(false), &pre, &[vopc(op(false), v(1), 2)], &post, &[r]);
            pre.vv(10, 0x1234);
            c.add(mn(true), &pre, &[vopc(op(true), v(1), 2), vop1(1, 10, ic(7))], &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
        }
        for (abs, neg) in [(1u32, 0u32), (0, 1), (1, 1)] {
            let x = if abs != 0 { b & 0x7fff_ffff } else { b };
            let x = if neg != 0 { x ^ 0x8000_0000 } else { x };
            let mask = 1 << class32(x) | if k % 2 == 0 { 0x0f0 } else { 0x00f };
            let mask = if k % 3 == 0 { mask & !(1 << class32(x)) } else { mask };
            let r = pass(x, mask);
            let mut pre = pr();
            pre.vv(1, b).vv(2, mask).sv(20, 0xdead_beef).sv(106, SENT);
            let mut post = pr();
            post.s2v(0, 20).s2v(1, 106);
            c.add(mn(false), &pre, &vop3m(op(false), 20, [v(1), v(2), 0], abs, neg, 0, false), &post, &[r, SENT]);
            pre.vv(10, 0x1234);
            let mut w = vop3m(op(true), 126, [v(1), v(2), 0], abs, neg, 0, false).to_vec();
            w.push(vop1(1, 10, ic(7)));
            c.add(mn(true), &pre, &w, &post_cmpx(), &[if r != 0 { 7 } else { 0x1234 }, r, SENT]);
        }
    }
    for (src, bits) in [(240u32, 0x3f00_0000u32), (241, 0xbf00_0000), (242, 0x3f80_0000), (243, 0xbf80_0000), (246, 0x4080_0000), (248, 0x3e22_f983), (ic(0), 0), (ic(-1), 0xffff_ffff), (ic(5), 5), (ic(-16), 0xffff_fff0)] {
        for mask in [1 << class32(bits), 0x3ff & !(1 << class32(bits))] {
            let r = pass(bits, mask);
            let mut pre = pr();
            pre.vv(2, mask).sv(106, SENT);
            let mut post = pr();
            post.vcc_to_v(0);
            c.add(mn(false), &pre, &[vopc(op(false), src, 2)], &post, &[r]);
        }
    }
    for (bits, mask) in [(0x7f80_0001u32, 0x3ffu32), (0x3f80_0000, 0x100), (0xc2f6_e979, 0x008)] {
        let r = pass(bits, mask);
        let mut pre = pr();
        pre.vv(2, mask).sv(106, SENT);
        let mut post = pr();
        post.vcc_to_v(0);
        c.add(mn(false), &pre, &[vopc(op(false), LIT, 2), bits], &post, &[r]);
    }
    let mut pre = pr();
    pre.vv(1, 0x3f80_0000).vv(2, 0x3ff).sv(106, SENT).vv(10, 0x1234).raw(&[sop1(3, 126, ic(0))]);
    let mut post = pr();
    post.raw(&[sop1(3, 126, ic(1))]).vcc_to_v(0);
    c.add(mn(false), &pre, &[vopc(op(false), v(1), 2)], &post, &[0]);
    c.add(mn(true), &pre, &[vopc(op(true), v(1), 2), vop1(1, 10, ic(7))], &post_cmpx(), &[0x1234, 0, SENT]);
    c
}

#[test]
fn vopc_class_f32() {
    class32_group().run_both("vopc_class_f32");
}
