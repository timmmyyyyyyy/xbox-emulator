use super::*;
use tombox_xshader::hlsl::translate_user_data;
use tombox_xshader::user_data_abi::TB_USER_DATA_DWORDS;

const A64: u64 = 0x0000_0410_0f00_0000;

fn texel(_m: u32, _l: u32, x: u32, y: u32) -> [u32; 4] {
    let v = (y * 10 + x) as f32;
    [fb(v), fb(v + 100.0), fb(v + 200.0), fb(v + 300.0)]
}

const IMG: Img = Img { w: 8, h: 8, layers: 1, array: false, mips: 1, fmt: R32G32B32A32_FLOAT, texel };

fn ops() -> [(&'static str, u32, fn(u64) -> u32); 6] {
    [
        ("s_bcnt0_i32_b64", 0x0e, |a| a.count_zeros()),
        ("s_bcnt1_i32_b64", 0x10, |a| a.count_ones()),
        ("s_ff0_i32_b64", 0x12, |a| ff(!a, 64)),
        ("s_ff1_i32_b64", 0x14, |a| ff(a, 64)),
        ("s_flbit_i32_b64", 0x16, |a| if a == 0 { u32::MAX } else { a.leading_zeros() }),
        ("s_flbit_i32_i64", 0x18, flbit_i64),
    ]
}

fn decodes_as(word: u32, m: &str) {
    let got = tombox_xshader::rdna2::disassemble(&word.to_le_bytes()).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
    assert_eq!(got, m);
}

fn table_copy_prog(op: u32, x: u32, y: u32) -> Vec<u32> {
    let mut p = Prog::new();
    p.sv(22, A64 as u32).sv(23, (A64 >> 32) as u32);
    p.raw(&[sop1(3, 20, P_SRV), sop1(3, 21, P_SRV + 1)]);
    p.raw(&[sop1(op, 19, 22)]);
    p.raw(&smem(3, 0, 20, 0, SNULL));
    p.vv(1, x).vv(2, y);
    p.raw(&super::mimg::mimg(0x00, 0xf, 1, 1, 4, 0, 0));
    p.raw(&[0xbf8c_3f70]);
    p.raw(&[vop1(1, 0, 19), vop1(1, 1, v(4)), vop1(1, 2, v(5)), vop1(1, 3, v(7))]);
    p.words()
}

#[test]
fn table_pointer_in_the_next_sgpr_survives_a_32_bit_result_of_a_64_bit_scalar_op() {
    let mut fails = Vec::new();
    for (m, op, f) in ops() {
        decodes_as(sop1(op, 19, 22), m);
        let (x, y) = (5u32, 3u32);
        let t = texel(0, 0, x, y);
        let want = [f(A64), t[0], t[1], t[3]];
        match exec(&table_copy_prog(op, x, y), &Env { tex: Some(&IMG), ..Default::default() }) {
            Ok(o) if o.rt == want => {}
            Ok(o) => fails.push(format!("{m}: rt {:08x?} want {want:08x?}", o.rt)),
            Err(e) => fails.push(format!("{m}: {}", e.lines().take(3).collect::<Vec<_>>().join(" | "))),
        }
    }
    assert!(fails.is_empty(), "{} failing\n{}", fails.len(), fails.join("\n"));
}

#[test]
fn user_data_in_the_next_sgpr_stays_live_after_a_32_bit_result_of_a_64_bit_scalar_op() {
    let mut ud = [0u32; TB_USER_DATA_DWORDS];
    for (i, w) in ud.iter_mut().enumerate() {
        *w = 0x5d00_0000 | (i as u32 * 0x0301);
    }
    ud[0] = A64 as u32;
    ud[1] = (A64 >> 32) as u32;
    let a = A64;
    let mut fails = Vec::new();
    for (m, op, f) in ops() {
        let mut p = Prog::new();
        p.raw(&[sop1(op, 7, 0)]);
        for (k, s) in [7u32, 8, 9, 2].iter().enumerate() {
            p.raw(&[vop1(1, k as u32, *s)]);
        }
        let words = p.words();
        let t = translate_user_data(&shader(&words)).unwrap_or_else(|e| panic!("{m}: {e}"));
        let live = t.hlsl.lines().next().unwrap_or_default().to_string();
        if !live.split_whitespace().any(|r| r == "s8") {
            fails.push(format!("{m}: s8 missing from the entry-live registers: {live}"));
            continue;
        }
        let want = [f(a), ud[8], ud[9], ud[2]];
        match super::user_data::draw_user_data(&words, &ud) {
            Ok(got) if got == want => {}
            Ok(got) => fails.push(format!("{m}: rt {got:08x?} want {want:08x?}")),
            Err(e) => fails.push(format!("{m}: {}", e.lines().take(3).collect::<Vec<_>>().join(" | "))),
        }
    }
    assert!(fails.is_empty(), "{} failing\n{}", fails.len(), fails.join("\n"));
}
