use super::*;

fn xs(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

pub fn opaque(pre: &[u32]) -> Vec<u32> {
    opaque_with(pre, vop1(0x07, 200, v(2)))
}

pub fn opaque_with(pre: &[u32], zero: u32) -> Vec<u32> {
    opaque_keep(pre, zero, &[])
}

pub fn opaque_keep(pre: &[u32], zero: u32, keep: &[u32]) -> Vec<u32> {
    let konst = |src: u32| matches!(src, 128..=208 | 240..=248 | LIT);
    let mut out = vec![zero];
    let mut i = 0;
    while i < pre.len() {
        let w = pre[i];
        let n = if w & 0x1ff == LIT { 2 } else { 1 };
        if w >> 25 == 0x3f && (w >> 9) & 0xff == 1 && konst(w & 0x1ff) && !keep.contains(&((w >> 17) & 0xff)) && i + n <= pre.len() {
            out.push(vop2(0x1c, (w >> 17) & 0xff, w & 0x1ff, 200));
            out.extend_from_slice(&pre[i + 1..i + n]);
            i += n;
            continue;
        }
        let n = if w & 0xff80_ff00 == 0xbe80_0300 && w & 0xff == LIT { 2 } else { n };
        if w & 0xff80_ff00 == 0xbe80_0300 && konst(w & 0xff) && ((w >> 16) & 0x7f < 106 || (w >> 16) & 0x7f == 124) && i + n <= pre.len() {
            out.push(vop2(0x1c, 201, w & 0xff, 200));
            out.extend_from_slice(&pre[i + 1..i + n]);
            out.push(vop1(0x02, (w >> 16) & 0x7f, v(201)));
            i += n;
            continue;
        }
        out.push(w);
        i += 1;
    }
    out
}

pub fn run_pos(words: &[u32]) -> Result<[u32; 4], String> {
    let mut ex = shader(words);
    ex.input_sig = vec![sig("SV_Position", 1, 3, 0, 0xf)];
    let t = translate(&ex)?;
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { draw(&mut c, &code) }.map_err(|e| format!("{e}\n{}", t.hlsl))
}

impl Cases {
    pub fn run_opaque(&self, group: &str) {
        let mut fails = Vec::new();
        let mut bad: std::collections::BTreeSet<&str> = Default::default();
        let mut all: std::collections::BTreeSet<&str> = Default::default();
        for (i, c) in self.v.iter().enumerate() {
            all.insert(c.m);
            let isa: Vec<u8> = c.op.iter().flat_map(|w| w.to_le_bytes()).collect();
            let got_m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
            if got_m != c.m {
                fails.push(format!("#{i} {}: encoding decodes as {got_m}", c.m));
                bad.insert(c.m);
                continue;
            }
            let mut p = Prog::new();
            p.raw(&opaque(&c.pre)).raw(&c.op).raw(&c.post);
            match run_pos(&p.words()) {
                Ok(r) if r[..c.exp.len()] == c.exp[..] => {}
                Ok(r) => {
                    let h = if std::env::var_os("TOMBOX_EXEC_HLSL").is_some() { translate(&shader(&p.words())).map(|t| t.hlsl).unwrap_or_default() } else { String::new() };
                    fails.push(format!("#{i} {}: got {:08x?} want {:08x?}{h}", c.m, &r[..c.exp.len()], c.exp));
                    bad.insert(c.m);
                }
                Err(e) => {
                    let e: String = e.lines().take(3).collect::<Vec<_>>().join(" | ");
                    fails.push(format!("#{i} {}: {e}", c.m));
                    bad.insert(c.m);
                }
            }
        }
        let ok: Vec<&str> = all.iter().filter(|m| !bad.contains(*m)).copied().collect();
        if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
            let f = std::path::Path::new(&dir).join(format!("{group}.txt"));
            let prev: Option<std::collections::BTreeSet<String>> = std::fs::read_to_string(&f).ok().map(|t| t.lines().map(str::to_string).collect());
            let keep: Vec<&str> = ok.iter().copied().filter(|m| prev.as_ref().map_or(true, |p| p.contains(*m))).collect();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(f, keep.join("\n") + "\n");
        }
        println!("{group} (runtime inputs): {} mnemonics verified, {} failing, {} cases", ok.len(), bad.len(), self.v.len());
        assert!(fails.is_empty(), "{group}: {} failing cases\n{}", fails.len(), fails.join("\n"));
    }

    pub fn run_both(&self, group: &str) {
        self.run(group);
        self.run_opaque(group);
    }
}

pub fn fm(a: f32, b: f32, c: f32) -> u32 {
    let r = a.mul_add(b, c);
    if r.is_nan() { 0x7fc0_0000 } else { r.to_bits() }
}

fn not_nan(b: u32) -> u32 {
    if b & 0x7fff_ffff > 0x7f80_0000 { b & 0xff80_0000 } else { b }
}

