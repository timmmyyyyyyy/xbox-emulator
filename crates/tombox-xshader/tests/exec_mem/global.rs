use super::*;
use tombox_xshader::global_abi::{fault_of, window_cb, Window, TB_GLOBAL_SPACE};

fn g(op: u32, seg: u32, off: i32, addr: u32, data: u32, saddr: u32, vdst: u32) -> [u32; 2] {
    [0xdc00_0000 | (op << 18) | (seg << 14) | (off as u32 & 0xfff), addr | (data << 8) | (saddr << 16) | (vdst << 24)]
}

const W0: Window = Window { base: 0x10_0000_0000, size: 4096 };
const W1: Window = Window { base: 0x7f10_0000_0000, size: 1024 };
const MISS: u64 = 0x99_0000_0000;

fn m1(i: usize) -> u32 {
    (i as u32).wrapping_mul(0x0101_0101) ^ 0x5a00_8000
}

fn prog() -> Vec<u32> {
    let mut p = Prog::new();
    p.sv(20, W0.base as u32).sv(21, (W0.base >> 32) as u32);
    p.sv(22, W1.base as u32).sv(23, (W1.base >> 32) as u32);
    p.sv(24, MISS as u32).sv(25, (MISS >> 32) as u32);
    p.sv(26, W0.base as u32 + 2048).sv(27, (W0.base >> 32) as u32);
    p.raw(&[vop2(0x1a, 1, ic(2), 0)]);
    p.raw(&[vop2(0x1d, 2, LIT, 0), 0xa5a5_0000]);
    p.raw(&g(0x1c, 2, 0, 1, 2, 20, 0));
    p.raw(&g(0x0c, 2, 0, 1, 0, 22, 3));
    p.raw(&[vop2(0x25, 3, ic(1), 3)]);
    p.raw(&g(0x1c, 2, 256, 1, 3, 20, 0));
    p.raw(&[vop1(1, 4, v(1)), vop1(1, 5, 23)]);
    p.raw(&g(0x0c, 0, 4, 4, 0, 0x7d, 6));
    p.raw(&g(0x1c, 2, 512, 1, 6, 20, 0));
    p.raw(&g(0x18, 2, 1024, 0, 0, 20, 0));
    p.raw(&[vop2(0x1a, 8, ic(1), 0)]);
    p.raw(&g(0x0b, 2, 0, 8, 0, 22, 7));
    p.raw(&g(0x1c, 2, 1280, 1, 7, 20, 0));
    p.raw(&g(0x0c, 2, 2, 1, 0, 22, 9));
    p.raw(&g(0x1c, 2, 1536, 1, 9, 20, 0));
    p.raw(&g(0x1a, 2, 1792, 8, 2, 20, 0));
    p.raw(&[vop2(0x1a, 10, ic(3), 0)]);
    p.raw(&g(0x1d, 2, 0, 10, 2, 26, 0));
    p.raw(&g(0x1c, 2, 0, 1, 2, 24, 0));
    let mut w = p.w;
    w.push(S_ENDPGM);
    w
}

