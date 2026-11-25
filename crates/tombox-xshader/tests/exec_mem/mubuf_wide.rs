use super::*;

const OUT: u32 = 1024;
const NAMES_LOAD: [&str; 3] = ["buffer_load_dwordx2", "buffer_load_dwordx3", "buffer_load_dwordx4"];
const NAMES_STORE: [&str; 3] = ["buffer_store_dwordx2", "buffer_store_dwordx3", "buffer_store_dwordx4"];
const GUARD: u32 = 0x7777_7777;

fn mu(op: u32, offset: u32, offen: bool, vaddr: u32, vdata: u32, soff: u32) -> [u32; 2] {
    [0xe000_0000 | (op << 18) | ((offen as u32) << 12) | offset, vaddr | (vdata << 8) | (2 << 16) | (soff << 24)]
}

fn op_step(n: u32) -> u32 {
    [0, 1, 3, 2][n as usize - 1]
}

fn data() -> Vec<u32> {
    (0..16u32).map(|i| (i.wrapping_mul(0x9e37_79b9) ^ 0x3c5a_96e1).rotate_left(i * 5)).collect()
}

fn sop1_mov(s: u32) -> u32 {
    0xbe80_0300 | (s << 16) | 0xff
}

fn prog(pre: &[u32]) -> Vec<u32> {
    let mut w = smem(2, 8, 4, 0, SNULL).to_vec();
    w.extend_from_slice(pre);
    w
}

fn addressing(at: u32, mode: u32) -> (u32, u32, bool, u32, Vec<u32>) {
    match mode {
        0 => (at, 0, true, 0x80, vec![]),
        1 => (0, at, false, 0x80, vec![]),
        2 => (1, 1, true, 0, vec![sop1_mov(0), at - 2]),
        _ => (0, 0, false, 0, vec![sop1_mov(0), at]),
    }
}

fn store_out(vdata: u32, k: u32) -> [u32; 2] {
    mu(0x1c, OUT + 4 * k, false, 0, vdata, 0x80)
}

fn load_case(n: u32, at: u32, mode: u32) -> CsCase {
    let init = data();
    let (vofs, imm, offen, soff, scalar) = addressing(at, mode);
    let op = mu(0x0c + op_step(n), imm, offen, 1, 10, soff);
    let mut w = prog(&[vop1(1, 1, LIT), vofs, vop1(1, 10 + n, LIT), GUARD]);
    w.extend_from_slice(&scalar);
    w.extend_from_slice(&op);
    for k in 0..=n {
        w.extend_from_slice(&store_out(10 + k, k));
    }
    w.push(S_ENDPGM);
    let first = (at / 4) as usize;
    let mut mem: Vec<(usize, u32)> = (0..n as usize).map(|k| ((OUT / 4) as usize + k, init[first + k])).collect();
    mem.push(((OUT / 4 + n) as usize, GUARD));
    CsCase { m: NAMES_LOAD[n as usize - 2], op: op.to_vec(), words: w, threads: 1, mem, init }
}

#[test]
fn buffer_load_dwordx_reads_every_register() {
    let mut cs = Vec::new();
    for n in 2..=4u32 {
        for at in [0u32, 8, 20, 48] {
            if at + 4 * n > 64 {
                continue;
            }
            for mode in 0..4u32 {
                if mode == 2 && at < 2 {
                    continue;
                }
                cs.push(load_case(n, at, mode));
            }
        }
    }
    run_cs_cases("mubuf_wide_load", &cs);
}

fn store_case(n: u32, at: u32, mode: u32) -> CsCase {
    let (vofs, imm, offen, soff, scalar) = addressing(at, mode);
    let op = mu(0x1c + op_step(n), imm, offen, 1, 10, soff);
    let vals: Vec<u32> = (0..n).map(|k| 0xa500_0000 | ((0x11 * (k + 1)) << 8) | (k + 1)).collect();
    let mut w = prog(&[vop1(1, 1, LIT), vofs]);
    for (k, &x) in vals.iter().enumerate() {
        w.extend_from_slice(&[vop1(1, 10 + k as u32, LIT), x]);
    }
    w.extend_from_slice(&scalar);
    w.extend_from_slice(&op);
    w.push(S_ENDPGM);
    let mut want = data();
    for (k, &x) in vals.iter().enumerate() {
        want[(at / 4) as usize + k] = x;
    }
    CsCase { m: NAMES_STORE[n as usize - 2], op: op.to_vec(), words: w, threads: 1, mem: want.into_iter().enumerate().collect(), init: data() }
}

