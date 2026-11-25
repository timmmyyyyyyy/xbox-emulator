use super::ds::{cs_prog, ds, dv, ev};
use super::*;
use tombox_xshader::gds_abi::{TB_GDS_HLSL_DECL, TB_GDS_TITLE_DWORDS};

const GDS_BYTES: u64 = 0x1_0000;
const T: u32 = 64;

fn gds_op(op: u32, offset: u32, addr: u32, d0: u32, d1: u32, vdst: u32) -> [u32; 2] {
    let mut w = ds(op, offset, addr, d0, d1, vdst);
    w[0] |= 1 << 17;
    w
}

fn exec_gds(words: &[u32], gds_init: &[u32]) -> Result<(Vec<u32>, Vec<u32>), String> {
    exec_gds_dispatch(words, gds_init, T, 1)
}

fn exec_gds_dispatch(words: &[u32], gds_init: &[u32], threads: u32, groups: u32) -> Result<(Vec<u32>, Vec<u32>), String> {
    let ex = cs_extract(words, threads);
    exec_gds_shader(translate(&ex)?, gds_init, groups)
}

fn exec_gds_shader(t: tombox_xshader::hlsl::Translated, gds_init: &[u32], groups: u32) -> Result<(Vec<u32>, Vec<u32>), String> {
    if !t.hlsl.contains(TB_GDS_HLSL_DECL) {
        return Err(format!("no GDS binding in\n{}", t.hlsl));
    }
    let hlsl = t.hlsl.replace(TB_GDS_HLSL_DECL, "RWByteAddressBuffer tb_gds : register(u1);");
    let code = dxc::compile(&hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{hlsl}"))?;
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { run(&mut c, &hlsl, &code, gds_init, groups) }.map_err(|e| format!("{e}\n{hlsl}"))
}

unsafe fn run(c: &mut Ctx, hlsl: &str, code: &[u8], gds_init: &[u32], groups: u32) -> Result<(Vec<u32>, Vec<u32>), String> {
    let g = &c.gpu;
    let blob = ser_ranges(&[vec![[1, 8, 0, 0, 0xffff_ffff]]])?;
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
        return Err(format!("CreateComputePipelineState hr={:#010x}{}", hr as u32, g.messages()));
    }
    let (vh, vcpu, vgpu, vinc) = g.heap(0, 8, true)?;
    let ubuf = g.buffer(CS_BYTES, 1, d3d::COPY_DEST, true)?;
    let gbuf = g.buffer(GDS_BYTES, 1, d3d::COPY_DEST, true)?;
    let up = g.buffer(CS_BYTES + GDS_BYTES, 2, d3d::GENERIC_READ, false)?;
    let rb = g.buffer(CS_BYTES + GDS_BYTES, 3, d3d::COPY_DEST, false)?;
    let mut bytes = vec![0u8; (CS_BYTES + GDS_BYTES) as usize];
    for (i, w) in gds_init.iter().enumerate() {
        let o = CS_BYTES as usize + 4 * i;
        bytes[o..o + 4].copy_from_slice(&w.to_le_bytes());
    }
    Gpu::map_write(up, &bytes);
    let n: u32 = hlsl.split("struct TbElem_ubuf0 { uint w[").nth(1).and_then(|s| s.split(']').next()).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut ud = [0u8; 40];
    ud[4..8].copy_from_slice(&1u32.to_le_bytes());
    ud[16..20].copy_from_slice(&((CS_BYTES as u32) / (n * 4)).to_le_bytes());
    ud[20..24].copy_from_slice(&(n * 4).to_le_bytes());
    vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), ubuf, core::ptr::null_mut(), ud.as_ptr(), vcpu);
    let mut gd = [0u8; 40];
    gd[0..4].copy_from_slice(&39u32.to_le_bytes());
    gd[4..8].copy_from_slice(&1u32.to_le_bytes());
    gd[16..20].copy_from_slice(&((GDS_BYTES / 4) as u32).to_le_bytes());
    gd[32..36].copy_from_slice(&1u32.to_le_bytes());
    vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), gbuf, core::ptr::null_mut(), gd.as_ptr(), vcpu + vinc as usize);
    c.gpu.begin();
    let l = c.gpu.list;
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), ubuf, 0, up, 0, CS_BYTES);
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), gbuf, 0, up, CS_BYTES, GDS_BYTES);
    c.gpu.barrier(ubuf, d3d::COPY_DEST, 8);
    c.gpu.barrier(gbuf, d3d::COPY_DEST, 8);
    let heaps = [vh];
    vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 1, heaps.as_ptr());
    vcall!(l, 29, fn(*mut c_void) -> (), rs);
    vcall!(l, 31, fn(u32, u64) -> (), 0, vgpu);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    vcall!(l, 14, fn(u32, u32, u32) -> (), groups, 1, 1);
    c.gpu.barrier(ubuf, 8, d3d::COPY_SOURCE);
    c.gpu.barrier(gbuf, 8, d3d::COPY_SOURCE);
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, ubuf, 0, CS_BYTES);
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, CS_BYTES, gbuf, 0, GDS_BYTES);
    let r = c.gpu.submit();
    let out = r.and_then(|_| Gpu::map_read(rb, (CS_BYTES + GDS_BYTES) as usize).ok_or_else(|| "map".to_string())).map(|b| {
        let w: Vec<u32> = b.chunks(4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect();
        let k = (CS_BYTES / 4) as usize;
        (w[..k].to_vec(), w[k..].to_vec())
    });
    for k in [pso, rs, vh, ubuf, gbuf, up, rb] {
        d3d::release(k);
    }
    out
}