unsafe fn run(words: &[u32]) -> Result<(Vec<u32>, Vec<u32>, [u32; 4], String), String> {
    let ex = cs_extract(words, 64);
    let t = translate(&ex)?;
    if !t.needs_global {
        return Err(format!("needs_global not set\n{}", t.hlsl));
    }
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(ctx_new()));
    let c = m.lock().unwrap_or_else(|e| e.into_inner());
    let g = &c.gpu;
    let blob = ser_ranges(&[vec![[2, 1, 0, TB_GLOBAL_SPACE, 0xffff_ffff], [1, 3, 0, TB_GLOBAL_SPACE, 0xffff_ffff]]])?;
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, blob.as_ptr(), blob.len(), &d3d::IID_ROOTSIG, &mut rs);
    if hr < 0 {
        return Err(format!("CreateRootSignature {hr:#x}"));
    }
    let mut d = [0u8; 56];
    d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
    d[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    d[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 11, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        d3d::release(rs);
        return Err(format!("CreateComputePipelineState hr={:#010x}{}\n{}", hr as u32, g.messages(), t.hlsl));
    }
    let (vh, vcpu, vgpu, vinc) = g.heap(0, 4, true)?;
    let sizes = [W0.size as u64, W1.size as u64, 256u64];
    let init: [Vec<u32>; 3] = [vec![0; W0.size as usize / 4], (0..W1.size as usize / 4).map(m1).collect(), vec![0; 64]];
    let mut kill = vec![pso, rs, vh];
    let mut bufs = Vec::new();
    for k in 0..3 {
        let b = g.buffer(sizes[k], 1, d3d::COPY_DEST, true)?;
        let up = g.buffer(sizes[k], 2, d3d::GENERIC_READ, false)?;
        let rb = g.buffer(sizes[k], 3, d3d::COPY_DEST, false)?;
        let mut bytes: Vec<u8> = init[k].iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.resize(sizes[k] as usize, 0);
        Gpu::map_write(up, &bytes);
        let mut ud = [0u8; 40];
        ud[0..4].copy_from_slice(&39u32.to_le_bytes());
        ud[4..8].copy_from_slice(&1u32.to_le_bytes());
        ud[16..20].copy_from_slice(&((sizes[k] / 4) as u32).to_le_bytes());
        ud[32..36].copy_from_slice(&1u32.to_le_bytes());
        vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), b, core::ptr::null_mut(), ud.as_ptr(), vcpu + (1 + k) * vinc);
        kill.extend([b, up, rb]);
        bufs.push((b, up, rb));
    }
    let cb = g.buffer(256, 2, d3d::GENERIC_READ, false)?;
    kill.push(cb);
    let cbw = window_cb(&[W0, W1], [0, 0]);
    let mut cbd: Vec<u8> = cbw.iter().flat_map(|w| w.to_le_bytes()).collect();
    cbd.resize(256, 0);
    Gpu::map_write(cb, &cbd);
    let cv = [Gpu::gpu_va(cb), 256u64];
    vcall!(g.device, 17, fn(*const u64, usize) -> (), cv.as_ptr(), vcpu);
    drop(c);
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    c.gpu.begin();
    let l = c.gpu.list;
    for (k, &(b, up, _)) in bufs.iter().enumerate() {
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), b, 0, up, 0, sizes[k]);
        c.gpu.barrier(b, d3d::COPY_DEST, 8);
    }
    let heaps = [vh];
    vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 1, heaps.as_ptr());
    vcall!(l, 29, fn(*mut c_void) -> (), rs);
    vcall!(l, 31, fn(u32, u64) -> (), 0, vgpu);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    vcall!(l, 14, fn(u32, u32, u32) -> (), 1, 1, 1);
    for (k, &(b, _, rb)) in bufs.iter().enumerate() {
        c.gpu.barrier(b, 8, d3d::COPY_SOURCE);
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, b, 0, sizes[k]);
    }
    let r = c.gpu.submit();
    let read = |k: usize| -> Vec<u32> { Gpu::map_read(bufs[k].2, sizes[k] as usize).unwrap_or_default().chunks(4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect() };
    let out = r.map(|_| {
        let f = read(2);
        (read(0), read(1), [f[0], f[1], f[2], f[3]], t.hlsl.clone())
    });
    for k in kill {
        d3d::release(k);
    }
    out
}

