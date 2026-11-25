use super::super::*;

pub const LANE: u32 = 250;
const ADDR: u32 = 251;
pub const WAVE: usize = 32;

pub fn lane_root() -> tombox_xshader::meta::RootSig {
    tombox_xshader::meta::RootSig { version: 2, flags: 1, params: vec![rtable(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() }
}

pub fn lane_shader(words: &[u32]) -> ShaderExtract {
    let mut ex = shader_root(words, lane_root());
    ex.stage = Stage::Compute;
    ex.isa_preamble = [0, WAVE as u32, 1, 1];
    ex.output_sig = vec![];
    ex.has_output_sig = false;
    ex.has_input_sig = false;
    ex
}

pub fn lane_words(body: &[u32]) -> Vec<u32> {
    let mut w = vec![vop1(1, LANE, v(0))];
    w.extend_from_slice(body);
    w.extend_from_slice(&[sop1(3, 126, ic(-1)), 0xf408_0002, 0xfa00_0000, vop2(0x1a, ADDR, ic(4), LANE)]);
    w.extend_from_slice(&mubuf(0x1e, 0, true, false, false, ADDR, 0));
    w.push(S_ENDPGM);
    w
}

struct HwGpu(Gpu);
unsafe impl Send for HwGpu {}

static HW: OnceLock<Mutex<Option<HwGpu>>> = OnceLock::new();

pub enum Dev {
    Warp,
    Hw,
}

unsafe fn with_gpu<R>(dev: &Dev, f: &mut dyn FnMut(&mut Gpu) -> R) -> Result<R, String> {
    match dev {
        Dev::Warp => {
            let m = CTX.get_or_init(|| Mutex::new(ctx_new()));
            let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
            Ok(f(&mut c.gpu))
        }
        Dev::Hw => {
            let m = HW.get_or_init(|| {
                Mutex::new(Gpu::new(false).ok().map(|mut g| {
                    g.open = false;
                    HwGpu(g)
                }))
            });
            let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
            match c.as_mut() {
                Some(g) => Ok(f(&mut g.0)),
                None => Err("no hardware D3D12 device".into()),
            }
        }
    }
}

unsafe fn dispatch(g: &mut Gpu, code: &[u8]) -> Result<Vec<[u32; 4]>, String> {
    let blob = ser_root(&[(1, 8)], &[])?;
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
    let n = (WAVE * 16) as u64;
    let mut kill = vec![rs, pso];
    let res = (|| -> Result<Vec<[u32; 4]>, String> {
        let ub = g.buffer(n, 1, 8, true)?;
        kill.push(ub);
        let rb = g.buffer(n, 3, d3d::COPY_DEST, false)?;
        kill.push(rb);
        let (vh, vcpu, vgpu, vinc) = g.heap(0, 8, true)?;
        kill.push(vh);
        for slot in 0..8usize {
            let mut ud = [0u8; 40];
            ud[4..8].copy_from_slice(&1u32.to_le_bytes());
            ud[16..20].copy_from_slice(&(WAVE as u32).to_le_bytes());
            ud[20..24].copy_from_slice(&16u32.to_le_bytes());
            vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), ub, core::ptr::null_mut(), ud.as_ptr(), vcpu + slot * vinc);
        }
        g.begin();
        let l = g.list;
        let heaps = [vh];
        vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 1, heaps.as_ptr());
        vcall!(l, 29, fn(*mut c_void) -> (), rs);
        vcall!(l, 25, fn(*mut c_void) -> (), pso);
        vcall!(l, 31, fn(u32, u64) -> (), 0, vgpu);
        vcall!(l, 14, fn(u32, u32, u32) -> (), 1, 1, 1);
        g.barrier(ub, 8, d3d::COPY_SOURCE);
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, ub, 0, n);
        g.submit()?;
        let b = Gpu::map_read(rb, n as usize).ok_or("map readback")?;
        Ok(b.chunks(16).map(|c| core::array::from_fn(|k| u32::from_le_bytes(c[k * 4..k * 4 + 4].try_into().unwrap()))).collect())
    })();
    for k in kill {
        d3d::release(k);
    }
    res
}