#[test]
fn fma_f32_full_range() {
    let mut c = Cases::new();
    let mut seed = 0x51f1_5eed_0bad_cafeu64;
    for i in 0..160 {
        let r = xs(&mut seed);
        let r2 = xs(&mut seed);
        let a = f32::from_bits(not_nan(r as u32));
        let b = f32::from_bits(not_nan((r >> 32) as u32));
        let p = a * b;
        let cc = match i % 4 {
            0 => f32::from_bits(not_nan(r2 as u32)),
            1 => f32::from_bits(not_nan((p.to_bits() ^ 0x8000_0000) ^ (r2 as u32 & 0xff))),
            2 => f32::from_bits(not_nan(((p.to_bits() & 0xff80_0000).wrapping_sub((r2 as u32 % 40) << 23)) | ((r2 >> 32) as u32 & 0x807f_ffff))),
            _ => f32::from_bits((r2 >> 32) as u32 & 0x80ff_ffff),
        };
        v3c(&mut c, "v_fma_f32", 0x14b, fb(a), fb(b), fb(cc), fm(a, b, cc));
    }
    let big = f32::MAX;
    let mid = 2f64.powi(128) - 2f64.powi(103);
    let edges: Vec<(f32, f32, f32)> = vec![
        (big, 2.0, -1e-30),
        (-big, 2.0, 1e-30),
        (big, 1.0, (mid - big as f64) as f32),
        (big, 1.0, ((mid - big as f64) * (1.0 - 2f64.powi(-20))) as f32),
        (1.0 + 2f32.powi(-23), big, 2f32.powi(103) - 2f32.powi(80)),
        (1.0 + 2f32.powi(-23), big, -(2f32.powi(80))),
        (2f32.powi(-75), 2f32.powi(-75), 0.0),
        (2f32.powi(-75), 1.5 * 2f32.powi(-75), 0.0),
        (2f32.powi(-75), 1.5 * 2f32.powi(-75), 2f32.powi(-140)),
        (2f32.powi(-75), 1.5 * 2f32.powi(-75), -(2f32.powi(-140))),
        (f32::from_bits(1), 0.5, 0.0),
        (f32::from_bits(3), 0.5, 0.0),
        (f32::from_bits(3), 0.5, -(2f32.powi(-200).max(f32::from_bits(1)))),
        (1.0 + 2f32.powi(-12), 1.0 - 2f32.powi(-12), -1.0),
        (1e30, 1e-30, -1.0),
        (3.0, 1.0 / 3.0, -1.0),
        (f32::INFINITY, 2.0, f32::NEG_INFINITY),
        (0.0, f32::INFINITY, 1.0),
        (-0.0, 1.0, 0.0),
        (-0.0, 1.0, -0.0),
        (1.0, -1.0, 1.0),
    ];
    for &(x, y, z) in &edges {
        v3c(&mut c, "v_fma_f32", 0x14b, fb(x), fb(y), fb(z), fm(x, y, z));
    }
    for k in 0..40 {
        let r = xs(&mut seed);
        let a = f32::from_bits(not_nan(r as u32));
        let b = f32::from_bits(not_nan((r >> 32) as u32));
        let d = f32::from_bits(not_nan(xs(&mut seed) as u32));
        vc(&mut c, "v_fmac_f32", &[(1, fb(a)), (2, fb(b)), (10, fb(d))], &[vop2(0x2b, 10, v(1), 2)], &[10], &[fm(a, b, d)]);
        if k % 2 == 0 {
            vc(&mut c, "v_fmamk_f32", &[(1, fb(a)), (2, fb(d))], &[vop2(0x2c, 10, v(1), 2), fb(b)], &[10], &[fm(a, b, d)]);
        } else {
            vc(&mut c, "v_fmaak_f32", &[(1, fb(a)), (2, fb(b))], &[vop2(0x2d, 10, v(1), 2), fb(d)], &[10], &[fm(a, b, d)]);
        }
    }
    for &(x, y, z) in &edges {
        vc(&mut c, "v_fmamk_f32", &[(1, fb(x)), (2, fb(z))], &[vop2(0x2c, 10, v(1), 2), fb(y)], &[10], &[fm(x, y, z)]);
        vc(&mut c, "v_fmac_f32", &[(1, fb(x)), (2, fb(y)), (10, fb(z))], &[vop2(0x2b, 10, v(1), 2)], &[10], &[fm(x, y, z)]);
    }
    c.run_both("fma_full");
}

fn mullit(a: f32, b: f32, cc: f32) -> f32 {
    if b == -f32::MAX || b == f32::NEG_INFINITY || b.is_nan() || cc <= 0.0 || cc.is_nan() { -f32::MAX } else { a * b }
}