#[test]
fn global_windows_load_store_and_fault() {
    let words = prog();
    let (w0, w1, fault, hlsl) = unsafe { run(&words) }.unwrap_or_else(|e| panic!("{e}"));
    let mut bad = Vec::new();
    let mut chk = |what: &str, k: usize, got: u32, want: u32| {
        if got != want {
            bad.push(format!("{what} [{k}] {got:08x} want {want:08x}"));
        }
    };
    for t in 0..64usize {
        let tv = t as u32 ^ 0xa5a5_0000;
        chk("store_dword", t, w0[t], tv);
        chk("load_dword+1", t, w0[64 + t], m1(t).wrapping_add(1));
        chk("flat_load_dword", t, w0[128 + t], m1(t + 1));
        let h = if t % 2 == 0 { m1(t / 2) & 0xffff } else { m1(t / 2) >> 16 };
        chk("load_sshort", t, w0[320 + t], h as u16 as i16 as i32 as u32);
        chk("load_dword unaligned", t, w0[384 + t], (m1(t) >> 16) | (m1(t + 1) << 16));
        chk("store_dwordx2 lo", t, w0[512 + 2 * t], tv);
        chk("store_dwordx2 hi", t, w0[513 + 2 * t], m1(t).wrapping_add(1));
    }
    for k in 0..16usize {
        let b = (4 * k as u32) | (4 * k as u32 + 1) << 8 | (4 * k as u32 + 2) << 16 | (4 * k as u32 + 3) << 24;
        chk("store_byte", k, w0[256 + k], b);
    }
    for k in 0..32usize {
        chk("store_short", k, w0[448 + k], (2 * k as u32) | (2 * k as u32 + 1) << 16);
    }
    for (k, &x) in w1.iter().enumerate() {
        chk("window1 untouched", k, x, m1(k));
    }
    let f = fault_of(&fault);
    chk("fault count", 0, f.count, 64);
    if !(MISS..MISS + 256).contains(&f.first_va) || f.first_va % 4 != 0 {
        bad.push(format!("fault first_va {:#x}", f.first_va));
    }
    assert!(bad.is_empty(), "{}\n{hlsl}", bad.join("\n"));
}

#[test]
fn global_routing_and_refusals() {
    let isa = |w: &[u32]| -> Vec<u8> { w.iter().flat_map(|x| x.to_le_bytes()).collect() };
    for (op, m) in [(0x08u32, "flat_load_ubyte"), (0x0b, "flat_load_sshort"), (0x20, "flat_load_ubyte_d16"), (0x21, "flat_load_ubyte_d16_hi"), (0x22, "flat_load_sbyte_d16"), (0x23, "flat_load_sbyte_d16_hi"), (0x24, "flat_load_short_d16"), (0x25, "flat_load_short_d16_hi"), (0x18, "flat_store_byte"), (0x19, "flat_store_byte_d16_hi"), (0x1a, "flat_store_short"), (0x1b, "flat_store_short_d16_hi"), (0x1c, "flat_store_dword"), (0x1d, "flat_store_dwordx2"), (0x1e, "flat_store_dwordx4"), (0x1f, "flat_store_dwordx3")] {
        let got = tombox_xshader::rdna2::disassemble(&isa(&g(op, 2, 0, 1, 2, 20, 3))).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
        assert_eq!(got, m);
    }
    let mut p = Prog::new();
    p.raw(&g(0x30, 1, 0, 1, 2, 0x7d, 0));
    let mut w = p.w.clone();
    w.push(S_ENDPGM);
    let e = translate(&cs_extract(&w, 64)).err().unwrap_or_default();
    assert!(e.contains("scratch"), "{e}");
    let mut p = Prog::new();
    p.raw(&g(0x20, 1, 0, 1, 0, 0x7d, 24));
    let mut w = p.w.clone();
    w.push(S_ENDPGM);
    let e = translate(&cs_extract(&w, 64)).err().unwrap_or_default();
    assert!(e.contains("unsupported"), "{e}");
    let mut p = Prog::new();
    p.raw(&g(0x30, 2, 0, 1, 2, 20, 3));
    let mut w = p.w.clone();
    w.push(S_ENDPGM);
    let e = translate(&cs_extract(&w, 64)).err().unwrap_or_default();
    assert!(e.contains("unsupported"), "{e}");
    let mut p = Prog::new();
    let mut lds = g(0x0c, 2, 0, 1, 0, 20, 3);
    lds[0] |= 1 << 13;
    p.raw(&lds);
    let mut w = p.w.clone();
    w.push(S_ENDPGM);
    let e = translate(&cs_extract(&w, 64)).err().unwrap_or_default();
    assert!(e.contains("LDS=1"), "{e}");
}