fn gi(k: usize) -> u32 {
    (k as u32).wrapping_mul(0x0101_0101) ^ 0x7000_0abc
}

fn init() -> Vec<u32> {
    (0..(GDS_BYTES / 4) as usize).map(gi).collect()
}

const BASE: u32 = 0x100;
const SIZE: u32 = 0x1000;
const OFF: u32 = 8;

fn m0(base: u32, size: u32) -> u32 {
    base << 16 | size
}

fn slot(t: u32) -> usize {
    ((BASE + t * 4 + OFF) / 4) as usize
}

fn prog(m0: u32, body: &[u32], outs: &[u32]) -> Vec<u32> {
    let mut b = vec![sop1(3, 124, LIT), m0];
    b.extend_from_slice(body);
    cs_prog(&b, outs)
}

type F = fn(u32, u32, u32) -> u32;

#[test]
fn ds_gds_atomics() {
    let ops: [(&str, u32, F); 14] = [
        ("ds_add_u32", 0x00, |o, a, _| o.wrapping_add(a)),
        ("ds_sub_u32", 0x01, |o, a, _| o.wrapping_sub(a)),
        ("ds_rsub_u32", 0x02, |o, a, _| a.wrapping_sub(o)),
        ("ds_inc_u32", 0x03, |o, a, _| if o >= a { 0 } else { o + 1 }),
        ("ds_dec_u32", 0x04, |o, a, _| if o == 0 || o > a { a } else { o - 1 }),
        ("ds_min_i32", 0x05, |o, a, _| (o as i32).min(a as i32) as u32),
        ("ds_max_i32", 0x06, |o, a, _| (o as i32).max(a as i32) as u32),
        ("ds_min_u32", 0x07, |o, a, _| o.min(a)),
        ("ds_max_u32", 0x08, |o, a, _| o.max(a)),
        ("ds_and_b32", 0x09, |o, a, _| o & a),
        ("ds_or_b32", 0x0a, |o, a, _| o | a),
        ("ds_xor_b32", 0x0b, |o, a, _| o ^ a),
        ("ds_cmpst_b32", 0x10, |o, a, b| if o == a { b } else { o }),
        ("ds_write_b32", 0x0d, |_, a, _| a),
    ];
    let mut fails = Vec::new();
    let mut ok = Vec::new();
    let g0 = init();
    for (m, op, f) in ops {
        for rtn in [false, true] {
            if rtn && op == 0x0d {
                continue;
            }
            let code = if rtn { op + 0x20 } else { op };
            let words = prog(m0(BASE, SIZE), &gds_op(code, OFF, 1, 2, 4, 10), &[10]);
            let name = if rtn { m.replacen("_u32", "_rtn_u32", 1).replacen("_i32", "_rtn_i32", 1).replacen("_b32", "_rtn_b32", 1) } else { m.to_string() };
            let isa: Vec<u8> = gds_op(code, OFF, 1, 2, 4, 10).iter().flat_map(|w| w.to_le_bytes()).collect();
            let dec = tombox_xshader::rdna2::disassemble(&isa)[0].mnemonic.clone();
            if dec != name {
                fails.push(format!("{name}: decodes as {dec}"));
                continue;
            }
            let mut gs = g0.clone();
            if op == 0x10 {
                for t in (0..T).step_by(2) {
                    gs[slot(t)] = dv(t);
                }
            }
            let rt = super::runtime_words(&words, &gds_op(code, OFF, 1, 2, 4, 10), vop2(0x16, 200, ic(31), 0));
            assert!(rt.is_some(), "{name}: no runtime-input variant");
            let mut good = true;
            for (tag, w) in [("", Some(words.clone())), (" (runtime inputs)", rt)] {
                let Some(w) = w else { continue };
                match exec_gds(&w, &gs) {
                    Ok((mem, gds)) => {
                        let mut bad = Vec::new();
                        for t in 0..T {
                            let s = slot(t);
                            let want = f(gs[s], dv(t), ev(t));
                            if gds[s] != want {
                                bad.push(format!("gds[{s}] {:08x} want {want:08x}", gds[s]));
                            }
                            if rtn && mem[t as usize] != gs[s] {
                                bad.push(format!("rtn[{t}] {:08x} want {:08x}", mem[t as usize], gs[s]));
                            }
                        }
                        if !bad.is_empty() {
                            good = false;
                            fails.push(format!("{name}{tag}: {}", bad.into_iter().take(4).collect::<Vec<_>>().join(", ")));
                        }
                    }
                    Err(e) => {
                        good = false;
                        fails.push(format!("{name}{tag}: {}", e.lines().take(3).collect::<Vec<_>>().join(" | ")));
                    }
                }
            }
            if good {
                ok.push(name);
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n"));
    println!("ds_gds_atomics: {} mnemonics verified", ok.len());
}

#[test]
fn ds_gds_shared_counters() {
    let g0 = init();
    let add = prog(m0(0, SIZE), &gds_op(0x00, 0x40, 13, 2, 0, 0), &[]);
    let add = {
        let mut w = vec![vop1(1, 13, ic(0))];
        w.extend_from_slice(&add);
        w
    };
    let (_, gds) = exec_gds(&add, &g0).unwrap();
    let sum = (0..T).fold(g0[0x10], |a, t| a.wrapping_add(dv(t)));
    assert_eq!(gds[0x10], sum);
    for (op, delta) in [(0x3eu32, 1u32), (0x3d, 0xffff_ffff)] {
        let isa: Vec<u8> = gds_op(op, 0x20, 0, 0, 0, 10).iter().flat_map(|w| w.to_le_bytes()).collect();
        let name = tombox_xshader::rdna2::disassemble(&isa)[0].mnemonic.clone();
        let words = prog(m0(0x200, SIZE), &gds_op(op, 0x20, 0, 0, 0, 10), &[10]);
        let (_, gds) = exec_gds(&words, &g0).unwrap();
        let s = (0x220 / 4) as usize;
        assert_eq!(gds[s], g0[s].wrapping_add(delta.wrapping_mul(T)), "{name}");
    }
    let lim = TB_GDS_TITLE_DWORDS * 4;
    let words = prog(m0(lim - OFF, SIZE), &gds_op(0x2d, OFF, 1, 2, 0, 10), &[10]);
    let (mem, gds) = exec_gds(&words, &g0).unwrap();
    assert!(mem[..T as usize].iter().all(|&v| v == 0));
    assert_eq!(gds, g0);
    let words = prog(m0(BASE, SIZE), &gds_op(0x2d, OFF, 1, 2, 0, 10), &[10]);
    let (mem, gds) = exec_gds(&words, &g0).unwrap();
    for t in 0..T {
        assert_eq!((gds[slot(t)], mem[t as usize]), (dv(t), g0[slot(t)]));
    }
    let words = prog(m0(BASE, OFF + 32 * 4), &gds_op(0x2d, OFF, 1, 2, 0, 10), &[10]);
    let (mem, gds) = exec_gds(&words, &g0).unwrap();
    for t in 0..T {
        let want = if t < 32 { (dv(t), g0[slot(t)]) } else { (g0[slot(t)], 0) };
        assert_eq!((gds[slot(t)], mem[t as usize]), want, "lane {t}");
    }
    let mut rd = gds_op(0x36, OFF, 1, 0, 0, 10).to_vec();
    rd.extend_from_slice(&gds_op(0x0d, OFF + 0x400, 1, 2, 0, 0));
    let (mem, gds) = exec_gds(&prog(m0(BASE, SIZE), &rd, &[10]), &g0).unwrap();
    for t in 0..T {
        assert_eq!(mem[t as usize], g0[slot(t)]);
    }
    for t in 0..T {
        assert_eq!(gds[slot(t) + 256], dv(t));
    }
    for op in [0x1du32, 0x3f] {
        let words = prog(0, &gds_op(op, 0, 1, 2, 0, 10), &[10]);
        assert!(translate(&cs_extract(&words, T)).is_err(), "op {op:#x} must be refused");
    }
}

const SENT: u32 = 0x5eed_0bad;

fn masks() -> Vec<(&'static str, Vec<u32>, fn(u32) -> bool)> {
    vec![
        ("all", vec![], |_| true),
        ("t>20", vec![vopc(0xd1, ic(20), 0)], |t| t > 20),
        ("odd", vec![vop2(0x1b, 20, ic(1), 0), vopc(0xd5, ic(0), 20)], |t| t & 1 == 1),
        ("lane 5 of wave 1", vec![vopc(0xd2, LIT, 0), 37], |t| t == 37),
    ]
}

#[test]
fn ds_gds_append_consume() {
    let g0 = init();
    let s = (0x220 / 4) as usize;
    let hw = std::env::var_os("TOMBOX_EXEC_HW").is_some();
    let mut ok = Vec::new();
    for (op, delta, want_m) in [(0x3eu32, 1u32, "ds_append"), (0x3d, 0xffff_ffff, "ds_consume")] {
        let isa: Vec<u8> = gds_op(op, 0x20, 0, 0, 0, 10).iter().flat_map(|w| w.to_le_bytes()).collect();
        let name = tombox_xshader::rdna2::disassemble(&isa)[0].mnemonic.clone();
        assert_eq!(name, want_m);
        for (label, sel, on) in masks() {
            let mut body = vec![vop1(1, 10, LIT), SENT, sop1(3, 40, 126)];
            body.extend_from_slice(&sel);
            body.extend_from_slice(&gds_op(op, 0x20, 0, 0, 0, 10));
            body.push(sop1(3, 126, 40));
            let words = prog(m0(0x200, SIZE), &body, &[10]);
            let (mem, gds) = exec_gds(&words, &g0).unwrap_or_else(|e| panic!("{name} {label}: {e}"));
            let n: Vec<u32> = (0..T / 32).map(|w| (32 * w..32 * w + 32).filter(|&t| on(t)).count() as u32).collect();
            let total: u32 = n.iter().sum();
            assert_eq!(gds[s], g0[s].wrapping_add(delta.wrapping_mul(total)), "{name} {label}: counter");
            let mut o: Vec<usize> = gds.iter().enumerate().filter(|&(k, &v)| k != s && v != g0[k]).map(|(k, _)| k).collect();
            o.truncate(4);
            assert!(o.is_empty(), "{name} {label}: other GDS dwords changed {o:?}");
            for t in 0..T {
                if !on(t) {
                    assert_eq!(mem[t as usize], SENT, "{name} {label}: inactive lane {t} was written");
                }
            }
            if !hw {
                continue;
            }
            let bases: Vec<Option<u32>> = (0..T / 32).map(|w| (32 * w..32 * w + 32).find(|&t| on(t)).map(|t| mem[t as usize])).collect();
            for w in 0..T / 32 {
                if let Some(b) = bases[w as usize] {
                    for t in (32 * w..32 * w + 32).filter(|&t| on(t)) {
                        assert_eq!(mem[t as usize], b, "{name} {label}: lane {t} of wave {w} differs from the wave's pre-op value");
                    }
                }
            }
            let live: Vec<(u32, u32)> = bases.iter().zip(&n).filter_map(|(b, &k)| b.map(|b| (b, k))).collect();
            let orders = if live.len() == 2 { vec![vec![0usize, 1], vec![1, 0]] } else { vec![(0..live.len()).collect()] };
            let fits = orders.iter().any(|ord| {
                let mut c = g0[s];
                ord.iter().all(|&i| {
                    let good = live[i].0 == c;
                    c = c.wrapping_add(delta.wrapping_mul(live[i].1));
                    good
                })
            });
            assert!(fits, "{name} {label}: wave pre-op values {live:08x?} are not a serial order of per-wave count_bits(exec) updates from {:08x}", g0[s]);
        }
        if hw {
            ok.push(name);
        }
    }
    if hw {
        let verified_names: std::collections::BTreeSet<&str> = ok.iter().map(String::as_str).collect();
        verified("gds_append_consume", &verified_names, &Default::default());
        println!("ds_gds_append_consume: {} mnemonics verified on hardware: {ok:?}", ok.len());
    } else {
        println!("ds_gds_append_consume: WARP waves are narrower than an RDNA2 wave32, so only the counter totals were checked. Set TOMBOX_EXEC_HW=1 to check the per-wave return values on a wave32 device.");
    }
}

#[test]
fn ds_gws_counter_updates_once_per_guest_wave() {
    use tombox_xshader::gds_abi::{TB_GWS_COUNTER_MASK, TB_GWS_TABLE_DWORD};

    let offset = 7;
    let base = 61;
    let index = (TB_GWS_TABLE_DWORD + 4) as usize;
    let initialize = gds_op(0x19, offset, 0, 20, 0, 0);
    let increment = gds_op(0x1a, offset, 0, 0, 0, 0);

    for (threads, threshold, expected) in [(32, 4, Some(7)), (32, 30, Some(33)), (32, 31, None), (64, 32, Some(35))] {
        let mut body = vec![sop1(3, 40, 126), vop2(0x25, 20, LIT, 0), 0x1000];
        body.extend_from_slice(&[vopc(0xd1, ic(threshold), 0)]);
        body.extend_from_slice(&initialize);
        body.extend_from_slice(&increment);
        body.extend_from_slice(&increment);
        body.push(sop1(3, 126, 40));
        let words = prog(base << 16, &body, &[0]);
        let g0 = init();
        let (_, actual) = exec_gds_dispatch(&words, &g0, threads, 1).unwrap();
        let mut want = g0;
        if let Some(count) = expected {
            want[index] = count & TB_GWS_COUNTER_MASK;
        }
        assert_eq!(actual[index], want[index], "threshold {threshold}");
        assert_eq!(actual, want, "another GDS resource was modified");
    }
}

#[test]
fn ds_gws_concurrent_posts_preserve_all_increments() {
    use tombox_xshader::gds_abi::TB_GWS_TABLE_DWORD;

    let mut body = vec![vopc(0xd1, ic(30), 0)];
    body.extend_from_slice(&gds_op(0x1a, 7, 0, 0, 0, 0));
    let words = prog(61 << 16, &body, &[]);
    let index = (TB_GWS_TABLE_DWORD + 4) as usize;
    for (threads, expected) in [(32, 21), (64, 25)] {
        let mut want = init();
        want[index] = 17;
        let (_, actual) = exec_gds_dispatch(&words, &want, threads, 4).unwrap();
        want[index] = expected;
        assert_eq!(actual, want);
    }
}

#[test]
fn ds_gws_bulk_post_uses_first_active_lane_in_each_group() {
    use tombox_xshader::gds_abi::{TB_GWS_COUNTER_MASK, TB_GWS_TABLE_DWORD};

    let mut body = vec![vop2(0x25, 20, LIT, 0), 0x1000, vopc(0xd1, ic(4), 0)];
    body.extend_from_slice(&gds_op(0x1b, 7, 0, 20, 0, 0));
    let words = prog(61 << 16, &body, &[]);
    let index = (TB_GWS_TABLE_DWORD + 4) as usize;
    for (threads, credits_per_group) in [(32, 5), (64, 5 + 32)] {
        let mut want = init();
        want[index] = TB_GWS_COUNTER_MASK - 1;
        let (_, actual) = exec_gds_dispatch(&words, &want, threads, 4).unwrap();
        want[index] += 4 * credits_per_group;
        assert_eq!(actual, want);
        assert_eq!(actual[index] & TB_GWS_COUNTER_MASK, 4 * credits_per_group - 2);
    }
}

#[test]
fn ds_gws_refuses_unsupported_group_and_wait() {
    let init = gds_op(0x19, 7, 0, 20, 0, 0);
    let words = prog(0, &init, &[0]);
    let e = translate(&cs_extract(&words, 1025)).err().unwrap();
    assert!(e.contains("wave32 compute group"), "{e}");

    let wait = gds_op(0x1c, 7, 0, 0, 0, 0);
    let words = prog(0, &wait, &[0]);
    let e = translate(&cs_extract(&words, 32)).err().unwrap();
    assert!(e.contains("global wave sync"), "{e}");
}

#[test]
fn ds_gws_counter_boundaries_across_rows_and_partial_waves() {
    use tombox_xshader::gds_abi::{TB_GWS_COUNTER_MASK, TB_GWS_TABLE_DWORD};
    let idx = (TB_GWS_TABLE_DWORD + 7) as usize;
    for (dims, waves) in [([1, 1, 1], 1), ([17, 3, 1], 2), ([16, 2, 3], 3), ([32, 32, 1], 32)] {
        let post = prog(0, &gds_op(0x1a, 7, 0, 0, 0, 0), &[]);
        let mut ex = cs_extract(&post, dims[0]);
        ex.isa_preamble[1..].copy_from_slice(&dims);
        let mut gds = init();
        gds[idx] = 4096 - waves;
        let (_, actual) = exec_gds_shader(translate(&ex).unwrap(), &gds, 1).unwrap();
        assert_eq!(actual[idx] & TB_GWS_COUNTER_MASK, 0, "{dims:?}");
        gds[idx] = actual[idx];
        assert_eq!(actual, gds);
    }

    for value in [0u32, 4095, 4096, 4097, u32::MAX] {
        let mut setup = vec![vop1(1, 20, LIT), value];
        setup.extend_from_slice(&gds_op(0x19, 7, 0, 20, 0, 0));
        setup.extend_from_slice(&gds_op(0x1b, 7, 0, 20, 0, 0));
        let mut expected = init();
        let (_, actual) = exec_gds_dispatch(&prog(0, &setup, &[]), &expected, 32, 1).unwrap();
        expected[idx] = 2 * (value & 4095);
        assert_eq!(actual, expected, "initialization and bulk post with {value:#x}");
    }
}

#[test]
fn ds_append_and_consume_on_lds_are_refused_by_name() {
    for (op, name) in [(0x3eu32, "ds_append"), (0x3d, "ds_consume")] {
        let words = prog(m0(0x200, SIZE), &ds(op, 0x20, 0, 0, 0, 10), &[10]);
        let e = translate(&cs_extract(&words, T)).err().unwrap_or_else(|| panic!("{name} on LDS translated"));
        assert!(e.contains(name), "{name}: the refusal should name the instruction: {e}");
    }
}