pub fn wave_width(dev: &Dev) -> Result<u32, String> {
    let src = "struct E { uint w[4]; }; RWStructuredBuffer<E> o : register(u0); [numthreads(32, 1, 1)] void main(uint3 t : SV_GroupThreadID) { E e; e.w[0] = WaveGetLaneCount(); e.w[1] = WaveGetLaneIndex(); e.w[2] = 0u; e.w[3] = 0u; o[t.x] = e; }";
    let code = dxc::compile(src, "cs_6_0")?;
    let out = unsafe { with_gpu(dev, &mut |g| dispatch(g, &code)) }??;
    let lanes_match = out.iter().enumerate().all(|(i, r)| r[1] as usize == i % r[0].max(1) as usize);
    if !lanes_match {
        return Err(format!("lane index is not thread index modulo wave width: {:?}", out.iter().map(|r| (r[0], r[1])).collect::<Vec<_>>()));
    }
    Ok(out[0][0])
}

pub fn run_lanes(dev: &Dev, body: &[u32]) -> Result<Vec<[u32; 4]>, String> {
    let w = lane_words(body);
    let t = translate(&lane_shader(&w))?;
    let code = dxc::compile(&t.hlsl, "cs_6_0").map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    unsafe { with_gpu(dev, &mut |g| dispatch(g, &code)) }?.map_err(|e| format!("{e}\n{}", t.hlsl))
}

pub struct LaneCase {
    pub m: &'static str,
    pub width: u32,
    pub body: Vec<u32>,
    pub at: usize,
    pub exp: Vec<[u32; 4]>,
}

pub fn run_lane_cases(group: &str, dev: Dev, cs: &[LaneCase]) {
    let width = wave_width(&dev).unwrap_or_else(|e| panic!("{group}: {e}"));
    let mut fails = Vec::new();
    let mut all: std::collections::BTreeSet<&str> = Default::default();
    let mut bad: std::collections::BTreeSet<&str> = Default::default();
    let mut ran = 0;
    for (i, c) in cs.iter().enumerate() {
        if c.width > width {
            continue;
        }
        ran += 1;
        all.insert(c.m);
        let isa: Vec<u8> = c.body[c.at..].iter().flat_map(|w| w.to_le_bytes()).collect();
        let got_m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
        if got_m != c.m {
            fails.push(format!("#{i} {}: decodes as {got_m}", c.m));
            bad.insert(c.m);
            continue;
        }
        match run_lanes(&dev, &c.body) {
            Ok(r) => {
                let n = c.exp.len();
                if r[..n] != c.exp[..] {
                    let diff: Vec<String> = (0..n).filter(|&l| r[l] != c.exp[l]).take(6).map(|l| format!("lane {l}: got {:08x?} want {:08x?}", r[l], c.exp[l])).collect();
                    let h = translate(&lane_shader(&lane_words(&c.body))).map(|t| t.hlsl).unwrap_or_default();
                    let body: String = h.lines().filter(|l| l.starts_with("    ")).collect::<Vec<_>>().join("\n");
                    fails.push(format!("#{i} {}:\n{}\n{body}", c.m, diff.join("\n")));
                    bad.insert(c.m);
                }
            }
            Err(e) => {
                fails.push(format!("#{i} {}: {}", c.m, e.lines().take(4).collect::<Vec<_>>().join(" | ")));
                bad.insert(c.m);
            }
        }
    }
    let ok: Vec<&str> = all.iter().filter(|m| !bad.contains(*m)).copied().collect();
    if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{group}_w{width}.txt")), ok.join("\n") + "\n");
    }
    println!("{group}: wave width {width}, {} mnemonics verified, {} failing, {ran} of {} cases run", ok.len(), bad.len(), cs.len());
    assert!(ran > 0, "{group}: no case fits a wave of {width} lanes");
    assert!(fails.is_empty(), "{group}: {} failing\n{}", fails.len(), fails.join("\n\n"));
}

pub fn hw_requested() -> bool {
    std::env::var_os("TOMBOX_EXEC_HW").is_some()
}

pub fn seed(r: u32, mul: u32, add: u32) -> Vec<u32> {
    vec![vop2(0x0b, r, LIT, LANE), mul, vop2(0x25, r, LIT, r), add]
}

pub fn lane_val(l: usize, mul: u32, add: u32) -> u32 {
    (l as u32 & 0xff_ffff).wrapping_mul(mul & 0xff_ffff).wrapping_add(add)
}

pub fn set_exec(mask: u32) -> Vec<u32> {
    let w = vop3(0x116, 6, [v(LANE), LIT, 0]);
    vec![w[0], w[1], mask, vop2(0x1b, 6, ic(1), 6), vopc(0xd5, ic(0), 6)]
}

