use super::*;

const OUT: u32 = 1024;
const NEW: u32 = 0xc0de_f00d;

fn mu(op: u32, offset: u32, offen: bool, vaddr: u32, vdata: u32, soff: u32) -> [u32; 2] {
    [0xe000_0000 | (op << 18) | ((offen as u32) << 12) | offset, vaddr | (vdata << 8) | (2 << 16) | (soff << 24)]
}

fn data() -> Vec<u32> {
    (0..16u32).map(|i| (i.wrapping_mul(0x9e37_79b9) ^ 0x3c5a_96e1).rotate_left(i * 5)).collect()
}

fn case(m: &'static str, inv: u32, store_first: bool, at: u32) -> CsCase {
    let init = data();
    let invalidate = mu(inv, 0, false, 0, 0, 0x80);
    let mut w = smem(2, 8, 4, 0, SNULL).to_vec();
    w.extend_from_slice(&[vop1(1, 1, LIT), at, vop1(1, 11, LIT), NEW]);
    if store_first {
        w.extend_from_slice(&mu(0x1c, 0, true, 1, 11, 0x80));
    }
    w.extend_from_slice(&invalidate);
    w.extend_from_slice(&mu(0x0c, 0, true, 1, 10, 0x80));
    w.extend_from_slice(&mu(0x1c, OUT, false, 0, 10, 0x80));
    w.push(S_ENDPGM);
    let word = (at / 4) as usize;
    let seen = if store_first { NEW } else { init[word] };
    let mut mem = vec![((OUT / 4) as usize, seen)];
    for k in 0..init.len() {
        mem.push((k, if store_first && k == word { NEW } else { init[k] }));
    }
    CsCase { m, op: invalidate.to_vec(), words: w, threads: 1, mem, init }
}

#[test]
fn buffer_gl_inv_leaves_the_buffer_contents_alone() {
    let mut cs = Vec::new();
    for (m, inv) in [("buffer_gl0_inv", 0x71u32), ("buffer_gl1_inv", 0x72)] {
        for store_first in [false, true] {
            for at in [0u32, 12, 60] {
                cs.push(case(m, inv, store_first, at));
            }
        }
    }
    run_cs_cases("buffer_gl_inv", &cs);
}