#[test]
fn mullit_f32() {
    let mut c = Cases::new();
    let rows: [(f32, f32, f32); 12] = [
        (2.0, 3.0, 1.0),
        (2.0, -f32::MAX, 1.0),
        (2.0, f32::NEG_INFINITY, 1.0),
        (2.0, f32::NAN, 1.0),
        (2.0, 3.0, -0.0),
        (2.0, 3.0, 0.0),
        (2.0, 3.0, -5.0),
        (2.0, 3.0, f32::NAN),
        (2.0, -3.0, 1e-40),
        (f32::INFINITY, 0.5, 1.0),
        (f32::MAX, 4.0, 1.0),
        (-1.5, f32::MAX, 2.0),
    ];
    for &(a, b, cc) in &rows {
        v3c(&mut c, "v_mullit_f32", 0x150, fb(a), fb(b), fb(cc), fb(mullit(a, b, cc)));
    }
    let m = |neg: u32, abs: u32, a: f32, b: f32, cc: f32, want: f32, c: &mut Cases| {
        vc(c, "v_mullit_f32", &[(1, fb(a)), (2, fb(b)), (3, fb(cc))], &vop3m(0x150, 10, [v(1), v(2), v(3)], abs, neg, 0, false), &[10], &[fb(want)]);
    };
    m(2, 0, 2.0, f32::MAX, 1.0, -f32::MAX, &mut c);
    m(2, 0, 2.0, f32::INFINITY, 1.0, -f32::MAX, &mut c);
    m(4, 0, 2.0, 3.0, -1.0, 6.0, &mut c);
    m(0, 4, 2.0, 3.0, -1.0, 6.0, &mut c);
    m(0, 2, 2.0, -3.0, 1.0, 6.0, &mut c);
    m(1, 0, 2.0, 3.0, 1.0, -6.0, &mut c);
    c.run_both("mullit");
}

const TWO_BY_PI: [u32; 37] = [
    0xa2f9836e, 0x4e441529, 0xfc2757d1, 0xf534ddc0, 0xdb629599, 0x3c439041, 0xfe5163ab, 0xdebbc561, 0xb7246e3a, 0x424dd2e0, 0x06492eea, 0x09d1921c, 0xfe1deb1c, 0xb129a73e, 0xe88235f5, 0x2ebb4484,
    0xe99c7026, 0xb45f7e41, 0x3991d639, 0x835339f4, 0x9c845f8b, 0xbdf9283b, 0x1ff897ff, 0xde05980f, 0xef2f118b, 0x5a0a6d1f, 0x6d367ecf, 0x27cb09b7, 0x4f463f66, 0x9e5fea2d, 0x7527bac7, 0xebe5f17b,
    0x3d0739f7, 0x8a5292ea, 0x6bfb5fb1, 0x1f8d5d08, 0x56033046,
];

fn ldexp_rne(x: f64, e: i32) -> f64 {
    let mut x = x;
    let mut e = e.clamp(-2200, 2200);
    for _ in 0..3 {
        let k = e.clamp(-1022, 1023);
        x *= f64::from_bits(((k + 1023) as u64) << 52);
        e -= k;
    }
    x
}

fn trig_preop(x: f64, seg: u32) -> f64 {
    let e = ((x.to_bits() >> 52) & 0x7ff) as u32;
    let mut shift = (seg & 31) * 53;
    if e > 1077 {
        shift += e - 1077;
    }
    let idx = (shift >> 5) as usize;
    if idx + 2 >= TWO_BY_PI.len() {
        return 0.0;
    }
    let bs = shift & 31;
    let mut thi = ((TWO_BY_PI[idx] as u64) << 32) | TWO_BY_PI[idx + 1] as u64;
    let tlo = (TWO_BY_PI[idx + 2] as u64) << 32;
    if bs != 0 {
        thi = (thi << bs) | (tlo >> (64 - bs));
    }
    thi >>= 11;
    let mut scale = -53 - shift as i32;
    if e >= 1968 {
        scale += 128;
    }
    ldexp_rne(thi as f64, scale)
}

fn f64_bits(e: u64, m: u64, neg: bool) -> f64 {
    f64::from_bits(((neg as u64) << 63) | (e << 52) | (m & ((1u64 << 52) - 1)))
}

#[test]
fn trig_preop_f64() {
    let mut c = Cases::new();
    let case = |c: &mut Cases, x: f64, seg: u32| {
        let [lo, hi] = dw(x);
        let want = dw(trig_preop(x, seg));
        vc(c, "v_trig_preop_f64", &[(2, lo), (3, hi), (4, seg)], &vop3(0x174, 10, [v(2), v(4), 0]), &[10, 11], &want);
    };
    for seg in 0..32 {
        case(&mut c, 1.0, seg);
    }
    for &e in &[0u64, 1, 1000, 1077, 1078, 1100, 1500, 1967, 1968, 1969, 2000, 2046, 2047] {
        for &seg in &[0u32, 1, 2, 3, 5, 7] {
            case(&mut c, f64_bits(e, 0x1234_5678_9abc, e % 2 == 1), seg);
        }
    }
    for &(e, seg) in &[(1077u64, 21u32), (1078, 21), (1081, 21), (1109, 20), (1110, 20), (1111, 20), (1150, 19), (1125, 20), (1077 + 1119 - 1007, 19), (1077 + 1120 - 1007, 19), (1077 + 1172 - 1007, 19), (1077 + 1200 - 1007, 19), (1077 + 1201 - 1007, 19)] {
        case(&mut c, f64_bits(e, 0, false), seg);
    }
    case(&mut c, f64::INFINITY, 0);
    case(&mut c, f64::NAN, 1);
    case(&mut c, -0.0, 4);
    for &seg in &[32u32, 33, 63, 0xffff_ffe1] {
        case(&mut c, 1.0, seg);
    }
    let mut seed = 0x7419_0000_1111_2222u64;
    for _ in 0..40 {
        let r = xs(&mut seed);
        case(&mut c, f64::from_bits(r), (r >> 7) as u32 & 31);
    }
    c.run_both("trig_preop");
}

