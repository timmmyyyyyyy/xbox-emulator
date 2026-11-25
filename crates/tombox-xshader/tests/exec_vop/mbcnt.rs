use super::*;

const MASKS: [u32; 6] = [0, 1, 0x8000_0000, 0xffff_ffff, 0x5555_5555, 0xdead_beef];
const ADDENDS: [u32; 5] = [0, 1, 31, 0x8000_0000, 0xffff_ffff];

#[test]
fn v_mbcnt_has_no_lane_below_lane_zero() {
    let mut c = Cases::new();
    for (m, op) in [("v_mbcnt_lo_u32_b32", 0x365), ("v_mbcnt_hi_u32_b32", 0x366)] {
        for &mask in &MASKS {
            for &add in &ADDENDS {
                vc(&mut c, m, &[(1, mask), (2, add)], &vop3(op, 10, [v(1), v(2), 0]), &[10], &[add]);
            }
        }
        vc(&mut c, m, &[(2, 7)], &vop3(op, 10, [ic(-1), v(2), 0]), &[10], &[7]);
        vc(&mut c, m, &[(1, 0xffff_ffff)], &vop3(op, 10, [v(1), ic(5), 0]), &[10], &[5]);
        vc(&mut c, m, &[(1, 0xffff_ffff)], &vop3(op, 10, [v(1), ic(-16), 0]), &[10], &[0xffff_fff0]);
    }
    c.run_both("mbcnt");
}
