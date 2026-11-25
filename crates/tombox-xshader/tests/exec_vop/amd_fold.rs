use super::*;

const NZ: u32 = 0x8000_0000;

fn f64_neg(c: &mut Cases, m: &'static str, op: u32, x: f64, f: fn(f64) -> f64) {
    let [lo, hi] = dw(x);
    let [ol, oh] = dw(f(-x));
    vc(c, m, &[(2, lo), (3, hi)], &vop3m(0x180 + op, 10, [v(2), 0, 0], 0, 1, 0, false), &[10, 11], &[ol, oh]);
}

#[test]
fn amd_host_zero_sign_and_f64_neg() {
    let mut c = Cases::new();
    for (a, want) in [(NZ, 0u32), (0, 0), (fb(-1.5), fb(-1.5))] {
        vc(&mut c, "v_add_f32", &[(1, a)], &[vop2(0x03, 10, ic(0), 1)], &[10], &[want]);
    }
    vc(&mut c, "v_add_f32", &[(1, NZ)], &vop3m(0x103, 10, [ic(0), v(1), 0], 0, 1, 0, false), &[10], &[NZ]);
    for (m, op) in [("v_fmaak_f32", 0x2du32), ("v_madak_f32", 0x21)] {
        for (a, b, k, want) in [(NZ, 0u32, 0u32, 0u32), (fb(-1.0), 0, 0, 0), (fb(2.0), fb(3.0), 0, fb(6.0)), (NZ, 0, NZ, NZ)] {
            vc(&mut c, m, &[(1, a), (2, b)], &[vop2(op, 10, v(1), 2), k], &[10], &[want]);
        }
        let fused = op == 0x2d;
        vc(&mut c, m, &[(1, fb(-1.0e-30)), (2, fb(1.0e-30))], &[vop2(op, 10, v(1), 2), 0], &[10], &[if fused { NZ } else { 0 }]);
    }
    vc(&mut c, "v_fma_f32", &[(1, NZ), (2, fb(1.0))], &vop3(0x14b, 10, [v(1), v(2), ic(0)]), &[10], &[0]);
    vc(&mut c, "v_fma_f32", &[(1, NZ), (2, fb(1.0))], &vop3m(0x14b, 10, [v(1), v(2), ic(0)], 0, 4, 0, false), &[10], &[NZ]);
    vc(&mut c, "v_fmaak_f16", &[(1, 0x8000), (2, 0x3c00)], &[vop2(0x38, 10, v(1), 2), 0], &[10], &[D16]);
    vc(&mut c, "v_add_f16", &[(1, 0x8000)], &[vop2(0x32, 10, ic(0), 1)], &[10], &[D16]);
    for e in [0u32, 5, 0xffff_fffb] {
        vc(&mut c, "v_ldexp_f16", &[(1, 0x8000), (2, e)], &[vop2(0x3b, 10, v(1), 2)], &[10], &[D16 | 0x8000]);
    }
    for x in [1.0f64, 2.5, -2.5, 0.5, -0.75, 1e20, 3.0e-310] {
        f64_neg(&mut c, "v_trunc_f64", 0x17, x, f64::trunc);
        f64_neg(&mut c, "v_ceil_f64", 0x18, x, f64::ceil);
        f64_neg(&mut c, "v_floor_f64", 0x1a, x, f64::floor);
        if x.abs() < 1e300 && x.abs() > 1e-300 {
            f64_neg(&mut c, "v_fract_f64", 0x3e, x, |y| y - y.floor());
        }
    }
    c.run_both("amd_host_zero_sign_and_f64_neg");
}