fn expf(x: f64) -> i32 {
    ((x.to_bits() >> 52) & 0x7ff) as i32
}

fn sub(x: f64) -> bool {
    x != 0.0 && x.abs() < f64::MIN_POSITIVE
}

fn div_scale(x: f64, d: f64, n: f64) -> (f64, u32) {
    if d == 0.0 || n == 0.0 {
        return (f64::from_bits(0x7ff8_0000_0000_0000), 1);
    }
    let (en, ed) = (expf(n), expf(d));
    if en - ed >= 768 {
        return (if x == d { ldexp_rne(x, 128) } else { x }, 1);
    }
    if sub(d) {
        return (ldexp_rne(x, 128), 0);
    }
    let (tr, tq) = (sub(1.0 / d), sub(n / d));
    if tr && tq {
        return (if x == d { ldexp_rne(x, -128) } else { x }, 1);
    }
    if tr {
        return (ldexp_rne(x, -128), 0);
    }
    if tq {
        return (if x == n { ldexp_rne(x, 128) } else { x }, 1);
    }
    if en <= 53 {
        return (ldexp_rne(x, 128), 0);
    }
    (x, 0)
}

fn d64v(c: &mut Prog, r: u32, x: f64) {
    let [lo, hi] = dw(x);
    c.vv(r, lo).vv(r + 1, hi);
}

fn v3d(op: u32, vdst: u32, srcs: [u32; 3], neg: u32) -> [u32; 2] {
    vop3m(op, vdst, srcs, 0, neg, 0, false)
}

fn v3b(op: u32, vdst: u32, sdst: u32, srcs: [u32; 3], neg: u32) -> [u32; 2] {
    let w = vop3b(op, vdst, sdst, srcs);
    [w[0], w[1] | (neg << 29)]
}

fn fdiv_pre(xv: f64, yv: f64) -> Prog {
    let mut p = pr();
    d64v(&mut p, 2, xv);
    d64v(&mut p, 4, yv);
    d64v(&mut p, 18, 1.0);
    p.raw(&v3b(0x16e, 6, 125, [v(4), v(4), v(2)], 0));
    p.raw(&[vop1(0x2f, 8, v(6))]);
    p.raw(&v3d(0x14c, 10, [v(6), v(8), v(18)], 1));
    p.raw(&v3d(0x14c, 12, [v(8), v(10), v(8)], 0));
    p.raw(&v3d(0x14c, 10, [v(6), v(12), v(18)], 1));
    p.raw(&v3b(0x16e, 14, VCC, [v(2), v(4), v(2)], 0));
    p.raw(&v3d(0x14c, 12, [v(12), v(10), v(12)], 0));
    p.raw(&v3d(0x165, 16, [v(14), v(12), 0], 0));
    p.raw(&v3d(0x14c, 10, [v(6), v(16), v(14)], 1));
    p
}

fn fdiv_case(c: &mut Cases, xv: f64, yv: f64) {
    let pre = fdiv_pre(xv, yv);
    let mut op = Vec::new();
    op.extend_from_slice(&v3d(0x170, 10, [v(10), v(12), v(16)], 0));
    op.extend_from_slice(&v3d(0x160, 10, [v(10), v(4), v(2)], 0));
    let mut post = pr();
    post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]);
    let q = xv / yv;
    let want = if q.is_nan() { [0, 0xfff8_0000] } else { dw(q) };
    c.add("v_div_fmas_f64", &pre, &op, &post, &want);
}

#[test]
fn div_f64_sequence() {
    let mut c = Cases::new();
    let tiny = f64::from_bits(1);
    let pairs: Vec<(f64, f64)> = vec![
        (1.0, 3.0),
        (2.0, 7.0),
        (-5.5, 0.1),
        (1e300, 1e-300),
        (f64::MAX, f64::MIN_POSITIVE),
        (2f64.powi(1000), 2f64.powi(-100)),
        (1e-300, 1e10),
        (1e-300, 3e20),
        (3.0 * 2f64.powi(-1060), 2.0),
        (2f64.powi(-1000), 2f64.powi(70)),
        (1.0, 2f64.powi(1023)),
        (1.0, f64::MAX),
        (3.0, 1.5 * 2f64.powi(1022)),
        (f64::MAX, 2f64.powi(1022) * 1.75),
        (1.0, tiny),
        (5e-324, 3e-320),
        (2.5e-310, 7.0),
        (2.5e-310, 1e-312),
        (1e-320, 0.3),
        (tiny, 2.0),
        (3.0 * tiny, 2.0),
        (2f64.powi(-1022), 3.0),
        (2f64.powi(-1020), 1.0 + 2f64.powi(-52)),
        (0.0, 1.0),
        (1.0, 0.0),
        (0.0, 0.0),
        (f64::INFINITY, 2.0),
        (2.0, f64::INFINITY),
        (f64::INFINITY, f64::NEG_INFINITY),
    ];
    for &(xv, yv) in &pairs {
        fdiv_case(&mut c, xv, yv);
        fdiv_case(&mut c, -xv, yv);
    }
    let mut seed = 0xd1d1_0000_abcd_0001u64;
    for k in 0..60 {
        let r = xs(&mut seed);
        let r2 = xs(&mut seed);
        let (ea, eb) = match k % 3 {
            0 => (900 + r2 % 300, 900 + (r2 >> 20) % 300),
            1 => (1 + r2 % 120, 1800 + (r2 >> 20) % 240),
            _ => (1900 + r2 % 140, 1 + (r2 >> 20) % 200),
        };
        fdiv_case(&mut c, f64_bits(ea, r, r2 & 1 != 0), f64_bits(eb, r >> 11, r2 & 2 != 0));
    }
    c.run_both("div_f64");
}