struct GCase {
    m: &'static str,
    op: Vec<u32>,
    words: Vec<u32>,
    w0: Vec<(usize, u32)>,
    w1: Vec<u32>,
}

fn run_global_cases(group: &str, cs: &[GCase]) {
    let mut fails = Vec::new();
    let mut all: std::collections::BTreeSet<&str> = Default::default();
    let mut bad: std::collections::BTreeSet<&str> = Default::default();
    let mut runtime = 0;
    for (i, c, rtw) in with_runtime(cs, |c| runtime_words(&c.words, &c.op, vop2(0x16, 200, ic(31), 0))) {
        all.insert(c.m);
        let isa: Vec<u8> = c.op.iter().flat_map(|w| w.to_le_bytes()).collect();
        let got_m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
        if got_m != c.m {
            fails.push(format!("#{i} {}: decodes as {got_m}", c.m));
            bad.insert(c.m);
            continue;
        }
        let tag = if rtw.is_some() { runtime += 1; " (runtime inputs)" } else { "" };
        match unsafe { run(rtw.as_deref().unwrap_or(&c.words)) } {
            Ok((w0, w1, fault, hlsl)) => {
                let mut wrong: Vec<String> = c.w0.iter().filter(|&&(k, w)| w0[k] != w).map(|&(k, w)| format!("w0[{k}] {:08x} want {w:08x}", w0[k])).collect();
                wrong.extend(w1.iter().zip(&c.w1).enumerate().filter(|(_, (g, w))| g != w).map(|(k, (g, w))| format!("w1[{k}] {g:08x} want {w:08x}")));
                let f = fault_of(&fault);
                if f.count != 0 {
                    wrong.push(format!("{} faults, first va {:#x}", f.count, f.first_va));
                }
                if !wrong.is_empty() {
                    let body: String = hlsl.lines().filter(|l| l.starts_with("    ") && !l.contains("// 0x")).collect::<Vec<_>>().join("\n");
                    fails.push(format!("#{i} {}{tag}: {}\n{body}", c.m, wrong.into_iter().take(6).collect::<Vec<_>>().join(", ")));
                    bad.insert(c.m);
                }
            }
            Err(e) => {
                fails.push(format!("#{i} {}{tag}: {}", c.m, e.lines().take(4).collect::<Vec<_>>().join(" | ")));
                bad.insert(c.m);
            }
        }
    }
    let ok = verified(group, &all, &bad);
    println!("{group}: {} mnemonics verified, {} failing, {} cases, {runtime} rerun with runtime inputs", ok.len(), bad.len(), cs.len());
    assert!(fails.is_empty(), "{group}: {} failing\n{}", fails.len(), fails.join("\n\n"));
}

const KD: [u32; 4] = [0x8080_8080, 0x1234_5678, 0xdead_beef, 0x0bad_f00d];

fn dval(t: u32, k: u32) -> u32 {
    t.wrapping_mul(0x2f5b3) ^ KD[k as usize]
}

const MODES: [(u32, u32, i32, i32); 5] = [(2, 22, -4, -4), (2, 22, 8, 8), (2, 0x7d, -16, -16), (0, 0x7d, 0, 0), (0, 0x7d, 0x80c, 12)];

