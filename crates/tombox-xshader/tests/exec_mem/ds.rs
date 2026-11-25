use super::*;

pub fn ds(op: u32, offset: u32, addr: u32, d0: u32, d1: u32, vdst: u32) -> [u32; 2] {
    [0xd800_0000 | (op << 18) | (offset & 0xffff), addr | (d0 << 8) | (d1 << 16) | (vdst << 24)]
}

fn lane_value(l: u32) -> u32 {
    (l * l * 0x1111 + 7) ^ 0xdead_0000
}

fn lane_prog(p: &mut Prog) {
    p.raw(&[vop1(0x07, 10, v(2)), vop1(0x07, 11, v(3))]);
    p.raw(&[vop2(0x1a, 12, ic(1), 11), vop2(0x25, 12, v(12), 10)]);
    p.raw(&[vop2(0x0b, 13, v(12), 12), vop2(0x0b, 13, LIT, 13), 0x1111]);
    p.raw(&[vop2(0x25, 13, ic(7), 13), vop2(0x1d, 13, LIT, 13), 0xdead_0000]);
}

#[test]
fn ds_swizzle_quad() {
    let mut cs = Vec::new();
    for &m in &[0xe4u32, 0x1b, 0x55, 0xaa, 0xff, 0x00, 0x4e, 0xb1, 0x93, 0x39] {
        for k in 0..4u32 {
            let op = ds(0x35, 0x8000 | m, 13, 0, 0, 14);
            let mut p = Prog::new();
            lane_prog(&mut p);
            p.raw(&op).raw(&ds(0x35, 0x8000 | k, 14, 0, 0, 0));
            let want = lane_value((m >> (2 * k)) & 3);
            cs.push(MCase { m: "ds_swizzle_b32", op: op.to_vec(), words: p.words(), env: Env { pos: true, ..Default::default() }, rt: vec![want], uav: vec![], tol: 0.0 });
        }
    }
    run_mem_cases("ds_swizzle", &cs);
}

const T: u32 = 64;

pub fn dv(t: u32) -> u32 {
    (t * 0x2f5b3) ^ 0x8080_8080
}

pub fn ev(t: u32) -> u32 {
    dv(t) ^ 0x0f0f_1234
}

fn store(vdata: u32, vaddr: u32, k: u32) -> [u32; 2] {
    [0xe000_0000 | (0x1c << 18) | (1 << 12) | (k * 256), vaddr | (vdata << 8) | (2 << 16) | (0x80 << 24)]
}

pub fn cs_prog(body: &[u32], outs: &[u32]) -> Vec<u32> {
    let mut p = Prog::new();
    p.raw(&smem(2, 8, 4, 0, SNULL));
    p.raw(&[vop2(0x1a, 1, ic(2), 0)]);
    p.raw(&[vop2(0x0b, 2, LIT, 0), 0x2f5b3, vop2(0x1d, 2, LIT, 2), 0x8080_8080]);
    p.raw(&[vop2(0x1d, 4, LIT, 2), 0x0f0f_1234]);
    p.raw(&[vop2(0x1d, 3, ic(1), 0), vop2(0x1a, 3, ic(2), 3)]);
    p.raw(&[vop2(0x1a, 5, ic(3), 0), vop2(0x1d, 6, ic(1), 0), vop2(0x1a, 6, ic(3), 6)]);
    p.raw(&[vop2(0x1d, 7, ic(1), 0), vop2(0x1a, 8, ic(1), 0), vop2(0x1a, 9, ic(1), 7)]);
    p.raw(body);
    for (k, &r) in outs.iter().enumerate() {
        p.raw(&store(r, 1, k as u32));
    }
    let mut w = p.w;
    w.push(S_ENDPGM);
    w
}

fn outs_of(f: &dyn Fn(u32) -> Vec<u32>) -> Vec<(usize, u32)> {
    let mut m = Vec::new();
    for t in 0..T {
        for (k, v) in f(t).into_iter().enumerate() {
            m.push((k * 64 + t as usize, v));
        }
    }
    m
}

fn cc(m: &'static str, op: [u32; 2], body: Vec<u32>, outs: &[u32], f: &dyn Fn(u32) -> Vec<u32>) -> CsCase {
    CsCase { m, op: op.to_vec(), words: cs_prog(&body, outs), threads: T, mem: outs_of(f), init: vec![] }
}