fn fmas_case(c: &mut Cases, a: f64, b: f64, cc: f64, vcc: u32, want: u64) {
    let mut pre = pr();
    d64v(&mut pre, 2, a);
    d64v(&mut pre, 4, b);
    d64v(&mut pre, 6, cc);
    pre.sv(106, vcc);
    let mut post = pr();
    post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]);
    c.add("v_div_fmas_f64", &pre, &v3d(0x170, 10, [v(2), v(4), v(6)], 0), &post, &[want as u32, (want >> 32) as u32]);
}

fn fmas_ref(a: f64, b: f64, cc: f64) -> Option<u64> {
    let up = 2f64.powi(128);
    let dn = 2f64.powi(-128);
    let r = if expf(cc) > 1023 {
        let s = a * up;
        if !s.is_finite() || s / up != a {
            return None;
        }
        s.mul_add(b, cc * up)
    } else {
        let sb = b * dn;
        let sc = cc * dn;
        if sb * up != b || sc * up != cc {
            return None;
        }
        a.mul_add(sb, sc)
    };
    Some(if r.is_nan() { 0x7ff8_0000_0000_0000 } else { r.to_bits() })
}

#[test]
fn div_fmas_f64_direct() {
    let mut c = Cases::new();
    let tiny = f64::from_bits(1);
    for k in [1u64, 2, 5] {
        let b = (2 * k + 1) as f64 * 2f64.powi(-947);
        fmas_case(&mut c, 1.0, b, tiny, 1, k + 1);
        fmas_case(&mut c, 1.0, b, -tiny, 1, k);
        fmas_case(&mut c, -1.0, b, -tiny, 1, (k + 1) | 1 << 63);
        fmas_case(&mut c, -1.0, b, tiny, 1, k | 1 << 63);
        fmas_case(&mut c, 1.0, b, 0.0, 1, if k & 1 == 1 { k + 1 } else { k });
    }
    fmas_case(&mut c, 3.0, 5.0, 1.0, 1, 2f64.powi(-124).to_bits());
    fmas_case(&mut c, 3.0, 5.0, 4.0, 1, (19.0 * 2f64.powi(128)).to_bits());
    fmas_case(&mut c, 3.0, 5.0, 4.0, 0, 19f64.to_bits());
    fmas_case(&mut c, -1.0, 2.0 + 2f64.powi(-51), 2.0, 1, (-2f64.powi(77)).to_bits());
    fmas_case(&mut c, f64::MAX, 1.0, 2.0, 1, f64::INFINITY.to_bits());
    fmas_case(&mut c, 0.0, f64::INFINITY, 1.0, 1, 0x7ff8_0000_0000_0000);
    fmas_case(&mut c, 0.0, 0.0, -0.0, 1, 0);
    fmas_case(&mut c, -0.0, 1.0, -0.0, 1, 1 << 63);
    let mut seed = 0xf3a5_0000_1234_9876u64;
    let mut n = 0;
    while n < 90 {
        let r = xs(&mut seed);
        let r2 = xs(&mut seed);
        let r3 = xs(&mut seed);
        let (ea, eb, ec) = match n % 3 {
            0 => (1000 + r2 % 60, 1000 + (r2 >> 16) % 60, 1000 + (r2 >> 32) % 60),
            1 => (1023 + r2 % 4, 1023 + (r2 >> 16) % 4, 1024 + (r2 >> 32) % 3),
            _ => (600 + r2 % 800, 600 + (r2 >> 16) % 800, 600 + (r2 >> 32) % 800),
        };
        let a = f64_bits(ea, r, r3 & 1 != 0);
        let b = f64_bits(eb, r >> 7, r3 & 2 != 0);
        let cc = f64_bits(ec, r3 >> 3, r3 & 4 != 0);
        let vcc = (r3 >> 5) as u32 & 1;
        let want = if vcc == 0 { Some(fm64(a, b, cc)) } else { fmas_ref(a, b, cc) };
        if let Some(w) = want {
            fmas_case(&mut c, a, b, cc, vcc, w);
            n += 1;
        }
    }
    c.run_both("div_fmas_direct");
}

fn fm64(a: f64, b: f64, cc: f64) -> u64 {
    let r = a.mul_add(b, cc);
    if r.is_nan() { 0x7ff8_0000_0000_0000 } else { r.to_bits() }
}