fn gprog(mul: u32, add: u32, eff: i32, body: &[u32]) -> Vec<u32> {
    let mut p = Prog::new();
    p.sv(20, W0.base as u32).sv(21, (W0.base >> 32) as u32);
    p.sv(22, W1.base as u32).sv(23, (W1.base >> 32) as u32);
    p.raw(&[vop2(0x1a, 1, ic(2), 0)]);
    for k in 0..4 {
        p.raw(&[vop2(0x0b, 10 + k, LIT, 0), 0x2f5b3, vop2(0x1d, 10 + k, LIT, 10 + k), KD[k as usize]]);
    }
    p.raw(&[vop2(0x0b, 20, LIT, 0), mul, vop2(0x25, 20, LIT, 20), add.wrapping_sub(eff as u32)]);
    p.vv(21, (W1.base >> 32) as u32);
    p.raw(body);
    let mut w = p.w;
    w.push(S_ENDPGM);
    w
}

#[test]
fn flat_global_mem() {
    assert_eq!(W1.base as u32, 0);
    let w1_init: Vec<u8> = (0..W1.size as usize / 4).flat_map(|i| m1(i).to_le_bytes()).collect();
    let le = |b: &[u8], a: usize| u32::from_le_bytes(b[a..a + 4].try_into().unwrap());
    let words_of = |b: &[u8]| -> Vec<u32> { (0..b.len() / 4).map(|i| le(b, 4 * i)).collect() };
    let mut cs = Vec::new();
    type Ext = fn(u32) -> u32;
    let loads: [(&'static str, u32, u32, u32, u32, Ext); 8] = [
        ("flat_load_ubyte", 0x08, 1, 3, 17, |x| x & 0xff),
        ("flat_load_sbyte", 0x09, 1, 3, 18, |x| x as u8 as i8 as i32 as u32),
        ("flat_load_ushort", 0x0a, 1, 2, 16, |x| x & 0xffff),
        ("flat_load_sshort", 0x0b, 1, 2, 18, |x| x as u16 as i16 as i32 as u32),
        ("flat_load_dword", 0x0c, 1, 4, 16, |x| x),
        ("flat_load_dwordx2", 0x0d, 2, 8, 16, |x| x),
        ("flat_load_dwordx3", 0x0f, 3, 12, 16, |x| x),
        ("flat_load_dwordx4", 0x0e, 4, 12, 16, |x| x),
    ];
    for (m, opc, n, mul, add, ext) in loads {
        for (seg, saddr, enc, eff) in MODES {
            let op = g(opc, seg, enc, 20, 0, saddr, 24);
            let mut body = op.to_vec();
            for k in 0..n {
                body.extend_from_slice(&g(0x1c, 2, 256 * k as i32, 1, 24 + k, 20, 0));
            }
            let mut w0 = Vec::new();
            for t in 0..64u32 {
                let a = (mul * t + add) as usize;
                for k in 0..n {
                    w0.push(((k * 64 + t) as usize, ext(le(&w1_init, a + 4 * k as usize))));
                }
            }
            cs.push(GCase { m, op: op.to_vec(), words: gprog(mul, add, eff, &body), w0, w1: words_of(&w1_init) });
        }
    }
    let stores: [(&'static str, u32, u32, u32, u32, u32, u32); 8] = [
        ("flat_store_byte", 0x18, 1, 1, 0, 3, 17),
        ("flat_store_byte_d16_hi", 0x19, 1, 1, 16, 3, 17),
        ("flat_store_short", 0x1a, 1, 2, 0, 6, 16),
        ("flat_store_short_d16_hi", 0x1b, 1, 2, 16, 6, 16),
        ("flat_store_dword", 0x1c, 1, 4, 0, 4, 16),
        ("flat_store_dwordx2", 0x1d, 2, 4, 0, 8, 16),
        ("flat_store_dwordx3", 0x1f, 3, 4, 0, 12, 16),
        ("flat_store_dwordx4", 0x1e, 4, 4, 0, 16, 0),
    ];
    for (m, opc, n, bytes, sh, mul, add) in stores {
        for (seg, saddr, enc, eff) in MODES {
            if (add as i32) < eff {
                continue;
            }
            let op = g(opc, seg, enc, 20, 10, saddr, 0);
            let mut b = w1_init.clone();
            for t in 0..64u32 {
                let a = (mul * t + add) as usize;
                for k in 0..n {
                    let v = dval(t, k) >> sh;
                    for i in 0..bytes as usize {
                        b[a + 4 * k as usize + i] = (v >> (8 * i)) as u8;
                    }
                }
            }
            cs.push(GCase { m, op: op.to_vec(), words: gprog(mul, add, eff, &op), w0: vec![], w1: words_of(&b) });
        }
    }
    run_global_cases("flat_global", &cs);
}

#[test]
fn flat_global_d16_loads() {
    let w1_init: Vec<u8> = (0..W1.size as usize / 4).flat_map(|i| m1(i).to_le_bytes()).collect();
    let le = |b: &[u8], a: usize| u32::from_le_bytes(b[a..a + 4].try_into().unwrap());
    let words_of = |b: &[u8]| -> Vec<u32> { (0..b.len() / 4).map(|i| le(b, 4 * i)).collect() };
    let keep = 0x5a5a_a5a5u32;
    let lo = |v: u32| (keep & 0xffff_0000) | (v & 0xffff);
    let hi = |v: u32| (keep & 0xffff) | (v << 16);
    let mut cs = Vec::new();
    type Ext = fn(u32) -> u32;
    let d16: [(&'static str, u32, u32, u32, bool, Ext); 6] = [
        ("flat_load_ubyte_d16", 0x20, 1, 17, false, |x| x & 0xff),
        ("flat_load_ubyte_d16_hi", 0x21, 1, 18, true, |x| x & 0xff),
        ("flat_load_sbyte_d16", 0x22, 1, 19, false, |x| x as u8 as i8 as i32 as u32),
        ("flat_load_sbyte_d16_hi", 0x23, 1, 20, true, |x| x as u8 as i8 as i32 as u32),
        ("flat_load_short_d16", 0x24, 2, 16, false, |x| x & 0xffff),
        ("flat_load_short_d16_hi", 0x25, 2, 18, true, |x| x & 0xffff),
    ];
    for (m, opc, mul, add, hi_b, ext) in d16 {
        for (seg, saddr, enc, eff) in MODES {
            let op = g(opc, seg, enc, 20, 0, saddr, 24);
            let mut body = vec![vop1(1, 24, LIT), keep];
            body.extend_from_slice(&op);
            body.extend_from_slice(&g(0x1c, 2, 0, 1, 24, 20, 0));
            let mut w0 = Vec::new();
            for t in 0..64u32 {
                let a = (mul * t + add) as usize;
                let v = ext(le(&w1_init, a));
                w0.push((t as usize, if hi_b { hi(v) } else { lo(v) }));
            }
            cs.push(GCase { m, op: op.to_vec(), words: gprog(mul, add, eff, &body), w0, w1: words_of(&w1_init) });
        }
    }
    run_global_cases("flat_global_d16", &cs);
}

#[test]
fn narrow_loads_at_window_end() {
    let last = m1(W1.size as usize / 4 - 1);
    let mut body = Vec::new();
    for (op, offset, dst) in [(0x08, 3, 24), (0x09, 3, 25), (0x0a, 2, 26), (0x0b, 2, 27)] {
        body.extend(g(op, 2, offset, 20, 0, 22, dst));
        body.extend(g(0x1c, 2, 256 * (dst as i32 - 24), 1, dst, 20, 0));
    }
    let words = gprog(0, W1.size - 4, 0, &body);
    let (w0, _, fault, _) = unsafe { run(&words) }.unwrap();
    let byte = last >> 24;
    let short = last >> 16;
    let expected = [byte, byte as i8 as i32 as u32, short, short as i16 as i32 as u32];
    for (lane_values, value) in w0.chunks_exact(64).zip(expected) {
        assert!(lane_values.iter().all(|&actual| actual == value));
    }
    assert_eq!(fault_of(&fault).count, 0);
}