#[test]
fn ds_lds_compute() {
    let mut cs = Vec::new();
    let nb = |t: u32| t ^ 1;
    for &off in &[0u32, 0x100, 0x1004] {
        let w = ds(0x0d, off, 1, 2, 0, 0);
        let r = ds(0x36, off, 3, 0, 0, 10);
        let mut b = w.to_vec();
        b.extend_from_slice(&r);
        cs.push(cc("ds_write_b32", w, b.clone(), &[10], &|t| vec![dv(nb(t))]));
        cs.push(cc("ds_read_b32", r, b, &[10], &|t| vec![dv(nb(t))]));
    }
    for &(o0, o1) in &[(0u32, 1u32), (1, 0), (2, 3)] {
        let w = ds(0x0e, o0 | (o1 << 8), 5, 2, 4, 0);
        let r = ds(0x37, o1 | (o0 << 8), 6, 0, 0, 10);
        let mut b = w.to_vec();
        b.extend_from_slice(&r);
        cs.push(cc("ds_write2_b32", w, b.clone(), &[10, 11], &|t| vec![ev(nb(t)), dv(nb(t))]));
        cs.push(cc("ds_read2_b32", r, b, &[10, 11], &|t| vec![ev(nb(t)), dv(nb(t))]));
        let w = ds(0x0f, o0 | (o1 << 8), 1, 2, 4, 0);
        let r = ds(0x38, o1 | (o0 << 8), 3, 0, 0, 10);
        let mut b = w.to_vec();
        b.extend_from_slice(&r);
        if o0 != o1 {
            cs.push(cc("ds_write2st64_b32", w, b.clone(), &[10, 11], &|t| vec![ev(nb(t)), dv(nb(t))]));
            cs.push(cc("ds_read2st64_b32", r, b, &[10, 11], &|t| vec![ev(nb(t)), dv(nb(t))]));
        }
    }
    for &off in &[0u32, 0x41] {
        let w = ds(0x1e, off, 0, 2, 0, 0);
        for (m, op, f) in [("ds_read_u8", 0x3au32, (|x: u32| x & 0xff) as fn(u32) -> u32), ("ds_read_i8", 0x39, |x: u32| (x as u8 as i8) as i32 as u32)] {
            let r = ds(op, off, 7, 0, 0, 10);
            let mut b = w.to_vec();
            b.extend_from_slice(&r);
            cs.push(cc("ds_write_b8", w, b.clone(), &[10], &move |t| vec![f(dv(nb(t)))]));
            cs.push(cc(m, r, b, &[10], &move |t| vec![f(dv(nb(t)))]));
        }
        let w = ds(0x1f, off & !1, 8, 2, 0, 0);
        for (m, op, f) in [("ds_read_u16", 0x3cu32, (|x: u32| x & 0xffff) as fn(u32) -> u32), ("ds_read_i16", 0x3b, |x: u32| (x as u16 as i16) as i32 as u32)] {
            let r = ds(op, off & !1, 9, 0, 0, 10);
            let mut b = w.to_vec();
            b.extend_from_slice(&r);
            cs.push(cc("ds_write_b16", w, b.clone(), &[10], &move |t| vec![f(dv(nb(t)))]));
            cs.push(cc(m, r, b, &[10], &move |t| vec![f(dv(nb(t)))]));
        }
        type Ext = fn(u32) -> u32;
        let w8 = ds(0x1e, off, 0, 2, 0, 0);
        let w16 = ds(0x1f, off & !1, 8, 2, 0, 0);
        let d16: [(&'static str, u32, bool, Ext); 6] = [
            ("ds_read_u8_d16", 0xa2, false, |x| x & 0xff),
            ("ds_read_u8_d16_hi", 0xa3, true, |x| x & 0xff),
            ("ds_read_i8_d16", 0xa4, false, |x| (x as u8 as i8) as i16 as u16 as u32),
            ("ds_read_i8_d16_hi", 0xa5, true, |x| (x as u8 as i8) as i16 as u16 as u32),
            ("ds_read_u16_d16", 0xa6, false, |x| x & 0xffff),
            ("ds_read_u16_d16_hi", 0xa7, true, |x| x & 0xffff),
        ];
        for (m, op, hi, f) in d16 {
            let wide = op >= 0xa6;
            let (w, raddr, roff) = if wide { (w16, 9, off & !1) } else { (w8, 7, off) };
            let r = ds(op, roff, raddr, 0, 0, 10);
            let mut b = w.to_vec();
            b.push(vop1(1, 10, v(4)));
            b.extend_from_slice(&r);
            cs.push(cc(m, r, b, &[10], &move |t| {
                let (old, x) = (ev(t), f(dv(nb(t))));
                vec![if hi { (old & 0xffff) | (x << 16) } else { (old & 0xffff_0000) | x }]
            }));
        }
        for byte in 0..4u32 {
            let at = (off & !3) + byte;
            let w_init = ds(0x0d, off & !3, 1, 4, 0, 0);
            let w_hi = ds(0xa0, at, 1, 2, 0, 0);
            let r_full = ds(0x36, off & !3, 3, 0, 0, 10);
            let mut b = w_init.to_vec();
            b.extend_from_slice(&w_hi);
            b.extend_from_slice(&r_full);
            let sh = byte * 8;
            cs.push(cc("ds_write_b8_d16_hi", w_hi, b, &[10], &move |t| {
                let n = nb(t);
                vec![(ev(n) & !(0xffu32 << sh)) | (((dv(n) >> 16) & 0xff) << sh)]
            }));
        }
        let base_off = off & !3;
        for half_off in [base_off, base_off + 2] {
            let w_init = ds(0x0d, base_off, 1, 4, 0, 0);
            let w_hi = ds(0xa1, half_off, 1, 2, 0, 0);
            let r_full = ds(0x36, base_off, 3, 0, 0, 10);
            let mut b = w_init.to_vec();
            b.extend_from_slice(&w_hi);
            b.extend_from_slice(&r_full);
            let hi_shift = (half_off & 2) * 8;
            cs.push(cc("ds_write_b16_d16_hi", w_hi, b, &[10], &move |t| {
                let n = nb(t);
                let orig = ev(n);
                let hi = dv(n) >> 16;
                vec![(orig & !(0xffffu32 << hi_shift)) | (hi << hi_shift)]
            }));
        }
    }
    type At = fn(u32, u32) -> u32;
    let atoms: [(&'static str, &'static str, u32, At); 6] = [
        ("ds_add_u32", "ds_add_rtn_u32", 0x00, |a, b| a.wrapping_add(b)),
        ("ds_min_u32", "ds_min_rtn_u32", 0x07, |a, b| a.min(b)),
        ("ds_max_u32", "ds_max_rtn_u32", 0x08, |a, b| a.max(b)),
        ("ds_and_b32", "ds_and_rtn_b32", 0x09, |a, b| a & b),
        ("ds_or_b32", "ds_or_rtn_b32", 0x0a, |a, b| a | b),
        ("ds_xor_b32", "ds_xor_rtn_b32", 0x0b, |a, b| a ^ b),
    ];
    let init = 0x5aa5_0ff0u32;
    for (m, mr, op, f) in atoms {
        let mut b = vec![vop1(1, 12, LIT), init, vop1(1, 13, ic(0))];
        b.extend_from_slice(&ds(0x0d, 0x200, 13, 12, 0, 0));
        let a = ds(op, 0x200, 13, 2, 0, 0);
        b.extend_from_slice(&a);
        b.extend_from_slice(&ds(0x36, 0x200, 13, 0, 0, 10));
        let total = (0..T).fold(init, |acc, t| f(acc, dv(t)));
        cs.push(cc(m, a, b, &[10], &move |_| vec![total]));
        let mut b = ds(0x0d, 0, 1, 4, 0, 0).to_vec();
        let a = ds(op + 0x20, 0, 1, 2, 0, 11);
        b.extend_from_slice(&a);
        b.extend_from_slice(&ds(0x36, 0, 3, 0, 0, 10));
        cs.push(cc(mr, a, b, &[11, 10], &move |t| vec![ev(t), f(ev(nb(t)), dv(nb(t)))]));
    }
    run_cs_cases("ds_lds", &cs);
}

fn reg_val(r: u32, t: u32) -> u32 {
    match r {
        0 => t,
        2 => dv(t),
        4 => ev(t),
        7 => t ^ 1,
        13 => 0,
        _ => unreachable!(),
    }
}

type At3 = fn(u32, u32, u32) -> u32;

#[test]
fn ds_lds_atomics_ext() {
    let mut cs = Vec::new();
    let nb = |t: u32| t ^ 1;
    let general: &[(u32, u32, u32)] = &[(4, 2, 0), (4, 4, 0), (0, 7, 0), (13, 2, 0), (13, 13, 0), (4, 0, 0), (0, 4, 0)];
    let cmp: &[(u32, u32, u32)] = &[(4, 4, 2), (4, 2, 0), (0, 0, 4), (13, 13, 7), (0, 7, 2)];
    let ops: [(Option<&'static str>, &'static str, u32, At3); 8] = [
        (Some("ds_sub_u32"), "ds_sub_rtn_u32", 0x01, |o, a, _| o.wrapping_sub(a)),
        (Some("ds_rsub_u32"), "ds_rsub_rtn_u32", 0x02, |o, a, _| a.wrapping_sub(o)),
        (Some("ds_inc_u32"), "ds_inc_rtn_u32", 0x03, |o, a, _| if o >= a { 0 } else { o + 1 }),
        (Some("ds_dec_u32"), "ds_dec_rtn_u32", 0x04, |o, a, _| if o == 0 || o > a { a } else { o - 1 }),
        (Some("ds_min_i32"), "ds_min_rtn_i32", 0x05, |o, a, _| (o as i32).min(a as i32) as u32),
        (Some("ds_max_i32"), "ds_max_rtn_i32", 0x06, |o, a, _| (o as i32).max(a as i32) as u32),
        (Some("ds_cmpst_b32"), "ds_cmpst_rtn_b32", 0x10, |o, a, b| if o == a { b } else { o }),
        (None, "ds_wrxchg_rtn_b32", 0x0d, |_, a, _| a),
    ];
    for (m, mr, op, f) in ops {
        let inputs = if op == 0x10 { cmp } else { general };
        for &(old, d0, d1) in inputs {
            let want = move |t: u32| f(reg_val(old, t), reg_val(d0, t), reg_val(d1, t));
            for rtn in [false, true] {
                let Some(name) = (if rtn { Some(mr) } else { m }) else { continue };
                let a = ds(op + if rtn { 0x20 } else { 0 }, 0, 1, d0, d1, 11);
                let mut b = vec![vop1(1, 13, ic(0))];
                b.extend_from_slice(&ds(0x0d, 0, 1, old, 0, 0));
                b.extend_from_slice(&a);
                b.extend_from_slice(&ds(0x36, 0, 3, 0, 0, 10));
                if rtn {
                    cs.push(cc(name, a, b, &[11, 10], &move |t| vec![reg_val(old, t), want(nb(t))]));
                } else {
                    cs.push(cc(name, a, b, &[10], &move |t| vec![want(nb(t))]));
                }
            }
        }
    }
    let init = 0x5aa5_0ff0u32;
    type At = fn(u32, u32) -> u32;
    let shared: [(&'static str, u32, u32, At); 5] = [
        ("ds_sub_u32", 0x01, 2, |a, b| a.wrapping_sub(b)),
        ("ds_min_i32", 0x05, 2, |a, b| (a as i32).min(b as i32) as u32),
        ("ds_max_i32", 0x06, 2, |a, b| (a as i32).max(b as i32) as u32),
        ("ds_inc_u32", 0x03, 14, |a, _| a + 1),
        ("ds_dec_u32", 0x04, 14, |a, _| a - 1),
    ];
    for (m, op, data, f) in shared {
        let mut b = vec![vop1(1, 12, LIT), init, vop1(1, 13, ic(0)), vop1(1, 14, LIT), 0xffff_ffff];
        b.extend_from_slice(&ds(0x0d, 0x200, 13, 12, 0, 0));
        let a = ds(op, 0x200, 13, data, 0, 0);
        b.extend_from_slice(&a);
        b.extend_from_slice(&ds(0x36, 0x200, 13, 0, 0, 10));
        let total = (0..T).fold(init, |acc, t| f(acc, dv(t)));
        cs.push(cc(m, a, b, &[10], &move |_| vec![total]));
    }
    run_cs_cases("ds_lds_ext", &cs);
}

#[test]
fn ds_lds_mskor_wrxchg2() {
    let mut cs = Vec::new();
    let nb = |t: u32| t ^ 1;
    let grid: &[(u32, u32, u32)] = &[(4, 2, 0), (2, 4, 7), (0, 13, 4), (4, 4, 4), (13, 13, 2), (2, 2, 13), (4, 0, 2), (2, 7, 4)];
    for (k, &(old, d0, d1)) in grid.iter().enumerate() {
        let off = if k % 2 == 0 { 0 } else { 0x104 };
        let want = move |t: u32| (reg_val(old, t) & !reg_val(d0, t)) | reg_val(d1, t);
        for rtn in [false, true] {
            let a = ds(if rtn { 0x2c } else { 0x0c }, off, 1, d0, d1, 11);
            let mut b = vec![vop1(1, 13, ic(0)), vop1(1, 11, v(4))];
            b.extend_from_slice(&ds(0x0d, off, 1, old, 0, 0));
            b.extend_from_slice(&a);
            b.extend_from_slice(&ds(0x36, off, 3, 0, 0, 10));
            if rtn {
                cs.push(cc("ds_mskor_rtn_b32", a, b, &[11, 10], &move |t| vec![reg_val(old, t), want(nb(t))]));
            } else {
                cs.push(cc("ds_mskor_b32", a, b, &[11, 10], &move |t| vec![ev(t), want(nb(t))]));
            }
        }
    }
    let mut b = vec![vop1(1, 12, LIT), 0x5aa5_0ff0, vop1(1, 13, ic(0)), vop1(1, 15, ic(1))];
    b.extend_from_slice(&[vop2(0x1a, 14, v(0), 15), vop2(0x1b, 16, ic(1), 0), vop2(0x1a, 17, v(0), 16)]);
    b.extend_from_slice(&ds(0x0d, 0x200, 13, 12, 0, 0));
    let a = ds(0x0c, 0x200, 13, 14, 17, 0);
    b.extend_from_slice(&a);
    b.extend_from_slice(&ds(0x36, 0x200, 13, 0, 0, 10));
    cs.push(cc("ds_mskor_b32", a, b, &[10], &|_| vec![0xaaaa_aaaa]));
    for &(o0, o1) in &[(0u32, 1u32), (1, 0), (2, 3)] {
        let offs = o0 | (o1 << 8);
        for (m, wr, xc, rd, own, other) in [("ds_wrxchg2_rtn_b32", 0x0eu32, 0x2eu32, 0x37u32, 5u32, 6u32), ("ds_wrxchg2st64_rtn_b32", 0x0f, 0x2f, 0x38, 1, 3)] {
            let x = ds(xc, offs, own, 4, 0, 10);
            let mut b = ds(wr, offs, own, 2, 4, 0).to_vec();
            b.extend_from_slice(&x);
            b.extend_from_slice(&ds(rd, offs, other, 0, 0, 12));
            cs.push(cc(m, x, b, &[10, 11, 12, 13], &|t| vec![dv(t), ev(t), ev(nb(t)), nb(t)]));
        }
    }
    let x = ds(0x2e, 0x0101, 5, 4, 0, 10);
    let mut b = ds(0x0d, 4, 5, 2, 0, 0).to_vec();
    b.extend_from_slice(&x);
    b.extend_from_slice(&ds(0x36, 4, 6, 0, 0, 12));
    cs.push(cc("ds_wrxchg2_rtn_b32", x, b, &[10, 11, 12], &|t| vec![dv(t), ev(t), nb(t)]));
    let x = ds(0x2e, 0x0100, 14, 15, 0, 14);
    let mut b = vec![vop1(1, 14, v(5)), vop1(1, 15, v(4))];
    b.extend_from_slice(&ds(0x0e, 0x0100, 5, 2, 4, 0));
    b.extend_from_slice(&x);
    b.extend_from_slice(&ds(0x37, 0x0100, 6, 0, 0, 12));
    cs.push(cc("ds_wrxchg2_rtn_b32", x, b, &[14, 15, 12, 13], &|t| vec![dv(t), ev(t), ev(nb(t)), nb(t)]));
    for (k, &(old, d0, d1)) in [(2u32, 4u32, 7u32), (4, 2, 0), (0, 13, 4), (13, 0, 2), (7, 0, 4), (4, 4, 2)].iter().enumerate() {
        let off = if k % 2 == 0 { 0 } else { 0x104 };
        let want = move |t: u32| {
            let (m, a, b) = (reg_val(old, t), reg_val(d0, t), reg_val(d1, t));
            if m >= a { m.wrapping_sub(a) } else { m.wrapping_add(b) }
        };
        let a = ds(0x34, off, 1, d0, d1, 11);
        let mut b = vec![vop1(1, 13, ic(0)), vop1(1, 11, v(4))];
        b.extend_from_slice(&ds(0x0d, off, 1, old, 0, 0));
        b.extend_from_slice(&a);
        b.extend_from_slice(&ds(0x36, off, 3, 0, 0, 10));
        cs.push(cc("ds_wrap_rtn_b32", a, b, &[11, 10], &move |t| vec![reg_val(old, t), want(nb(t))]));
    }
    run_cs_cases("ds_lds_mskor_wrxchg2", &cs);
}

#[test]
fn ds_lds_src2_compute() {
    let mut cs = Vec::new();
    type Op = fn(u32, u32) -> u32;
    let ops: [(&'static str, u32, Op); 13] = [
        ("ds_add_src2_u32", 0x80, |o, x| o.wrapping_add(x)),
        ("ds_sub_src2_u32", 0x81, |o, x| o.wrapping_sub(x)),
        ("ds_rsub_src2_u32", 0x82, |o, x| x.wrapping_sub(o)),
        ("ds_inc_src2_u32", 0x83, |o, x| if o >= x { 0 } else { o + 1 }),
        ("ds_dec_src2_u32", 0x84, |o, x| if o == 0 || o > x { x } else { o - 1 }),
        ("ds_min_src2_i32", 0x85, |o, x| (o as i32).min(x as i32) as u32),
        ("ds_max_src2_i32", 0x86, |o, x| (o as i32).max(x as i32) as u32),
        ("ds_min_src2_u32", 0x87, |o, x| o.min(x)),
        ("ds_max_src2_u32", 0x88, |o, x| o.max(x)),
        ("ds_and_src2_b32", 0x89, |o, x| o & x),
        ("ds_or_src2_b32", 0x8a, |o, x| o | x),
        ("ds_xor_src2_b32", 0x8b, |o, x| o ^ x),
        ("ds_write_src2_b32", 0x8d, |_o, x| x),
    ];
    for &(m, op, f) in &ops {
        for &(off_field, bbyte) in &[(0x0040u32, 0x0100u32), (0x7fc0u32, 0x7f00u32)] {
            let a = ds(op, off_field, 1, 0, 0, 0);
            let mut b = ds(0x0d, 0, 1, 4, 0, 0).to_vec();
            b.extend_from_slice(&ds(0x0d, bbyte, 1, 2, 0, 0));
            b.extend_from_slice(&a);
            b.extend_from_slice(&ds(0x36, 0, 1, 0, 0, 10));
            cs.push(cc(m, a, b, &[10], &move |t| vec![f(ev(t), dv(t))]));
        }
        for &(k, bbyte) in &[(64u32, 0x0100u32), (0x7fc0u32, 0x7f00u32)] {
            let a = ds(op, 0x8000, 13, 0, 0, 0);
            let mut b = vec![vop2(0x1c, 13, LIT, v(1)), k << 17];
            b.extend_from_slice(&ds(0x0d, 0, 13, 4, 0, 0));
            b.extend_from_slice(&ds(0x0d, bbyte, 13, 2, 0, 0));
            b.extend_from_slice(&a);
            b.extend_from_slice(&ds(0x36, 0, 13, 0, 0, 10));
            cs.push(cc(m, a, b, &[10], &move |t| vec![f(ev(t), dv(t))]));
        }
    }
    run_cs_cases("ds_lds_src2", &cs);
}

#[test]
fn ds_lds_b64_compute() {
    let mut cs = Vec::new();
    let nb = |t: u32| t ^ 1;
    let sh4 = [vop2(0x1a, 14, ic(4), 0), vop2(0x1a, 15, ic(4), 7)];
    for &off in &[0u32, 8, 0x404] {
        let w = ds(0x4d, off, 5, 2, 0, 0);
        for (vd, outs) in [(10u32, [10u32, 11]), (6, [6, 7])] {
            let r = ds(0x76, off, 6, 0, 0, vd);
            let mut b = w.to_vec();
            b.extend_from_slice(&r);
            cs.push(cc("ds_write_b64", w, b.clone(), &outs, &|t| vec![dv(nb(t)), t * 4]));
            cs.push(cc("ds_read_b64", r, b, &outs, &|t| vec![dv(nb(t)), t * 4]));
        }
    }
    for &(o0, o1) in &[(0u32, 1u32), (1, 0)] {
        let w = ds(0x4e, o0 | (o1 << 8), 14, 2, 4, 0);
        let r = ds(0x77, o0 | (o1 << 8), 15, 0, 0, 10);
        let mut b = sh4.to_vec();
        b.extend_from_slice(&w);
        b.extend_from_slice(&r);
        let f = |t: u32| vec![dv(t ^ 1), t * 4, ev(t ^ 1), (t ^ 1) * 8];
        cs.push(cc("ds_write2_b64", w, b.clone(), &[10, 11, 12, 13], &f));
        cs.push(cc("ds_read2_b64", r, b, &[10, 11, 12, 13], &f));
        let w = ds(0x4f, o0 | (o1 << 8), 5, 2, 4, 0);
        let r = ds(0x78, o0 | (o1 << 8), 6, 0, 0, 10);
        let mut b = w.to_vec();
        b.extend_from_slice(&r);
        cs.push(cc("ds_write2st64_b64", w, b.clone(), &[10, 11, 12, 13], &f));
        cs.push(cc("ds_read2st64_b64", r, b, &[10, 11, 12, 13], &f));
    }
    let r = ds(0x77, 0x0100, 15, 0, 0, 14);
    let w = ds(0x4e, 0x0100, 14, 2, 4, 0);
    let mut b = sh4.to_vec();
    b.extend_from_slice(&[vop2(0x1a, 15, ic(4), 7), vop1(1, 16, v(15))]);
    b.extend_from_slice(&w);
    b.extend_from_slice(&[vop1(1, 15, v(16))]);
    b.extend_from_slice(&r);
    cs.push(cc("ds_read2_b64", r, b, &[14, 15, 16, 17], &|t| vec![dv(t ^ 1), t * 4, ev(t ^ 1), (t ^ 1) * 8]));
    for &off in &[0u32, 0x10, 0x400] {
        for (wop, rop, n, wm, rm) in [(0xdeu32, 0xfeu32, 3usize, "ds_write_b96", "ds_read_b96"), (0xdf, 0xff, 4, "ds_write_b128", "ds_read_b128")] {
            let w = ds(wop, off, 14, 2, 0, 0);
            for vd in [10u32, 15] {
                let r = ds(rop, off, 15, 0, 0, vd);
                let mut b = sh4.to_vec();
                b.extend_from_slice(&w);
                b.extend_from_slice(&r);
                let outs: Vec<u32> = (vd..vd + n as u32).collect();
                let f = move |t: u32| vec![dv(t ^ 1), t * 4, ev(t ^ 1), (t ^ 1) * 8][..n].to_vec();
                cs.push(cc(wm, w, b.clone(), &outs, &f));
                cs.push(cc(rm, r, b, &outs, &f));
            }
        }
    }
    run_cs_cases("ds_lds_b64", &cs);
}

fn perm_body(mul: u32, add: u32, shr: u32) -> Vec<u32> {
    vec![
        vop2(0x09, 16, LIT, 0),
        mul,
        vop2(0x25, 16, LIT, 16),
        add,
        vop2(0x16, 16, ic(shr as i32), 16),
        vop2(0x1a, 16, ic(2), 16),
        vop2(0x25, 16, LIT, 16),
        0x8000_0080,
    ]
}

#[test]
fn ds_permute_compute() {
    let mut cs = Vec::new();
    let g = |t: u32| t & !31;
    for &(mul, add, off) in &[(7u32, 3u32, 0u32), (1, 0, 4), (13, 5, 0x7c), (0, 9, 0)] {
        let tgt = move |t: u32, o: u32| (((t * mul + add) * 4 + 0x8000_0080).wrapping_add(o) >> 2) & 31;
        let op = ds(0xb3, off, 16, 2, 0, 10);
        let mut b = perm_body(mul, add, 0);
        b.extend_from_slice(&op);
        cs.push(cc("ds_bpermute_b32", op, b, &[10], &move |t| vec![dv(g(t) + tgt(t, off))]));
        let op = ds(0xb2, off, 16, 2, 0, 10);
        let mut b = perm_body(mul, add, 0);
        b.extend_from_slice(&op);
        cs.push(cc("ds_permute_b32", op, b, &[10], &move |t| {
            let src = (0..32u32).filter(|&k| tgt(g(t) + k, off) == t & 31).max();
            vec![src.map(|k| dv(g(t) + k)).unwrap_or(0)]
        }));
    }
    let op = ds(0xb2, 0, 16, 2, 0, 10);
    let mut b = perm_body(1, 0, 1);
    b.extend_from_slice(&op);
    cs.push(cc("ds_permute_b32", op, b, &[10], &move |t| {
        let src = (0..32u32).filter(|&k| (((g(t) + k) >> 1) & 31) == t & 31).max();
        vec![src.map(|k| dv(g(t) + k)).unwrap_or(0)]
    }));
    run_cs_cases("ds_permute", &cs);
}

#[test]
fn ds_swizzle_compute() {
    let mut cs = Vec::new();
    let g = |t: u32| t & !31;
    let bit = |off: u32, l: u32| (((l & (off & 0x1f)) | ((off >> 5) & 0x1f)) ^ ((off >> 10) & 0x1f)) & 31;
    let quad = |off: u32, l: u32| (l & !3) | ((off >> (2 * (l & 3))) & 3);
    for &off in &[0x001fu32, 0x041f, 0x081f, 0x101f, 0x401f, 0x0010, 0x03e0, 0x00a0, 0x0c1b, 0x7c00, 0x1fa5, 0x7fff] {
        let op = ds(0x35, off, 2, 0, 0, 10);
        cs.push(cc("ds_swizzle_b32", op, op.to_vec(), &[10], &move |t| vec![dv(g(t) + bit(off, t & 31))]));
    }
    for &m in &[0xe4u32, 0x1b, 0x55, 0xaa, 0x4e, 0xb1, 0x00, 0xff] {
        let off = 0x8000 | m;
        let op = ds(0x35, off, 2, 0, 0, 10);
        cs.push(cc("ds_swizzle_b32", op, op.to_vec(), &[10], &move |t| vec![dv(g(t) + quad(off, t & 31))]));
    }
    run_cs_cases("ds_swizzle_cs", &cs);
}

fn rotate_source(off: u32, lane: u32) -> u32 {
    let mask = off & 31;
    let n = (off >> 5) & 31;
    let amount = if off & 0x400 == 0 { n } else { (32 - n) % 32 };
    (lane & mask) | (((lane + amount) & 31) & !mask & 31)
}

fn fft_source(off: u32, lane: u32) -> u32 {
    let mask = off & 31;
    let reversed = (0..5).fold(0, |a, b| a | (((lane >> b) & 1) << (4 - b)));
    (reversed >> mask.count_ones()) | (lane & mask)
}

#[test]
fn ds_swizzle_rotate_and_fft_compute() {
    assert_eq!((0..4).map(|l| fft_source(0xe000, l)).collect::<Vec<_>>(), [0, 16, 8, 24]);
    assert!((0..32).all(|l| fft_source(0xe01f, l) == l));
    assert_eq!([1, 16, 17].map(|l| fft_source(0xe010, l)), [8, 16, 24]);
    assert_eq!((0..4).map(|l| rotate_source(0xc020, l)).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert_eq!((0..4).map(|l| rotate_source(0xc420, l)).collect::<Vec<_>>(), [31, 0, 1, 2]);
    assert_eq!((0..8).map(|l| rotate_source(0xc060 | 0x18, l)).collect::<Vec<_>>(), [3, 4, 5, 6, 7, 0, 1, 2]);
    let g = |t: u32| t & !31;
    let mut cs = Vec::new();
    let mut offsets = Vec::new();
    for dir in [0u32, 1] {
        for n in [0u32, 1, 3, 31] {
            for mask in [0x00u32, 0x18, 0x1c, 0x01] {
                offsets.push(0xc000 | (dir << 10) | (n << 5) | mask);
            }
        }
    }
    offsets.extend([0xd800u32 | 0x0460 | 0x18, 0xc000 | (3 << 5) | 0x1f, 0xdbff]);
    for &off in &offsets {
        let op = ds(0x35, off, 2, 0, 0, 10);
        cs.push(cc("ds_swizzle_b32", op, op.to_vec(), &[10], &move |t| vec![dv(g(t) + rotate_source(off, t & 31))]));
    }
    for &mask in &[0x00u32, 0x10, 0x1f, 0x18, 0x1c, 0x1e, 0x01, 0x0f, 0x15, 0x08] {
        let off = 0xe000 | mask;
        let op = ds(0x35, off, 2, 0, 0, 10);
        cs.push(cc("ds_swizzle_b32", op, op.to_vec(), &[10], &move |t| vec![dv(g(t) + fft_source(off, t & 31))]));
    }
    for &off in &[0xffe0u32, 0xf550, 0xfffe, 0xe3f1] {
        let op = ds(0x35, off, 2, 0, 0, 10);
        cs.push(cc("ds_swizzle_b32", op, op.to_vec(), &[10], &move |t| vec![dv(g(t) + fft_source(off, t & 31))]));
    }
    run_cs_cases("ds_swizzle_rotate_fft", &cs);
}

#[test]
fn ds_permute_with_lds_is_refused() {
    let mut b = perm_body(1, 0, 0);
    b.extend_from_slice(&ds(0x0d, 0, 1, 2, 0, 0));
    b.extend_from_slice(&ds(0xb3, 0, 16, 2, 0, 10));
    let w = cs_prog(&b, &[10]);
    let e = match translate(&cs_extract(&w, T)) {
        Ok(_) => panic!("permute with LDS translated"),
        Err(e) => e,
    };
    assert!(e.contains("groupshared"), "{e}");
}

#[test]
fn glc_and_dlc_buffer_accesses_are_globally_coherent() {
    let load = |glc: u32, dlc: u32| [0xe000_0000 | (0x0c << 18) | (dlc << 15) | (glc << 14) | (1 << 12), 1 | (10 << 8) | (2 << 16) | (0x80 << 24)];
    let atomic = [0xe000_0000 | (0x32 << 18) | (1 << 14) | (1 << 12), 1 | (10 << 8) | (2 << 16) | (0x80 << 24)];
    let glc_then_atomic = [load(1, 0), atomic].concat();
    let cases: [(Vec<u32>, &[u32], bool); 7] = [
        (load(0, 0).to_vec(), &[10], false),
        (load(1, 0).to_vec(), &[10], true),
        (load(0, 1).to_vec(), &[10], true),
        (load(1, 1).to_vec(), &[10], true),
        (atomic.to_vec(), &[10], false),
        (load(1, 0).to_vec(), &[], false),
        (glc_then_atomic, &[], true),
    ];
    for (k, (op, outs, want)) in cases.into_iter().enumerate() {
        let h = translate(&cs_extract(&cs_prog(&op, outs), T)).unwrap_or_else(|e| panic!("case {k}: {e}")).hlsl;
        let uavs: Vec<&str> = h.lines().filter(|l| l.contains(": register(u")).collect();
        assert!(!uavs.is_empty(), "case {k}:\n{h}");
        assert!(uavs.iter().all(|l| l.starts_with("globallycoherent ") == want), "case {k}:\n{h}");
        assert_eq!(h.matches("globallycoherent").count(), if want { uavs.len() } else { 0 }, "case {k}:\n{h}");
    }
}
#[derive(Clone, Copy)]
enum Src {
    Lit(u32, u32),
    Mem,
    Swap,
}

fn val64(s: Src, t: u32) -> u64 {
    let (lo, hi) = match s {
        Src::Lit(lo, hi) => (lo, hi),
        Src::Mem => (dv(t), ev(t)),
        Src::Swap => (ev(t), dv(t)),
    };
    (hi as u64) << 32 | lo as u64
}

fn load64(b: &mut Vec<u32>, reg: u32, s: Src) {
    match s {
        Src::Lit(lo, hi) => b.extend_from_slice(&[vop1(1, reg, LIT), lo, vop1(1, reg + 1, LIT), hi]),
        Src::Mem => b.extend_from_slice(&[vop1(1, reg, v(2)), vop1(1, reg + 1, v(4))]),
        Src::Swap => b.extend_from_slice(&[vop1(1, reg, v(4)), vop1(1, reg + 1, v(2))]),
    }
}

fn op64(key: &str, o: u64, d: u64, e: u64) -> u64 {
    match key {
        "add" => o.wrapping_add(d),
        "sub" => o.wrapping_sub(d),
        "rsub" => d.wrapping_sub(o),
        "inc" => if o >= d { 0 } else { o + 1 },
        "dec" => if o == 0 || o > d { d } else { o - 1 },
        "min_i" => if (d as i64) < (o as i64) { d } else { o },
        "max_i" => if (d as i64) > (o as i64) { d } else { o },
        "min_u" => o.min(d),
        "max_u" => o.max(d),
        "and" => o & d,
        "or" => o | d,
        "xor" => o ^ d,
        "mskor" => (o & !d) | e,
        "cmpst" => if o == d { e } else { o },
        _ => d,
    }
}

const OPS64: [(&str, u32, &str, &str); 15] = [
    ("add", 0x40, "ds_add_u64", "ds_add_rtn_u64"),
    ("sub", 0x41, "ds_sub_u64", "ds_sub_rtn_u64"),
    ("rsub", 0x42, "ds_rsub_u64", "ds_rsub_rtn_u64"),
    ("inc", 0x43, "ds_inc_u64", "ds_inc_rtn_u64"),
    ("dec", 0x44, "ds_dec_u64", "ds_dec_rtn_u64"),
    ("min_i", 0x45, "ds_min_i64", "ds_min_rtn_i64"),
    ("max_i", 0x46, "ds_max_i64", "ds_max_rtn_i64"),
    ("min_u", 0x47, "ds_min_u64", "ds_min_rtn_u64"),
    ("max_u", 0x48, "ds_max_u64", "ds_max_rtn_u64"),
    ("and", 0x49, "ds_and_b64", "ds_and_rtn_b64"),
    ("or", 0x4a, "ds_or_b64", "ds_or_rtn_b64"),
    ("xor", 0x4b, "ds_xor_b64", "ds_xor_rtn_b64"),
    ("mskor", 0x4c, "ds_mskor_b64", "ds_mskor_rtn_b64"),
    ("cmpst", 0x50, "ds_cmpst_b64", "ds_cmpst_rtn_b64"),
    ("xchg", 0x4d, "", "ds_wrxchg_rtn_b64"),
];

fn lds_fssm(granules: u32) -> Vec<u8> {
    cs_fssm(granules << 15 | 0x10)
}

fn one64(cs: &mut Vec<CsCase>, k: usize, rtn: bool, off: u32, mem: Src, d: Src, e: Src) {
    let (key, op, plain, ret) = OPS64[k];
    let mut b = Vec::new();
    load64(&mut b, 20, mem);
    load64(&mut b, 22, d);
    load64(&mut b, 24, e);
    b.extend_from_slice(&[vop1(1, 10, LIT), 0x5a5a_0001, vop1(1, 11, LIT), 0x5a5a_0002]);
    b.extend_from_slice(&ds(0x4d, off, 5, 20, 0, 0));
    let w = ds(if rtn { op + 0x20 } else { op }, off, 5, 22, 24, 10);
    b.extend_from_slice(&w);
    b.extend_from_slice(&ds(0x76, off, 5, 0, 0, 14));
    cs.push(cc(if rtn { ret } else { plain }, w, b, &[10, 11, 14, 15], &move |t| {
        let o = val64(mem, t);
        let n = op64(key, o, val64(d, t), val64(e, t));
        let (r0, r1) = if rtn { (o as u32, (o >> 32) as u32) } else { (0x5a5a_0001, 0x5a5a_0002) };
        vec![r0, r1, n as u32, (n >> 32) as u32]
    }));
}

fn all64(cs: &mut Vec<CsCase>, k: usize, off: u32, mem: Src, d: Src, e: Src) {
    if !OPS64[k].2.is_empty() {
        one64(cs, k, false, off, mem, d, e);
    }
    one64(cs, k, true, off, mem, d, e);
}

#[test]
fn ds_lds_atomic64() {
    let mut cs = Vec::new();
    let e = Src::Lit(0x0f0f_00ff, 0x1234_8000);
    for k in 0..OPS64.len() {
        all64(&mut cs, k, 0, Src::Mem, Src::Lit(0xffff_ff00, 0x8000_0000), e);
        all64(&mut cs, k, 0x104, Src::Mem, Src::Swap, e);
    }
    let edge: [(usize, Src, Src); 12] = [
        (0, Src::Lit(0xffff_ffff, 0), Src::Lit(1, 0)),
        (0, Src::Lit(0xffff_ffff, 0xffff_ffff), Src::Lit(1, 0)),
        (1, Src::Lit(0, 1), Src::Lit(1, 0)),
        (2, Src::Lit(1, 0), Src::Lit(0, 1)),
        (3, Src::Lit(0xffff_ffff, 0), Src::Lit(0, 0x10)),
        (3, Src::Lit(7, 7), Src::Lit(7, 7)),
        (4, Src::Lit(0, 0), Src::Lit(9, 9)),
        (4, Src::Lit(0, 1), Src::Lit(0, 2)),
        (5, Src::Lit(0, 0x8000_0000), Src::Lit(1, 0)),
        (7, Src::Lit(0, 0x8000_0000), Src::Lit(1, 0)),
        (6, Src::Lit(0xffff_ffff, 0xffff_ffff), Src::Lit(0, 0)),
        (8, Src::Lit(0xffff_ffff, 0xffff_ffff), Src::Lit(0, 0)),
    ];
    for &(k, mem, d) in &edge {
        all64(&mut cs, k, 8, mem, d, e);
    }
    all64(&mut cs, 13, 0, Src::Mem, Src::Mem, e);
    all64(&mut cs, 1, 0, Src::Mem, Src::Mem, e);
    {
        let mut b = Vec::new();
        load64(&mut b, 20, Src::Mem);
        load64(&mut b, 22, Src::Swap);
        b.extend_from_slice(&ds(0x4d, 0, 5, 20, 0, 0));
        let w = ds(0x6d, 0, 5, 22, 0, 22);
        b.extend_from_slice(&w);
        b.extend_from_slice(&ds(0x76, 0, 5, 0, 0, 14));
        cs.push(cc("ds_wrxchg_rtn_b64", w, b, &[22, 23, 14, 15], &|t| vec![dv(t), ev(t), ev(t), dv(t)]));
    }
    for &(m, op, unit, shift, o0, o1) in &[
        ("ds_wrxchg2_rtn_b64", 0x6eu32, 8u32, 5i32, 0u32, 1u32),
        ("ds_wrxchg2_rtn_b64", 0x6e, 8, 5, 3, 1),
        ("ds_wrxchg2st64_rtn_b64", 0x6f, 512, 4, 0, 2),
        ("ds_wrxchg2st64_rtn_b64", 0x6f, 512, 4, 2, 0),
    ] {
        for back in [false, true] {
            let mut b = vec![vop2(0x1a, 26, ic(shift), 0)];
            load64(&mut b, 20, Src::Mem);
            load64(&mut b, 28, Src::Swap);
            load64(&mut b, 22, Src::Lit(0x1111_2222, 0x3333_4444));
            load64(&mut b, 24, Src::Lit(0x5555_6666, 0x7777_8888));
            b.extend_from_slice(&ds(0x4d, o0 * unit, 26, 20, 0, 0));
            b.extend_from_slice(&ds(0x4d, o1 * unit, 26, 28, 0, 0));
            let w = ds(op, o0 | o1 << 8, 26, 22, 24, 10);
            b.extend_from_slice(&w);
            b.extend_from_slice(&ds(0x76, o0 * unit, 26, 0, 0, 14));
            b.extend_from_slice(&ds(0x76, o1 * unit, 26, 0, 0, 16));
            if back {
                cs.push(cc(m, w, b, &[14, 15, 16, 17], &|_| vec![0x1111_2222, 0x3333_4444, 0x5555_6666, 0x7777_8888]));
            } else {
                cs.push(cc(m, w, b, &[10, 11, 12, 13], &|t| vec![dv(t), ev(t), ev(t), dv(t)]));
            }
        }
    }
    let init = Src::Lit(0x8000_0001, 0x0000_0003);
    for &k in &[0usize, 11, 8, 5, 1] {
        let (key, op, plain, _) = OPS64[k];
        let mut b = vec![vop1(1, 30, LIT), 0x140];
        load64(&mut b, 20, init);
        load64(&mut b, 22, Src::Mem);
        b.extend_from_slice(&ds(0x4d, 0, 30, 20, 0, 0));
        let w = ds(op, 0, 30, 22, 0, 10);
        b.extend_from_slice(&w);
        b.extend_from_slice(&ds(0x76, 0, 30, 0, 0, 14));
        let want = (0..T).fold(val64(init, 0), |a, t| op64(key, a, val64(Src::Mem, t), 0));
        cs.push(cc(plain, w, b, &[14, 15], &move |_| vec![want as u32, (want >> 32) as u32]));
    }
    for flip in [false, true] {
        let (first, second) = if flip { (0x41u32, 0x40u32) } else { (0x40, 0x41) };
        let mut b = vec![vop1(1, 30, LIT), 0x140, vop2(0x1b, 31, ic(1), 0)];
        load64(&mut b, 20, init);
        load64(&mut b, 22, Src::Mem);
        b.extend_from_slice(&ds(0x4d, 0, 30, 20, 0, 0));
        let w = ds(first, 0, 30, 22, 0, 10);
        b.extend_from_slice(&[vopc(0xc2, ic(0), 31), sopp(0x06, 3)]);
        b.extend_from_slice(&w);
        b.push(sopp(0x02, 2));
        b.extend_from_slice(&ds(second, 0, 30, 22, 0, 10));
        b.extend_from_slice(&ds(0x76, 0, 30, 0, 0, 14));
        let want = (0..T).fold(val64(init, 0), |a, t| op64(if (t & 1 == 0) != flip { "add" } else { "sub" }, a, val64(Src::Mem, t), 0));
        cs.push(cc(OPS64[flip as usize].2, w, b, &[14, 15], &move |_| vec![want as u32, (want >> 32) as u32]));
    }
    run_cs_cases_fssm("ds_lds_atomic64", &cs, &lds_fssm(5));
}

#[test]
fn ds_lds_atomic64_sizing() {
    let body = |off: u32| cs_prog(&ds(0x40, off, 5, 22, 24, 10), &[10]);
    let err = |words: &[u32], fssm: &[u8]| translate(&cs_extract_fssm(words, T, fssm)).map(|t| t.hlsl).unwrap_err();
    assert!(err(&body(0), &[]).contains("COMPUTE_PGM_RSRC2 not found"));
    assert!(err(&body(0), &cs_fssm_sub(7, 0x130, 5 << 15)).contains("sub-version 0x7 has no known location"));
    assert!(err(&body(0), &cs_fssm_sub(6, 0x130, 5 << 15)).contains("declares no LDS"));
    let h = translate(&cs_extract_fssm(&body(0x9f8), T, &cs_fssm_sub(6, 0x138, 5 << 15))).map(|t| t.hlsl).unwrap();
    assert!(h.contains("groupshared uint tb_lds[640];") && h.contains("% 640u]") && !h.contains("& 8191u]"));
    assert!(err(&body(0), &lds_fssm(0)).contains("declares no LDS"));
    assert!(err(&body(0), &lds_fssm(64)).contains("no room for the lock word"));
    assert!(err(&body(0), &cs_fssm(5 << 15 | 0x50)).contains("not trusted as an LDS size"));
    assert!(err(&body(0), &cs_fssm(5 << 15 | 0x0100_0010)).contains("not trusted as an LDS size"));
    assert!(err(&body(0x9fc), &lds_fssm(5)).contains("past the 2560 bytes the shader declares"));
    let h = translate(&cs_extract_fssm(&body(0x9f8), T, &lds_fssm(5))).map(|t| t.hlsl).unwrap();
    assert!(h.contains("groupshared uint tb_lds[640];") && h.contains("groupshared uint tb_lds64_lock;") && h.contains("% 640u]") && !h.contains("& 8191u]"));
    assert!(h.contains("tb_lds64_lock = 0u;\n    GroupMemoryBarrierWithGroupSync();"));
    let h = translate(&cs_extract_fssm(&body(0), T, &lds_fssm(63))).map(|t| t.hlsl).unwrap();
    assert!(h.contains("groupshared uint tb_lds[8064];"));
    let arms = |second: [u32; 2]| {
        let mut b = vec![vop2(0x1b, 31, ic(1), 0), vopc(0xc2, ic(0), 31), sopp(0x06, 3)];
        b.extend_from_slice(&ds(0x40, 0, 5, 22, 0, 10));
        b.push(sopp(0x02, 2));
        b.extend_from_slice(&second);
        cs_prog(&b, &[10])
    };
    for other in [ds(0x0d, 0, 1, 2, 0, 0), ds(0x00, 0, 1, 2, 0, 10), ds(0x36, 0, 1, 0, 0, 10)] {
        assert!(err(&arms(other), &lds_fssm(5)).contains("serialised only against each other"));
    }
    assert!(translate(&cs_extract_fssm(&arms(ds(0x41, 0, 5, 22, 0, 10)), T, &lds_fssm(5))).is_ok());
    let plain = cs_prog(&ds(0x00, 0, 1, 2, 0, 10), &[10]);
    for fssm in [Vec::new(), lds_fssm(5)] {
        let h = translate(&cs_extract_fssm(&plain, T, &fssm)).map(|t| t.hlsl).unwrap();
        assert!(h.contains("groupshared uint tb_lds[8192];") && h.contains("& 8191u]") && !h.contains("tb_lds64_lock"));
    }
}