#[test]
fn div_scale_and_fixup_f64() {
    let mut c = Cases::new();
    let rows: Vec<(f64, f64)> = vec![
        (6.0, 2.0),
        (1.0, 3.0),
        (1e300, 1e-300),
        (1.0, 1e-310),
        (1.0, 1.5 * 2f64.powi(1022)),
        (1e-300, 1e10),
        (2f64.powi(-1000), 1.0),
        (0.0, 1.0),
        (1.0, 0.0),
        (f64::NAN, 1.0),
        (f64::INFINITY, 1.0),
    ];
    for &(n, d) in &rows {
        for x in [d, n] {
            let (r, vcc) = div_scale(x, d, n);
            let mut pre = pr();
            d64v(&mut pre, 2, x);
            d64v(&mut pre, 4, d);
            d64v(&mut pre, 6, n);
            pre.sv(106, 0x55);
            let mut post = pr();
            post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]).s2v(2, 106);
            c.add("v_div_scale_f64", &pre, &vop3b(0x16e, 10, VCC, [v(2), v(4), v(6)]), &post, &[dw(r)[0], dw(r)[1], vcc]);
        }
        let (r, _) = div_scale(-n, d, -n);
        let mut pre = pr();
        d64v(&mut pre, 2, n);
        d64v(&mut pre, 4, d);
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10)), vop1(1, 1, v(11))]);
        let w = v3b(0x16e, 10, 125, [v(2), v(4), v(2)], 5);
        if !r.is_nan() {
            c.add("v_div_scale_f64", &pre, &w, &post, &dw(r));
        }
    }
    let fix: Vec<(u64, f64, f64, u64)> = vec![
        (2.5f64.to_bits(), 2.0, 5.0, 2.5f64.to_bits()),
        (2.5f64.to_bits(), -2.0, 5.0, (-2.5f64).to_bits()),
        (7.0f64.to_bits(), 0.0, 1.0, f64::INFINITY.to_bits()),
        (7.0f64.to_bits(), 0.0, -1.0, f64::NEG_INFINITY.to_bits()),
        (7.0f64.to_bits(), f64::INFINITY, 1.0, 0),
        (7.0f64.to_bits(), 1.0, f64::INFINITY, f64::INFINITY.to_bits()),
        (7.0f64.to_bits(), 0.0, 0.0, 0xfff8_0000_0000_0000),
        (7.0f64.to_bits(), f64::INFINITY, f64::NEG_INFINITY, 0xfff8_0000_0000_0000),
        (7.0f64.to_bits(), 1.0, f64::from_bits(0x7ff0_0000_0000_0001), 0x7ff8_0000_0000_0001),
        (7.0f64.to_bits(), f64::from_bits(0xfff0_0000_0000_0002), 1.0, 0xfff8_0000_0000_0002),
        (7.0f64.to_bits(), 1e300, 1e-300, 0),
        (7.0f64.to_bits(), -1e300, 1e-300, 0x8000_0000_0000_0000),
        (0x7ff8_0000_0000_0000, 3.0, 1.0, f64::INFINITY.to_bits()),
        (0x7ff8_0000_0000_0000, -3.0, 1.0, f64::NEG_INFINITY.to_bits()),
        ((-0.25f64).to_bits(), -2.0, -0.5, 0.25f64.to_bits()),
    ];
    for &(q, d, n, want) in &fix {
        vc(&mut c, "v_div_fixup_f64", &[(2, q as u32), (3, (q >> 32) as u32), (4, dw(d)[0]), (5, dw(d)[1]), (6, dw(n)[0]), (7, dw(n)[1])], &vop3(0x160, 10, [v(2), v(4), v(6)]), &[10, 11], &[want as u32, (want >> 32) as u32]);
    }
    c.run_both("div_scale_fixup");
}

fn expf32(x: f32) -> i32 {
    ((x.to_bits() >> 23) & 0xff) as i32
}

fn sub32(x: f32) -> bool {
    x != 0.0 && x.abs() < f32::from_bits(0x0080_0000)
}

fn ldexp32(x: f32, e: i32) -> f32 {
    (x as f64 * 2f64.powi(e)) as f32
}

fn f32_bits(e: u32, m: u32, neg: bool) -> f32 {
    f32::from_bits(((neg as u32) << 31) | (e << 23) | (m & 0x7f_ffff))
}

fn fma32_exact(a: f32, b: f32, c: f32) -> f32 {
    let p = (a as f64) * (b as f64);
    let s = p + (c as f64);
    if s.is_nan() {
        return f32::from_bits(0x7fc0_0000);
    }
    if s.is_infinite() {
        return s as f32;
    }
    let bb = s - p;
    let e = (p - (s - bb)) + ((c as f64) - bb);
    let bits = s.to_bits();
    let lo = bits as u32;
    let hi = (bits >> 32) as u32;
    let adj = ((e != 0.0) && (lo & 1 == 0)) as u32;
    let dn = adj & (((e > 0.0) != (s > 0.0)) as u32);
    let new_lo = lo.wrapping_add(adj).wrapping_sub(2 * dn);
    let new_hi = hi.wrapping_sub(dn & ((lo == 0) as u32));
    f64::from_bits(((new_hi as u64) << 32) | (new_lo as u64)) as f32
}