#[test]
fn buffer_store_dwordx_writes_exactly_its_registers() {
    let mut cs = Vec::new();
    for n in 2..=4u32 {
        for at in [0u32, 8, 20, 48] {
            if at + 4 * n > 64 {
                continue;
            }
            for mode in 0..4u32 {
                if mode == 2 && at < 2 {
                    continue;
                }
                cs.push(store_case(n, at, mode));
            }
        }
    }
    run_cs_cases("mubuf_wide_store", &cs);
}

fn pre_regs(idx: u32, keeps: &[(u32, u32)]) -> Prog {
    let mut p = Prog::new();
    p.vv(1, idx);
    for &(r, x) in keeps {
        p.vv(r, x);
    }
    p
}

fn keep(r: u32) -> u32 {
    0xaaaa_0000 | r
}

struct FCase {
    m: &'static str,
    pre: Vec<u32>,
    op: [u32; 2],
    post: Vec<u32>,
    rt: Vec<u32>,
    mem: Vec<(usize, u32)>,
}

fn run_fcases(group: &str, cs: &[FCase]) {
    let root = tombox_xshader::meta::RootSig { version: 2, flags: 1, params: vec![rtable(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() };
    let mut fails = Vec::new();
    let mut all: std::collections::BTreeSet<&str> = Default::default();
    let mut bad: std::collections::BTreeSet<&str> = Default::default();
    for (i, c, runtime) in cs.iter().enumerate().flat_map(|(i, c)| [(i, c, false), (i, c, true)]) {
        all.insert(c.m);
        let tag = if runtime { " (runtime inputs)" } else { "" };
        let isa: Vec<u8> = c.op.iter().flat_map(|w| w.to_le_bytes()).collect();
        let got = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
        if got != c.m {
            fails.push(format!("#{i} {}: decodes as {got}", c.m));
            bad.insert(c.m);
            continue;
        }
        let mut p = Prog::new();
        p.raw(&if runtime { vop::opaque(&c.pre) } else { c.pre.clone() }).raw(&[0xf408_0004, 0xfa00_0000]).raw(&c.op).raw(&c.post).raw(&[vop1(1, 0, v(4)), vop1(1, 1, v(5))]);
        match run_res_psv(&p.words(), root.clone(), Some(&BUF), None, sampler(0, 3, 0), runtime, &[]) {
            Ok(o) => {
                let wrong_mem: Vec<String> = c.mem.iter().filter(|&&(k, w)| o.buf[k] != w).map(|&(k, w)| format!("[{k}] {:08x} want {w:08x}", o.buf[k])).collect();
                if o.rt[..c.rt.len()] != c.rt[..] || !wrong_mem.is_empty() {
                    fails.push(format!("#{i} {}{tag}: rt {:08x?} want {:08x?}; mem {}", c.m, &o.rt[..c.rt.len()], c.rt, wrong_mem.join(", ")));
                    bad.insert(c.m);
                }
            }
            Err(e) => {
                fails.push(format!("#{i} {}{tag}: {}", c.m, e.lines().take(3).collect::<Vec<_>>().join(" | ")));
                bad.insert(c.m);
            }
        }
    }
    let ok = verified(group, &all, &bad);
    println!("{group}: {} mnemonics verified, {} failing, {} cases", ok.len(), bad.len(), cs.len());
    assert!(fails.is_empty(), "{group}: {} failing\n{}", fails.len(), fails.join("\n\n"));
}

const LOAD_FORMATS: [&str; 4] = ["buffer_load_format_x", "buffer_load_format_xy", "buffer_load_format_xyz", "buffer_load_format_xyzw"];
const STORE_FORMATS: [&str; 4] = ["buffer_store_format_x", "buffer_store_format_xy", "buffer_store_format_xyz", "buffer_store_format_xyzw"];
const UINT_HINT: u32 = 0x25;

fn load_format_cases(uint_view: bool) -> Vec<FCase> {
    let mut cs = Vec::new();
    let idxs: &[u32] = if uint_view { &[0, 1, 2] } else { &[0, 1, 3, 7] };
    for n in 1..=4u32 {
        for &idx in idxs {
            for vd in [2u32, 3, 4] {
                let keeps: Vec<(u32, u32)> = (2..=7).map(|r| (r, keep(r))).collect();
                let comp = |k: u32| -> u32 {
                    if uint_view {
                        BUF.get((4 * idx + k) as usize).copied().unwrap_or(0)
                    } else {
                        match k {
                            0 => BUF[idx as usize],
                            3 => 0x3f80_0000,
                            _ => 0,
                        }
                    }
                };
                let value = |r: u32| if r >= vd && r < vd + n { comp(r - vd) } else { keep(r) };
                let post: Vec<u32> = if uint_view { (vd..vd + n).map(|r| vop2(UINT_HINT, r, ic(0), r)).collect() } else { vec![] };
                cs.push(FCase {
                    m: LOAD_FORMATS[n as usize - 1],
                    pre: pre_regs(idx, &keeps).w,
                    op: mubuf(n - 1, 0, false, true, false, 1, vd),
                    post,
                    rt: vec![value(4), value(5)],
                    mem: vec![],
                });
            }
        }
    }
    cs
}

#[test]
fn buffer_load_format_float_view() {
    run_fcases("mubuf_load_format_float", &load_format_cases(false));
}

#[test]
fn buffer_load_format_uint_view() {
    run_fcases("mubuf_load_format_uint", &load_format_cases(true));
}

#[test]
fn buffer_store_format_xyzw() {
    let vals = [0x1122_3344u32, 0x5566_7788, 0x99aa_bbcc, 0xddee_ff01];
    let mut cs = Vec::new();
    for idx in [0u32, 1, 2] {
        let keeps: Vec<(u32, u32)> = (4..8).map(|r| (r, vals[r as usize - 4])).collect();
        let mut pre = pre_regs(idx, &keeps);
        for r in 4..8 {
            pre.raw(&[vop2(UINT_HINT, r, ic(0), r)]);
        }
        let mut mem: Vec<(usize, u32)> = (0..16).map(|k| (k as usize, BUF.get(k as usize).copied().unwrap_or(0))).collect();
        for k in 0..4usize {
            mem[4 * idx as usize + k].1 = vals[k];
        }
        cs.push(FCase { m: STORE_FORMATS[3], pre: pre.w, op: mubuf(0x07, 0, false, true, false, 1, 4), post: vec![], rt: vec![], mem });
    }
    run_fcases("mubuf_store_format_xyzw", &cs);
}

#[test]
fn buffer_store_format_xyzw_float_view_writes_x_only() {
    let mut cs = Vec::new();
    for idx in [0u32, 2, 5] {
        let keeps = [(4, 0xc0ff_ee00), (5, 0x4000_0000), (6, 0x4040_0000), (7, 0x4080_0000)];
        let mut mem: Vec<(usize, u32)> = (0..8).map(|k| (k as usize, BUF[k as usize])).collect();
        mem[idx as usize].1 = 0xc0ff_ee00;
        cs.push(FCase { m: STORE_FORMATS[3], pre: pre_regs(idx, &keeps).w, op: mubuf(0x07, 0, false, true, false, 1, 4), post: vec![], rt: vec![], mem });
    }
    run_fcases("mubuf_store_format_xyzw_float", &cs);
}

#[test]
fn buffer_atomic_swap_and_umin() {
    type At = fn(u32, u32, u32) -> u32;
    let atoms: [(&'static str, u32, At); 2] = [("buffer_atomic_swap", 0x30, |_, s, _| s), ("buffer_atomic_umin", 0x36, |m, s, _| m.min(s))];
    let mut cs = Vec::new();
    for (m, op, f) in atoms {
        for &(w, s) in &[(4usize, 5u32), (5, 0x10), (5, 0x20), (6, 0x8000_0000), (7, 0xffff_ffff), (7, 0), (2, 0x0302_0100), (0, 0x8382_8180)] {
            let old = BUF[w];
            let p = pre_regs(w as u32 * 4, &[(4, s), (5, 0), (2, 0x1111)]);
            for glc in [true, false] {
                cs.push(FCase { m, pre: p.w.clone(), op: mubuf(op, 0, true, false, glc, 1, 4), post: vec![], rt: vec![if glc { old } else { s }], mem: vec![(w, f(old, s, 0)), (w ^ 1, BUF[w ^ 1])] });
            }
        }
    }
    run_fcases("mubuf_atomic_swap_umin", &cs);
}