pub fn all_exec() -> Vec<u32> {
    vec![sop1(3, 126, ic(-1))]
}

pub fn lanes_of(f: impl Fn(usize) -> [u32; 4], width: u32) -> Vec<[u32; 4]> {
    (0..width as usize).map(f).collect()
}

const MA: u32 = 0x0001_0101;
const AA: u32 = 0xab00_0000;
const MO: u32 = 0x0000_0011;
const AO: u32 = 0x7700_0000;

fn readlane_cases(cs: &mut Vec<LaneCase>, width: u32) {
    let ks: Vec<u32> = if width >= 32 { vec![0, 1, 5, 16, 31] } else { (0..width).collect() };
    for &k in &ks {
        let mut b = seed(5, MA, AA);
        let at = b.len();
        b.extend_from_slice(&vop3(0x360, 10, [v(5), ic(k as i32), 0]));
        b.extend_from_slice(&[vop1(1, 0, 10), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_readlane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [lane_val(k as usize, MA, AA), lane_val(l, MA, AA), l as u32, 0], k + 1) });
        let mut b = seed(5, MA, AA);
        b.extend_from_slice(&[sop1(3, 11, LIT), k + 32]);
        b.extend_from_slice(&set_exec(!(1u32 << k)));
        let at = b.len();
        b.extend_from_slice(&vop3(0x360, 10, [v(5), 11, 0]));
        b.extend_from_slice(&all_exec());
        b.extend_from_slice(&[vop1(1, 0, 10), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_readlane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [lane_val(k as usize, MA, AA), lane_val(l, MA, AA), l as u32, 0], k + 1) });
        let mut b = seed(7, MO, AO);
        b.extend_from_slice(&[sop1(3, 12, LIT), 0x1234_5678]);
        b.extend_from_slice(&set_exec(!(1u32 << k)));
        let at = b.len();
        b.extend_from_slice(&vop3(0x361, 7, [12, ic(k as i32), 0]));
        b.extend_from_slice(&all_exec());
        b.extend_from_slice(&[vop1(1, 0, v(7)), vop1(1, 1, v(LANE)), vop1(1, 2, ic(0)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_writelane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [if l == k as usize { 0x1234_5678 } else { lane_val(l, MO, AO) }, l as u32, 0, 0], k + 1) });
    }
}

#[test]
fn readlane_writelane_lanes() {
    let mut cs = Vec::new();
    readlane_cases(&mut cs, 4);
    run_lane_cases("lanes_rw_warp", Dev::Warp, &cs);
    if hw_requested() {
        let mut cs = Vec::new();
        readlane_cases(&mut cs, 32);
        run_lane_cases("lanes_rw_hw", Dev::Hw, &cs);
    }
}