fn div_scale32_ref(x: f32, d: f32, n: f32) -> (f32, u32) {
    if d == 0.0 || n == 0.0 {
        return (f32::from_bits(0x7fc0_0000), 1);
    }
    let (en, ed) = (expf32(n), expf32(d));
    if en - ed >= 96 {
        return (if x == d { ldexp32(x, 64) } else { x }, 1);
    }
    if sub32(d) {
        return (ldexp32(x, 64), 0);
    }
    let tr = (1.0f64 / (d as f64)).abs() < 2f64.powi(-126);
    let tq = ((n as f64) / (d as f64)).abs() < 2f64.powi(-126);
    if tr && tq {
        return (if x == d { ldexp32(x, -64) } else { x }, 1);
    }
    if tr {
        return (ldexp32(x, -64), 0);
    }
    if tq {
        return (if x == n { ldexp32(x, 64) } else { x }, 1);
    }
    if en <= 23 {
        return (ldexp32(x, 64), 0);
    }
    (x, 0)
}

fn div_fmas32_ref(a: f32, b: f32, c: f32, vcc: u32) -> Option<u32> {
    let r = fma32_exact(a, b, c);
    if r.is_nan() {
        return Some(0x7fc0_0000);
    }
    if vcc & 1 == 0 {
        return Some(r.to_bits());
    }
    if expf32(c) > 127 {
        if sub32(r) {
            return None;
        }
        let s = (r as f64) * 2f64.powi(64);
        return Some(if s.is_infinite() { f32::INFINITY.to_bits() } else { (s as f32).to_bits() });
    }
    let q = (r as f64) * 2f64.powi(-64);
    if q != 0.0 && q.abs() < 2f64.powi(-126) {
        return None;
    }
    Some((q as f32).to_bits())
}

fn fmas32_case(c: &mut Cases, a: f32, b: f32, cc: f32, vcc: u32, want: u32) {
    let mut pre = pr();
    pre.vv(2, fb(a)).vv(4, fb(b)).vv(6, fb(cc));
    pre.sv(106, vcc);
    let mut post = pr();
    post.raw(&[vop1(1, 0, v(10))]);
    c.add("v_div_fmas_f32", &pre, &vop3(0x16f, 10, [v(2), v(4), v(6)]), &post, &[want]);
}

#[test]
fn div_fmas_f32_direct() {
    let mut c = Cases::new();
    fmas32_case(&mut c, 3.0, 5.0, 4.0, 0, fb(19.0));
    fmas32_case(&mut c, 3.0, 5.0, 4.0, 1, fb(19.0 * 2f32.powi(64)));
    fmas32_case(&mut c, 3.0, 5.0, 1.0, 1, fb(16.0 * 2f32.powi(-64)));
    fmas32_case(&mut c, -1.0, 2.0 + 2f32.powi(-22), 2.0, 1, fb(-2f32.powi(42)));
    fmas32_case(&mut c, f32::MAX, 1.0, 2.0, 1, f32::INFINITY.to_bits());
    fmas32_case(&mut c, 0.0, f32::INFINITY, 1.0, 1, 0x7fc0_0000);
    fmas32_case(&mut c, 0.0, 0.0, -0.0, 1, 0);
    fmas32_case(&mut c, -0.0, 1.0, -0.0, 1, 1 << 31);
    let mut seed = 0xf3a5_0000_1234_9876u64;
    let mut n = 0;
    while n < 60 {
        let r = xs(&mut seed);
        let r2 = xs(&mut seed);
        let r3 = xs(&mut seed);
        let (ea, eb, ec) = match n % 3 {
            0 => (120 + r2 % 40, 120 + (r2 >> 16) % 40, 120 + (r2 >> 32) % 40),
            1 => (127 + r2 % 2, 127 + (r2 >> 16) % 2, 128 + (r2 >> 32) % 2),
            _ => (60 + r2 % 100, 60 + (r2 >> 16) % 100, 60 + (r2 >> 32) % 100),
        };
        let a = f32_bits(ea as u32, r as u32, r3 & 1 != 0);
        let b = f32_bits(eb as u32, (r >> 7) as u32, r3 & 2 != 0);
        let cc = f32_bits(ec as u32, (r3 >> 3) as u32, r3 & 4 != 0);
        let vcc = (r3 >> 5) as u32 & 1;
        if let Some(w) = div_fmas32_ref(a, b, cc, vcc) {
            fmas32_case(&mut c, a, b, cc, vcc, w);
            n += 1;
        }
    }
    c.run_both("div_fmas_f32_direct");
}