fn readlane_special_cases(cs: &mut Vec<LaneCase>, width: u32) {
    let ks: Vec<u32> = if width >= 32 { vec![0, 5, 31] } else { vec![0, 3] };
    for &k in &ks {
        let mut b = seed(5, MA, AA);
        b.extend_from_slice(&[sop1(3, 124, LIT), 0x1357_2468]);
        let at = b.len();
        b.extend_from_slice(&vop3(0x360, 124, [v(5), ic(k as i32), 0]));
        b.extend_from_slice(&[vop1(1, 0, 124), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_readlane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [lane_val(k as usize, MA, AA), lane_val(l, MA, AA), l as u32, 0], k + 1) });

        let mut b = seed(5, MA, AA);
        b.extend_from_slice(&[sop1(3, 11, LIT), k, sop1(3, 124, LIT), 0x1357_2468]);
        b.extend_from_slice(&set_exec(!(1u32 << k)));
        let at = b.len();
        b.extend_from_slice(&vop3(0x360, 124, [v(5), 11, 0]));
        b.extend_from_slice(&all_exec());
        b.extend_from_slice(&[vop1(1, 0, 124), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_readlane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [lane_val(k as usize, MA, AA), lane_val(l, MA, AA), l as u32, 0], k + 1) });

        let mut b = seed(5, MA, AA);
        b.extend_from_slice(&[sop1(3, 124, LIT), 0x1357_2468]);
        let at = b.len();
        b.extend_from_slice(&vop3(0x360, 125, [v(5), ic(k as i32), 0]));
        b.extend_from_slice(&[vop1(1, 0, 124), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_readlane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [0x1357_2468, lane_val(l, MA, AA), l as u32, 0], k + 1) });

        for dst in [107u32, 127] {
            let mut b = seed(5, MA, AA);
            let at = b.len();
            b.extend_from_slice(&vop3(0x360, dst, [v(5), ic(k as i32), 0]));
            b.extend_from_slice(&[vop1(1, 0, dst), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
            cs.push(LaneCase { m: "v_readlane_b32", width: k + 1, body: b, at, exp: lanes_of(|l| [lane_val(k as usize, MA, AA), lane_val(l, MA, AA), l as u32, 0], k + 1) });
        }
    }

    for (dst, read_dst, want) in [(124u32, 124u32, 0xdead_beefu32), (125, 124, 0x1357_2468), (107, 107, 0xdead_beef), (127, 127, 0xdead_beef)] {
        let mut b = vec![sop1(3, 10, LIT), 0xdead_beef, sop1(3, 124, LIT), 0x1357_2468];
        b.extend_from_slice(&vop3(0x361, 9, [10, ic(0), 0]));
        let at = b.len();
        b.extend_from_slice(&vop3(0x360, dst, [v(9), ic(0), 0]));
        b.extend_from_slice(&[vop1(1, 0, read_dst), vop1(1, 1, ic(0)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
        cs.push(LaneCase { m: "v_readlane_b32", width: 1, body: b, at, exp: lanes_of(|l| [want, 0, l as u32, 0], 1) });
    }
}

#[test]
fn readlane_special_scalar_dsts() {
    let mut cs = Vec::new();
    readlane_special_cases(&mut cs, 4);
    run_lane_cases("lanes_rw_special_warp", Dev::Warp, &cs);
    if hw_requested() {
        let mut cs = Vec::new();
        readlane_special_cases(&mut cs, 32);
        run_lane_cases("lanes_rw_special_hw", Dev::Hw, &cs);
    }
}

pub fn permlane(x16: bool, src: &[u32], old: &[u32], exec: u32, lo: u32, hi: u32, fi: bool, bc: bool) -> Vec<u32> {
    (0..WAVE)
        .map(|i| {
            if exec >> i & 1 == 0 {
                return old[i];
            }
            let row = if x16 { (i / 16) ^ 1 } else { i / 16 };
            let q = i % 16;
            let sel = if q < 8 { (lo >> (q * 4)) & 15 } else { (hi >> ((q - 8) * 4)) & 15 } as usize;
            let sl = row * 16 + sel;
            if fi || exec >> sl & 1 != 0 { src[sl] } else if bc { 0 } else { old[i] }
        })
        .collect()
}

fn permlane_cases(cs: &mut Vec<LaneCase>) {
    let src: Vec<u32> = (0..WAVE).map(|l| lane_val(l, MA, AA)).collect();
    let old: Vec<u32> = (0..WAVE).map(|l| lane_val(l, MO, AO)).collect();
    let sels: [(u32, u32); 4] = [(0x7654_3210, 0xfedc_ba98), (0x0123_4567, 0x89ab_cdef), (0x0000_0000, 0xffff_ffff), (0x3f2e_1d0c, 0x8b9a_4756)];
    let execs: [u32; 4] = [0xffff_ffff, 0x5555_aaaa, 0x0000_ffff, 0xfffe_7ffe];
    for (op, m, x16) in [(0x377u32, "v_permlane16_b32", false), (0x378, "v_permlanex16_b32", true)] {
        for &(lo, hi) in &sels {
            for &ex in &execs {
                for opsel in 0..4u32 {
                    let mut b = seed(5, MA, AA);
                    b.extend_from_slice(&seed(7, MO, AO));
                    b.extend_from_slice(&[sop1(3, 20, LIT), lo, sop1(3, 21, LIT), hi]);
                    b.extend_from_slice(&set_exec(ex));
                    let at = b.len();
                    b.extend_from_slice(&vop3m(op, 7, [v(5), 20, 21], 0, 0, opsel, false));
                    b.extend_from_slice(&all_exec());
                    b.extend_from_slice(&[vop1(1, 0, v(7)), vop1(1, 1, v(5)), vop1(1, 2, v(LANE)), vop1(1, 3, ic(0))]);
                    let r = permlane(x16, &src, &old, ex, lo, hi, opsel & 1 != 0, opsel & 2 != 0);
                    cs.push(LaneCase { m, width: 32, body: b, at, exp: lanes_of(|l| [r[l], src[l], l as u32, 0], 32) });
                }
            }
        }
    }
}

#[test]
fn permlane_lanes() {
    let mut cs = Vec::new();
    permlane_cases(&mut cs);
    if hw_requested() {
        run_lane_cases("lanes_permlane_hw", Dev::Hw, &cs);
    } else {
        let w = wave_width(&Dev::Warp).unwrap();
        assert!(w < 32, "WARP now runs {w}-lane waves: run permlane on it");
        println!("permlane: WARP waves are {w} lanes; v_permlane16/x16 need 32. Set TOMBOX_EXEC_HW=1 to run them on the hardware device.");
    }
}

pub enum Src {
    Lane(usize),
    Zero,
    Off,
}

pub fn dpp16_lane(ctrl: u32, lane: usize) -> Option<usize> {
    let (row, r, n) = (lane & !15, lane & 15, (ctrl & 15) as usize);
    match ctrl {
        0x000..=0x0ff => Some((lane & !3) + ((ctrl as usize >> ((lane & 3) * 2)) & 3)),
        0x101..=0x10f => (r + n < 16).then_some(row + r + n),
        0x111..=0x11f => (r >= n).then(|| row + r - n),
        0x121..=0x12f => Some(row + ((r + 16 - n) & 15)),
        0x140 => Some(row + 15 - r),
        0x141 => Some((lane & !7) + 7 - (lane & 7)),
        0x150..=0x15f => Some(row + n),
        0x160..=0x16f => Some(row + (r ^ n)),
        _ => None,
    }
}

pub fn dpp_src(code: u32, ext: u32, exec: u32, lane: usize) -> Src {
    let on = |s: usize| exec >> s & 1 != 0;
    if code == 250 {
        let (rm, bm) = ((ext >> 28) & 15, (ext >> 24) & 15);
        if rm >> (lane >> 4) & 1 == 0 || bm >> ((lane >> 2) & 3) & 1 == 0 {
            return Src::Off;
        }
        match dpp16_lane((ext >> 8) & 0x1ff, lane) {
            Some(s) if (ext >> 18) & 1 != 0 || on(s) => Src::Lane(s),
            _ if (ext >> 19) & 1 != 0 => Src::Zero,
            _ => Src::Off,
        }
    } else {
        let s = (lane & !7) | ((ext >> (8 + 3 * (lane & 7))) & 7) as usize;
        if code == 234 || on(s) { Src::Lane(s) } else { Src::Zero }
    }
}

const MB: u32 = 3;
const AB: u32 = 0x4000_0000;

fn dpp_case(cs: &mut Vec<LaneCase>, width: u32, code: u32, ext: u32, exec: u32, kind: u32) {
    let w = width as usize;
    let src: Vec<u32> = (0..w).map(|l| lane_val(l, MA, AA)).collect();
    let old: Vec<u32> = (0..w).map(|l| lane_val(l, MO, AO)).collect();
    let b1: Vec<u32> = (0..w).map(|l| lane_val(l, MB, AB)).collect();
    let mut b = seed(5, MA, AA);
    b.extend_from_slice(&seed(7, MO, AO));
    b.extend_from_slice(&seed(8, MB, AB));
    b.extend_from_slice(&[vop1(1, 3, ic(0)), vop1(1, 9, ic(1))]);
    b.extend_from_slice(&set_exec(exec));
    let at = b.len();
    let ext = ext & !0xff | 5;
    let (m, op): (&'static str, [u32; 2]) = match kind {
        0 => ("v_mov_b32", [vop1(1, 7, code), ext]),
        1 => ("v_add_nc_u32", [vop2(0x25, 7, code, 8), ext]),
        2 => ("v_add_f32", [vop2(0x03, 7, code, 8), ext]),
        3 => ("v_cmp_gt_u32", [vopc(0xc4, code, 8), ext]),
        _ => ("v_cmpx_gt_u32", [vopc(0xd4, code, 8), ext]),
    };
    b.extend_from_slice(&op);
    if kind == 4 {
        b.push(vop1(1, 3, ic(1)));
    }
    b.extend_from_slice(&all_exec());
    if kind == 3 {
        b.push(vop2(0x01, 3, ic(0), 9));
    }
    b.extend_from_slice(&[vop1(1, 0, v(7)), vop1(1, 1, v(5)), vop1(1, 2, v(LANE))]);
    let fneg = |x: u32, neg: bool, abs: bool| {
        let x = if abs { x & 0x7fff_ffff } else { x };
        if neg { x ^ 0x8000_0000 } else { x }
    };
    let mods = if code == 250 { (ext >> 20) & 15 } else { 0 };
    let exp = lanes_of(
        |l| {
            let on = exec >> l & 1 != 0;
            let s = dpp_src(code, ext, exec, l);
            let a = match s {
                Src::Lane(k) => Some(src[k]),
                Src::Zero => Some(0),
                Src::Off => None,
            };
            let a = a.filter(|_| on);
            let res = match (kind, a) {
                (_, None) => old[l],
                (0, Some(a)) => a,
                (1, Some(a)) => a.wrapping_add(b1[l]),
                (2, Some(a)) => (f32::from_bits(fneg(a, mods & 1 != 0, mods & 2 != 0)) + f32::from_bits(fneg(b1[l], mods & 4 != 0, mods & 8 != 0))).to_bits(),
                _ => old[l],
            };
            let flag = match (kind, a) {
                (3 | 4, Some(a)) => (a > b1[l]) as u32,
                _ => 0,
            };
            [res, src[l], l as u32, flag]
        },
        width,
    );
    cs.push(LaneCase { m, width, body: b, at, exp });
}

pub fn dpp16(ctrl: u32, fi: bool, bc: bool, mods: u32, bank: u32, row: u32) -> u32 {
    (ctrl << 8) | ((fi as u32) << 18) | ((bc as u32) << 19) | (mods << 20) | (bank << 24) | (row << 28)
}

pub fn dpp8(sel: [u32; 8]) -> u32 {
    sel.iter().enumerate().fold(0, |a, (k, &s)| a | ((s & 7) << (8 + 3 * k)))
}

fn dpp_cases(cs: &mut Vec<LaneCase>, width: u32) {
    let quad = [0xe4u32, 0x1b, 0x00, 0x55, 0x9c];
    let wide = [0x101u32, 0x103, 0x10f, 0x111, 0x114, 0x11f, 0x121, 0x127, 0x140, 0x141, 0x150, 0x155, 0x161, 0x16f];
    let ctrls: Vec<u32> = if width >= 16 { quad.iter().chain(wide.iter()).copied().collect() } else { quad.to_vec() };
    let execs: Vec<u32> = if width >= 32 { vec![u32::MAX, 0x5a5a_a5a5, 0xffff_0ff7] } else { vec![0xf, 0xb, 0x6] };
    for &ctrl in &ctrls {
        for &ex in &execs {
            for (fi, bc) in [(false, false), (true, false), (false, true), (true, true)] {
                dpp_case(cs, width, 250, dpp16(ctrl, fi, bc, 0, 15, 15), ex, 0);
            }
            dpp_case(cs, width, 250, dpp16(ctrl, false, false, 0, 15, 15), ex, 1);
            dpp_case(cs, width, 250, dpp16(ctrl, false, true, 0, 15, 15), ex, 3);
        }
    }
    let ex0 = *execs.last().unwrap();
    for &(bank, row) in &[(0xau32, 0xfu32), (0x5, 0xf), (0xf, 0x2), (0x3, 0x1)] {
        dpp_case(cs, width, 250, dpp16(0x1b, false, false, 0, bank, row), ex0, 0);
        dpp_case(cs, width, 250, dpp16(0x1b, true, true, 0, bank, row), u32::MAX, 1);
    }
    for mods in [1u32, 2, 3, 4, 8, 12, 15] {
        dpp_case(cs, width, 250, dpp16(0x1b, false, true, mods, 15, 15), ex0, 2);
    }
    dpp_case(cs, width, 250, dpp16(0x1b, false, true, 0, 15, 15), ex0, 4);
    if width >= 8 {
        for sel in [[0u32, 1, 2, 3, 4, 5, 6, 7], [7, 6, 5, 4, 3, 2, 1, 0], [1, 1, 3, 3, 0, 0, 6, 2], [4, 5, 6, 7, 0, 1, 2, 3]] {
            for &ex in &execs {
                for code in [233u32, 234] {
                    dpp_case(cs, width, code, dpp8(sel), ex, 0);
                    dpp_case(cs, width, code, dpp8(sel), ex, 1);
                }
            }
        }
    }
}

#[test]
fn dpp_lanes() {
    let mut cs = Vec::new();
    dpp_cases(&mut cs, 4);
    run_lane_cases("lanes_dpp_warp", Dev::Warp, &cs);
    if hw_requested() {
        let mut cs = Vec::new();
        dpp_cases(&mut cs, 32);
        run_lane_cases("lanes_dpp_hw", Dev::Hw, &cs);
    }
}