#[test]
fn div_scale_and_fixup_f32() {
    let mut c = Cases::new();
    let rows: Vec<(f32, f32)> = vec![
        (6.0, 2.0),
        (1.0, 3.0),
        (1e30, 1e-30),
        (1.0, 1e-30),
        (1.0, 1.5 * 2f32.powi(126)),
        (1e-30, 1e10),
        (2f32.powi(-100), 1.0),
        (0.0, 1.0),
        (1.0, 0.0),
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
    ];
    for &(n, d) in &rows {
        for x in [d, n] {
            let (r, vcc) = div_scale32_ref(x, d, n);
            let mut pre = pr();
            pre.vv(2, fb(x)).vv(4, fb(d)).vv(6, fb(n));
            pre.sv(106, 0x55);
            let mut post = pr();
            post.raw(&[vop1(1, 0, v(10))]).s2v(1, 106);
            c.add("v_div_scale_f32", &pre, &vop3b(0x16d, 10, VCC, [v(2), v(4), v(6)]), &post, &[fb(r), vcc]);
        }
        let (r, _) = div_scale32_ref(-n, d, -n);
        let mut pre = pr();
        pre.vv(2, fb(n)).vv(4, fb(d));
        let mut post = pr();
        post.raw(&[vop1(1, 0, v(10))]);
        let w = v3b(0x16d, 10, 124, [v(2), v(4), v(2)], 5);
        if !r.is_nan() {
            c.add("v_div_scale_f32", &pre, &w, &post, &[fb(r)]);
        }
    }
    let fix: Vec<(u32, f32, f32, u32)> = vec![
        (fb(2.5), 2.0, 5.0, fb(2.5)),
        (fb(2.5), -2.0, 5.0, fb(-2.5)),
        (fb(7.0), 0.0, 1.0, f32::INFINITY.to_bits()),
        (fb(7.0), 0.0, -1.0, f32::NEG_INFINITY.to_bits()),
        (fb(7.0), f32::INFINITY, 1.0, 0),
        (fb(7.0), 1.0, f32::INFINITY, f32::INFINITY.to_bits()),
        (fb(7.0), 0.0, 0.0, 0x7fc0_0000),
        (fb(7.0), f32::INFINITY, f32::NEG_INFINITY, 0x7fc0_0000),
        (fb(7.0), 1.0, f32::from_bits(0x7f80_0001), 0x7fc0_0001),
        (fb(7.0), f32::from_bits(0xff80_0002), 1.0, 0xffc0_0002),
        (fb(7.0), 1e30, 1e-30, 0),
        (fb(7.0), -1e30, 1e-30, 0x8000_0000),
        (0x7fc0_0000, 3.0, 1.0, f32::INFINITY.to_bits()),
        (0x7fc0_0000, -3.0, 1.0, f32::NEG_INFINITY.to_bits()),
        (fb(-0.25), -2.0, -0.5, fb(0.25)),
    ];
    for &(q, d, n, want) in &fix {
        vc(&mut c, "v_div_fixup_f32", &[(2, q), (4, fb(d)), (6, fb(n))], &vop3(0x15f, 10, [v(2), v(4), v(6)]), &[10], &[want]);
    }
    c.run_both("div_scale_fixup_f32");
}

fn fdiv32_pre(xv: f32, yv: f32) -> Prog {
    let mut p = pr();
    p.vv(2, fb(xv));
    p.vv(4, fb(yv));
    p.vv(18, fb(1.0));
    p.raw(&v3b(0x16d, 6, 124, [v(4), v(4), v(2)], 0));
    p.raw(&[vop1(0x2a, 8, v(6))]);
    p.raw(&v3d(0x14b, 10, [v(6), v(8), v(18)], 1));
    p.raw(&v3d(0x14b, 12, [v(8), v(10), v(8)], 0));
    p.raw(&v3d(0x14b, 10, [v(6), v(12), v(18)], 1));
    p.raw(&v3b(0x16d, 14, VCC, [v(2), v(4), v(2)], 0));
    p.raw(&v3d(0x14b, 12, [v(12), v(10), v(12)], 0));
    p.raw(&[vop2(0x08, 16, v(14), 12)]);
    p.raw(&v3d(0x14b, 10, [v(6), v(16), v(14)], 1));
    p
}

fn fdiv32_case(c: &mut Cases, xv: f32, yv: f32) {
    let pre = fdiv32_pre(xv, yv);
    let mut op = Vec::new();
    op.extend_from_slice(&v3d(0x16f, 10, [v(10), v(12), v(16)], 0));
    op.extend_from_slice(&v3d(0x15f, 10, [v(10), v(4), v(2)], 0));
    let mut post = pr();
    post.raw(&[vop1(1, 0, v(10))]);
    let q = (xv as f64) / (yv as f64);
    let want = if q.is_nan() { 0x7fc0_0000 } else { (q as f32).to_bits() };
    c.add("v_div_fmas_f32", &pre, &op, &post, &[want]);
}

#[test]
fn div_f32_sequence() {
    let mut c = Cases::new();
    let pairs: Vec<(f32, f32)> = vec![
        (1.0, 3.0),
        (2.0, 7.0),
        (-5.5, 0.1),
        (1e20, 1e-10),
        (1e-10, 1e10),
        (3.0, 1.5),
        (123.456, 0.789),
        (1000.0, 3.0),
        (0.001, 1000.0),
        (1.0, 1.0),
        (0.0, 1.0),
        (1.0, 0.0),
        (0.0, 0.0),
        (f32::INFINITY, 2.0),
        (2.0, f32::INFINITY),
        (f32::INFINITY, f32::NEG_INFINITY),
    ];
    for &(xv, yv) in &pairs {
        fdiv32_case(&mut c, xv, yv);
        fdiv32_case(&mut c, -xv, yv);
    }
    c.run_both("div_f32");
}

mod amd_fold;
mod basic;
mod fract;
mod interp;
mod lanes;
mod leonida;
mod mbcnt;
mod rdna2_silicon;
mod scratch;
mod vopc;
