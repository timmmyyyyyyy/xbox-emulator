use super::*;
use tombox_xshader::hlsl::{TB_GMEM_SPACE, TB_MIP_SPACE};
use tombox_xshader::meta::{RootParam, RootParamKind, RootRange, RootSig};

mod bvh;
mod branch;
mod cache_inv;
mod cfg_fuzz;
mod ds;
mod gds;
mod exp;
mod flat;
mod global;
mod mimg;
mod mimg_rdna2;
mod mtbuf;
mod mtbuf_hw;
mod mubuf_wide;
mod trans16;
mod trans32;
mod rootmem;
mod smem_nr;
mod sop1_dst;
mod user_data;
mod vfetch;

pub const R32G32B32A32_FLOAT: u32 = 2;
pub const R32G32B32A32_UINT: u32 = 3;
pub const R32G32B32A32_SINT: u32 = 4;
pub const R32G32_UINT: u32 = 17;
pub const R32_FLOAT: u32 = 41;
pub const R32_UINT: u32 = 42;

pub struct Img {
    pub w: u32,
    pub h: u32,
    pub layers: u32,
    pub array: bool,
    pub mips: u32,
    pub fmt: u32,
    pub texel: fn(u32, u32, u32, u32) -> [u32; 4],
}

impl Img {
    fn bpp(&self) -> usize {
        match self.fmt {
            R32_FLOAT | R32_UINT => 4,
            R32G32_UINT => 8,
            _ => 16,
        }
    }
    fn tex_desc(&self, uav: bool) -> [u8; 0x38] {
        let mut td = [0u8; 0x38];
        td[0..4].copy_from_slice(&3u32.to_le_bytes());
        td[0x10..0x18].copy_from_slice(&(self.w as u64).to_le_bytes());
        td[0x18..0x1c].copy_from_slice(&self.h.to_le_bytes());
        td[0x1c..0x1e].copy_from_slice(&(self.layers as u16).to_le_bytes());
        td[0x1e..0x20].copy_from_slice(&(self.mips as u16).to_le_bytes());
        td[0x20..0x24].copy_from_slice(&self.fmt.to_le_bytes());
        td[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
        if uav {
            td[0x30..0x34].copy_from_slice(&4u32.to_le_bytes());
        }
        td
    }
    fn subs(&self) -> u32 {
        self.mips * self.layers
    }
}

#[derive(Default)]
pub struct Env<'a> {
    pub tex: Option<&'a Img>,
    pub samp: Option<[u8; 52]>,
    pub uav: Option<&'a Img>,
    pub gmem: Option<(&'a [u32], u64)>,
    pub pos: bool,
    pub code: Option<&'a [u32]>,
}

pub struct Out {
    pub rt: [u32; 4],
    pub uav: Vec<Vec<u32>>,
}

pub const P_SRV: u32 = 8;
pub const P_SAMP: u32 = 10;
pub const P_UAV: u32 = 12;

fn table(range_type: u32, num: u32) -> RootParam {
    RootParam { visibility: 5, kind: RootParamKind::Table(vec![RootRange { range_type, num, base_register: 0, space: 0, offset_in_table: u32::MAX }]) }
}

pub fn root() -> RootSig {
    RootSig { version: 2, flags: 1, params: vec![table(0, 64), table(0, 1), table(3, 16), table(0, 1), table(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() }
}

pub fn extract(words: &[u32], pos: bool) -> ShaderExtract {
    extract_code(words, pos, None)
}

pub fn extract_code(words: &[u32], pos: bool, code: Option<&[u32]>) -> ShaderExtract {
    let mut ex = shader_root(words, root());
    if let Some(c) = code {
        ex.isa_data.extend(c.iter().flat_map(|w| w.to_le_bytes()));
    }
    if pos {
        ex.input_sig = vec![SigElement { name: "SV_Position".into(), index: 0, sysval: 1, comp_type: 3, register: 0, mask: 0xf, rw_mask: 0xf }];
    }
    ex
}

unsafe fn ser_ranges(params: &[Vec<[u32; 5]>]) -> Result<Vec<u8>, String> {
    let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let ser: unsafe extern "system" fn(*const u8, u32, *mut *mut c_void, *mut *mut c_void) -> i32 =
        core::mem::transmute(GetProcAddress(d3d, b"D3D12SerializeRootSignature\0".as_ptr()));
    let mut ps = vec![0u8; 32 * params.len().max(1)];
    for (i, r) in params.iter().enumerate() {
        let o = i * 32;
        ps[o + 8..o + 12].copy_from_slice(&(r.len() as u32).to_le_bytes());
        ps[o + 16..o + 24].copy_from_slice(&(r.as_ptr() as u64).to_le_bytes());
    }
    let mut desc = [0u8; 40];
    desc[0..4].copy_from_slice(&(params.len() as u32).to_le_bytes());
    desc[8..16].copy_from_slice(&(ps.as_ptr() as u64).to_le_bytes());
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    if ser(desc.as_ptr(), 1, &mut blob, &mut err) < 0 {
        let msg = if err.is_null() { String::new() } else { std::ffi::CStr::from_ptr(vcall!(err, 3, fn() -> *const i8)).to_string_lossy().into_owned() };
        return Err(format!("serialize root signature: {msg}"));
    }
    let p = vcall!(blob, 3, fn() -> *const u8);
    let n = vcall!(blob, 4, fn() -> usize);
    Ok(core::slice::from_raw_parts(p, n).to_vec())
}

unsafe fn upload_img(g: &Gpu, img: &Img, uav: bool, kill: &mut Vec<*mut c_void>) -> Result<(*mut c_void, *mut c_void, Vec<[u8; 32]>, u64), String> {
    let td = img.tex_desc(uav);
    let res = g.committed(&td, d3d::default_heap(), d3d::COPY_DEST)?;
    kill.push(res);
    let (fps, total) = g.footprints(&td, img.subs());
    let up = g.buffer(total, 2, d3d::GENERIC_READ, false)?;
    kill.push(up);
    let mut bytes = vec![0u8; total as usize];
    let bpp = img.bpp();
    for layer in 0..img.layers {
        for mip in 0..img.mips {
            let fp = &fps[(mip + layer * img.mips) as usize];
            let off = u64::from_le_bytes(fp[0..8].try_into().unwrap()) as usize;
            let pitch = u32::from_le_bytes(fp[24..28].try_into().unwrap()) as usize;
            let (mw, mh) = ((img.w >> mip).max(1), (img.h >> mip).max(1));
            for y in 0..mh {
                for x in 0..mw {
                    let v = (img.texel)(mip, layer, x, y);
                    for k in 0..bpp / 4 {
                        let o = off + y as usize * pitch + x as usize * bpp + k * 4;
                        bytes[o..o + 4].copy_from_slice(&v[k].to_le_bytes());
                    }
                }
            }
        }
    }
    Gpu::map_write(up, &bytes);
    Ok((res, up, fps, total))
}

unsafe fn copy_subs(l: *mut c_void, dst: *mut c_void, src: *mut c_void, fps: &[[u8; 32]], to_texture: bool) {
    for (i, fp) in fps.iter().enumerate() {
        let mut tex = [0u8; 48];
        let mut buf = [0u8; 48];
        let (t, b) = if to_texture { (dst, src) } else { (src, dst) };
        tex[0..8].copy_from_slice(&(t as u64).to_le_bytes());
        tex[16..20].copy_from_slice(&(i as u32).to_le_bytes());
        buf[0..8].copy_from_slice(&(b as u64).to_le_bytes());
        buf[8..12].copy_from_slice(&1u32.to_le_bytes());
        buf[16..48].copy_from_slice(fp);
        let (d, s) = if to_texture { (tex, buf) } else { (buf, tex) };
        vcall!(l, 16, fn(*const u8, u32, u32, u32, *const u8, *const c_void) -> (), d.as_ptr(), 0, 0, 0, s.as_ptr(), core::ptr::null());
    }
}

fn srv_desc(img: &Img, first_mip: u32, mips: u32) -> [u8; 40] {
    let mut sd = [0u8; 40];
    sd[0..4].copy_from_slice(&img.fmt.to_le_bytes());
    sd[8..12].copy_from_slice(&0x1688u32.to_le_bytes());
    if img.array {
        sd[4..8].copy_from_slice(&5u32.to_le_bytes());
        sd[16..20].copy_from_slice(&first_mip.to_le_bytes());
        sd[20..24].copy_from_slice(&mips.to_le_bytes());
        sd[28..32].copy_from_slice(&img.layers.to_le_bytes());
    } else {
        sd[4..8].copy_from_slice(&4u32.to_le_bytes());
        sd[16..20].copy_from_slice(&first_mip.to_le_bytes());
        sd[20..24].copy_from_slice(&mips.to_le_bytes());
    }
    sd
}

fn uav_desc(img: &Img, mip: u32) -> [u8; 40] {
    let mut ud = [0u8; 40];
    ud[0..4].copy_from_slice(&img.fmt.to_le_bytes());
    if img.array {
        ud[4..8].copy_from_slice(&5u32.to_le_bytes());
        ud[8..12].copy_from_slice(&mip.to_le_bytes());
        ud[16..20].copy_from_slice(&img.layers.to_le_bytes());
    } else {
        ud[4..8].copy_from_slice(&4u32.to_le_bytes());
        ud[8..12].copy_from_slice(&mip.to_le_bytes());
    }
    ud
}

pub fn exec(words: &[u32], env: &Env) -> Result<Out, String> {
    let ex = extract_code(words, env.pos, env.code);
    let t = translate(&ex)?;
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { exec_in(&mut c, &t, &code, env) }.map_err(|e| format!("{e}\n{}", t.hlsl))
}

unsafe fn exec_in(c: &mut Ctx, t: &tombox_xshader::hlsl::Translated, code: &[u8], env: &Env) -> Result<Out, String> {
    let ranges: Vec<Vec<[u32; 5]>> = vec![
        vec![[0, 64, 0, 0, 0xffff_ffff]],
        vec![[0, 1, 64, 0, 0xffff_ffff]],
        vec![[3, 16, 0, 0, 0xffff_ffff]],
        vec![[0, 1, 65, 0, 0xffff_ffff]],
        vec![[1, 8, 0, 0, 0xffff_ffff]],
        vec![[1, 16, 0, TB_MIP_SPACE, 0xffff_ffff], [0, 16, 0, TB_MIP_SPACE, 0xffff_ffff]],
        vec![[0, 1, 0, TB_GMEM_SPACE, 0xffff_ffff], [2, 1, 0, TB_GMEM_SPACE, 0xffff_ffff]],
    ];
    let g = &c.gpu;
    let blob = ser_ranges(&ranges)?;
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, blob.as_ptr(), blob.len(), &d3d::IID_ROOTSIG, &mut rs);
    if hr < 0 {
        return Err(format!("CreateRootSignature {hr:#x}{}", g.messages()));
    }
    let (vh, vcpu, vgpu, vinc) = g.heap(0, 7 * 64, true)?;
    let (sh, scpu, sgpu, sinc) = g.heap(1, 16, true)?;
    let mut kill: Vec<*mut c_void> = vec![rs, vh, sh];
    let at = |p: usize, k: usize| vcpu + (p * 64 + k) * vinc;
    let mut null_srv = [0u8; 40];
    null_srv[0..4].copy_from_slice(&R32G32B32A32_FLOAT.to_le_bytes());
    null_srv[4..8].copy_from_slice(&4u32.to_le_bytes());
    null_srv[8..12].copy_from_slice(&0x1688u32.to_le_bytes());
    null_srv[20..24].copy_from_slice(&1u32.to_le_bytes());
    for p in [0usize, 1, 3] {
        for k in 0..64 {
            vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), core::ptr::null_mut(), null_srv.as_ptr(), at(p, k));
        }
    }
    for k in 16..32 {
        vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), core::ptr::null_mut(), null_srv.as_ptr(), at(5, k));
    }
    let mut null_uav = [0u8; 40];
    null_uav[0..4].copy_from_slice(&R32G32B32A32_FLOAT.to_le_bytes());
    null_uav[4..8].copy_from_slice(&4u32.to_le_bytes());
    for (p, n) in [(4usize, 8usize), (5, 16)] {
        for k in 0..n {
            vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), core::ptr::null_mut(), core::ptr::null_mut(), null_uav.as_ptr(), at(p, k));
        }
    }
    let samp = env.samp.unwrap_or_else(|| sampler(0, 3, 0));
    for k in 0..16 {
        vcall!(g.device, 22, fn(*const u8, usize) -> (), samp.as_ptr(), scpu + k * sinc);
    }
    let mut tex_up: Option<(*mut c_void, *mut c_void, Vec<[u8; 32]>)> = None;
    if let Some(img) = env.tex {
        let (res, up, fps, _) = upload_img(g, img, false, &mut kill)?;
        let sd = srv_desc(img, 0, img.mips);
        vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), res, sd.as_ptr(), at(0, 0));
        for mip in 0..img.mips {
            let sd = srv_desc(img, mip, 1);
            vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), res, sd.as_ptr(), at(5, 16 + mip as usize));
        }
        tex_up = Some((res, up, fps));
    }
    let mut uav_up: Option<(*mut c_void, *mut c_void, Vec<[u8; 32]>, u64)> = None;
    if let Some(img) = env.uav {
        let (res, up, fps, total) = upload_img(g, img, true, &mut kill)?;
        let ud = uav_desc(img, 0);
        vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), res, core::ptr::null_mut(), ud.as_ptr(), at(4, 0));
        for mip in 0..img.mips {
            let ud = uav_desc(img, mip);
            vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), res, core::ptr::null_mut(), ud.as_ptr(), at(5, mip as usize));
        }
        uav_up = Some((res, up, fps, total));
    }
    if let Some((words, base)) = env.gmem {
        let n = (words.len() * 4).max(256) as u64;
        let gb = g.buffer(n, 2, d3d::GENERIC_READ, false)?;
        kill.push(gb);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        Gpu::map_write(gb, &bytes);
        let mut sd = [0u8; 40];
        sd[4..8].copy_from_slice(&1u32.to_le_bytes());
        sd[8..12].copy_from_slice(&0x1688u32.to_le_bytes());
        sd[0..4].copy_from_slice(&39u32.to_le_bytes());
        sd[24..28].copy_from_slice(&((n / 4) as u32).to_le_bytes());
        sd[32..36].copy_from_slice(&1u32.to_le_bytes());
        vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), gb, sd.as_ptr(), at(6, 0));
        let cb = g.buffer(256, 2, d3d::GENERIC_READ, false)?;
        kill.push(cb);
        let mut cbd = vec![0u8; 256];
        cbd[0..8].copy_from_slice(&base.to_le_bytes());
        Gpu::map_write(cb, &cbd);
        let cv = [Gpu::gpu_va(cb), 256u64];
        vcall!(g.device, 17, fn(*const u64, usize) -> (), cv.as_ptr(), at(6, 1));
    }
    let rb_uav = match &uav_up {
        Some((_, _, _, total)) => {
            let b = c.gpu.buffer(*total, 3, d3d::COPY_DEST, false)?;
            kill.push(b);
            b
        }
        None => core::ptr::null_mut(),
    };
    let gp = &c.gpu as *const Gpu;
    let heaps = [vh, sh];
    let tables: Vec<u64> = (0..7).map(|p| if p == 2 { sgpu } else { vgpu + (p * 64 * vinc) as u64 }).collect();
    let tex_up2 = tex_up.as_ref().map(|(r, u, f)| (*r, *u, f.clone()));
    let uav_up2 = uav_up.as_ref().map(|(r, u, f, _)| (*r, *u, f.clone()));
    let pre = |l: *mut c_void| {
        let g = &*gp;
        if let Some((res, up, fps)) = &tex_up2 {
            copy_subs(l, *res, *up, fps, true);
            g.barrier(*res, d3d::COPY_DEST, 0x40 | 0x80);
        }
        if let Some((res, up, fps)) = &uav_up2 {
            copy_subs(l, *res, *up, fps, true);
            g.barrier(*res, d3d::COPY_DEST, 8);
        }
        vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 2, heaps.as_ptr());
        vcall!(l, 30, fn(*mut c_void) -> (), rs);
        for (i, &h) in tables.iter().enumerate() {
            vcall!(l, 32, fn(u32, u64) -> (), i as u32, h);
        }
    };
    let post = |l: *mut c_void| {
        let g = &*gp;
        if let Some((res, _, fps)) = &uav_up2 {
            g.barrier(*res, 8, d3d::COPY_SOURCE);
            copy_subs(l, rb_uav, *res, fps, false);
        }
    };
    let r = draw_with(c, code, rs, &pre, &post);
    let out = r.map(|rt| {
        let mut uav = Vec::new();
        if let (Some(img), Some((_, _, fps, total))) = (env.uav, &uav_up) {
            let b = Gpu::map_read(rb_uav, *total as usize).unwrap_or_default();
            let bpp = img.bpp();
            for (i, fp) in fps.iter().enumerate() {
                let mip = i as u32 % img.mips;
                let off = u64::from_le_bytes(fp[0..8].try_into().unwrap()) as usize;
                let pitch = u32::from_le_bytes(fp[24..28].try_into().unwrap()) as usize;
                let (mw, mh) = ((img.w >> mip).max(1), (img.h >> mip).max(1));
                let mut v = Vec::new();
                for y in 0..mh as usize {
                    for x in 0..(mw as usize * bpp / 4) {
                        let o = off + y * pitch + x * 4;
                        v.push(b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).unwrap_or(0));
                    }
                }
                uav.push(v);
            }
        }
        Out { rt, uav }
    });
    let _ = t;
    for k in kill {
        d3d::release(k);
    }
    out
}

fn find_op(words: &[u32], op: &[u32]) -> Option<usize> {
    if op.is_empty() || op.len() > words.len() {
        return None;
    }
    (0..=words.len() - op.len()).find(|&k| words[k..k + op.len()] == op[..])
}

pub fn runtime_words(words: &[u32], op: &[u32], zero: u32) -> Option<Vec<u32>> {
    let at = find_op(words, op)?;
    let isa: Vec<u8> = op.iter().flat_map(|w| w.to_le_bytes()).collect();
    let m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
    let keep: Vec<u32> = if m.starts_with("image_") && m.ends_with("_o") && op.len() > 1 { vec![op[1] & 0xff] } else { vec![] };
    let mut w = crate::vop::opaque_keep(&words[..at], zero, &keep);
    if w[1..] == words[..at] {
        return None;
    }
    w.extend_from_slice(&words[at..]);
    Some(w)
}

fn with_runtime<T>(cs: &[T], f: impl Fn(&T) -> Option<Vec<u32>>) -> Vec<(usize, &T, Option<Vec<u32>>)> {
    let mut v = Vec::new();
    for (i, c) in cs.iter().enumerate() {
        v.push((i, c, None));
        if let Some(w) = f(c) {
            v.push((i, c, Some(w)));
        }
    }
    v
}

pub struct MCase<'a> {
    pub m: &'static str,
    pub op: Vec<u32>,
    pub words: Vec<u32>,
    pub env: Env<'a>,
    pub rt: Vec<u32>,
    pub uav: Vec<(usize, usize, u32)>,
    pub tol: f32,
}

pub fn atomic_int64_on_typed_resource_supported() -> bool {
    static SUP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUP.get_or_init(|| {
        let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
        let c = m.lock().unwrap_or_else(|e| e.into_inner());
        let mut o9 = [0u32; 6];
        let hr = unsafe { vcall!(c.gpu.device, 13, fn(u32, *mut u32, u32) -> i32, 37, o9.as_mut_ptr(), 24) };
        hr >= 0 && o9[2] != 0
    })
}

fn max_shader_model() -> u32 {
    static MAX: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MAX.get_or_init(|| {
        let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
        let c = m.lock().unwrap_or_else(|e| e.into_inner());
        for sm in (0x60u32..=0x68).rev() {
            let mut v = [sm];
            let hr = unsafe { vcall!(c.gpu.device, 13, fn(u32, *mut u32, u32) -> i32, 7, v.as_mut_ptr(), 4) };
            if hr >= 0 {
                return v[0];
            }
        }
        0x60
    })
}

fn profile_shader_model(profile: &str) -> u32 {
    let mut parts = profile.split('_').skip(1);
    match (parts.next(), parts.next().and_then(|m| m.parse::<u32>().ok())) {
        (Some("6"), Some(minor)) => 0x60 + minor,
        _ => 0x60,
    }
}

pub fn run_mem_cases(group: &str, cs: &[MCase]) {
    let mut skipped = 0;
    let mut fails = Vec::new();
    let mut all: std::collections::BTreeSet<&str> = Default::default();
    let mut bad: std::collections::BTreeSet<&str> = Default::default();
    let mut runtime = 0;
    for (i, c, rtw) in with_runtime(cs, |c| runtime_words(&c.words, &c.op, vop1(0x07, 200, v(2)))) {
        all.insert(c.m);
        let isa: Vec<u8> = c.op.iter().flat_map(|w| w.to_le_bytes()).collect();
        let got_m = tombox_xshader::rdna2::disassemble(&isa).into_iter().next().map(|i| i.mnemonic).unwrap_or_default();
        if got_m != c.m {
            fails.push(format!("#{i} {}: decodes as {got_m}", c.m));
            bad.insert(c.m);
            continue;
        }
        let tag = if rtw.is_some() { runtime += 1; " (runtime inputs)" } else { "" };
        let words = rtw.as_deref().unwrap_or(&c.words);
        let env = Env { tex: c.env.tex, samp: c.env.samp, uav: c.env.uav, gmem: c.env.gmem, pos: c.env.pos || rtw.is_some(), code: c.env.code };
        match exec(words, &env) {
            Ok(o) => {
                let mut ok = if c.tol > 0.0 {
                    o.rt[..c.rt.len()].iter().zip(&c.rt).all(|(&a, &b)| (f32::from_bits(a) - f32::from_bits(b)).abs() <= c.tol)
                } else {
                    o.rt[..c.rt.len()] == c.rt[..]
                };
                let mut got = Vec::new();
                for &(sub, k, want) in &c.uav {
                    let g = o.uav.get(sub).and_then(|v| v.get(k)).copied().unwrap_or(0xbad0_bad0);
                    got.push(g);
                    ok &= g == want;
                }
                if !ok {
                    let h = translate(&extract_code(words, env.pos, c.env.code)).map(|t| t.hlsl).unwrap_or_default();
                    let body: String = h.lines().filter(|l| l.starts_with("    ") && !l.contains("TB_RTFMT") && !l.contains("// 0x")).collect::<Vec<_>>().join("\n");
                    let want_mem: Vec<u32> = c.uav.iter().map(|x| x.2).collect();
                    fails.push(format!("#{i} {}{tag}: rt {:08x?} want {:08x?}; mem {:08x?} want {:08x?}\n{body}", c.m, &o.rt[..c.rt.len()], c.rt, got, want_mem));
                    bad.insert(c.m);
                }
            }
            Err(e) => {
                let need = translate(&extract_code(words, env.pos, c.env.code)).ok().map_or(0, |t| profile_shader_model(t.profile));
                if need > max_shader_model() && e.contains("hr=0x80070057") {
                    println!("SKIP #{i} {}{tag}: needs shader model {need:#x}, this D3D12 runtime supports up to {:#x}", c.m, max_shader_model());
                    skipped += 1;
                    bad.insert(c.m);
                    continue;
                }
                fails.push(format!("#{i} {}{tag}: {}", c.m, e.lines().take(4).collect::<Vec<_>>().join(" | ")));
                bad.insert(c.m);
            }
        }
    }
    let ok = verified(group, &all, &bad);
    println!("{group}: {} mnemonics verified, {} failing or skipped, {} cases, {runtime} rerun with runtime inputs, {skipped} cases skipped for the runtime's shader model", ok.len(), bad.len(), cs.len());
    assert!(fails.is_empty(), "{group}: {} failing\n{}", fails.len(), fails.join("\n\n"));
}

#[test]
fn mem_device_features() {
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let mut sm = [0x68u32];
        let hr = vcall!(c.gpu.device, 13, fn(u32, *mut u32, u32) -> i32, 7, sm.as_mut_ptr(), 4);
        let mut o14 = [0u32; 3];
        let hr14 = vcall!(c.gpu.device, 13, fn(u32, *mut u32, u32) -> i32, 43, o14.as_mut_ptr(), 12);
        let mut o1 = [0u32; 6];
        let hr1 = vcall!(c.gpu.device, 13, fn(u32, *mut u32, u32) -> i32, 8, o1.as_mut_ptr(), 24);
        let mut o9 = [0u32; 6];
        let hr9 = vcall!(c.gpu.device, 13, fn(u32, *mut u32, u32) -> i32, 37, o9.as_mut_ptr(), 24);
        println!("shader model {:#x} (hr {hr:#x}); options14 {o14:?} (hr {hr14:#x}); options1 {o1:?} (hr {hr1:#x}); options9 {o9:?} (hr {hr9:#x})", sm[0]);
    }
}

pub const CS_BYTES: u64 = 4096;

pub fn cs_extract(words: &[u32], threads: u32) -> ShaderExtract {
    cs_extract_fssm(words, threads, &[])
}

pub fn cs_fssm(compute_pgm_rsrc2: u32) -> Vec<u8> {
    cs_fssm_sub(5, 0x130, compute_pgm_rsrc2)
}

pub fn cs_fssm_sub(sub_version: u32, at: usize, compute_pgm_rsrc2: u32) -> Vec<u8> {
    let mut b = vec![0u8; 0x13c];
    b[0x10..0x14].copy_from_slice(&sub_version.to_le_bytes());
    b[at..at + 4].copy_from_slice(&compute_pgm_rsrc2.to_le_bytes());
    b
}

pub fn cs_extract_fssm(words: &[u32], threads: u32, fssm: &[u8]) -> ShaderExtract {
    let mut ex = shader_root(words, RootSig { version: 2, flags: 1, params: vec![table(1, 8)], num_static_samplers: 0, static_samplers: Vec::new() });
    ex.stage = Stage::Compute;
    ex.isa_preamble = [0, threads, 1, 1];
    ex.fssm = fssm.to_vec();
    if !fssm.is_empty() {
        ex.isa_offset_in_fssm = fssm.len() + 16;
    }
    ex.output_sig = vec![];
    ex.has_output_sig = false;
    ex.has_input_sig = false;
    ex
}

pub fn exec_cs(words: &[u32], threads: u32, init: &[u32]) -> Result<Vec<u32>, String> {
    exec_cs_fssm(words, threads, init, &[])
}

pub fn exec_cs_fssm(words: &[u32], threads: u32, init: &[u32], fssm: &[u8]) -> Result<Vec<u32>, String> {
    let ex = cs_extract_fssm(words, threads, fssm);
    let t = translate(&ex)?;
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(unsafe { ctx_new() }));
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { exec_cs_in(&mut c, &t.hlsl, &code, init) }.map_err(|e| format!("{e}\n{}", t.hlsl))
}

unsafe fn exec_cs_in(c: &mut Ctx, hlsl: &str, code: &[u8], init: &[u32]) -> Result<Vec<u32>, String> {
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
    let up = g.buffer(CS_BYTES, 2, d3d::GENERIC_READ, false)?;
    let rb = g.buffer(CS_BYTES, 3, d3d::COPY_DEST, false)?;
    let mut bytes: Vec<u8> = init.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.resize(CS_BYTES as usize, 0);
    Gpu::map_write(up, &bytes);
    let (stride, fmt) = if hlsl.contains("RWStructuredBuffer<TbElem_ubuf0>") {
        let n: u32 = hlsl.split("struct TbElem_ubuf0 { uint w[").nth(1).and_then(|s| s.split(']').next()).and_then(|s| s.parse().ok()).unwrap_or(1);
        (n * 4, 0u32)
    } else if hlsl.contains("RWStructuredBuffer<uint> ubuf0") {
        (4, 0)
    } else if hlsl.contains("RWBuffer<uint4>") {
        (0, 3)
    } else if hlsl.contains("RWBuffer<uint>") {
        (0, 42)
    } else {
        (0, 41)
    };
    let esz = if stride != 0 { stride } else if fmt == 3 { 16 } else { 4 };
    let mut ud = [0u8; 40];
    ud[0..4].copy_from_slice(&fmt.to_le_bytes());
    ud[4..8].copy_from_slice(&1u32.to_le_bytes());
    ud[16..20].copy_from_slice(&((CS_BYTES as u32) / esz).to_le_bytes());
    ud[20..24].copy_from_slice(&stride.to_le_bytes());
    vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), ubuf, core::ptr::null_mut(), ud.as_ptr(), vcpu);
    let _ = vinc;
    c.gpu.begin();
    let l = c.gpu.list;
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), ubuf, 0, up, 0, CS_BYTES);
    c.gpu.barrier(ubuf, d3d::COPY_DEST, 8);
    let heaps = [vh];
    vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 1, heaps.as_ptr());
    vcall!(l, 29, fn(*mut c_void) -> (), rs);
    vcall!(l, 31, fn(u32, u64) -> (), 0, vgpu);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    vcall!(l, 14, fn(u32, u32, u32) -> (), 1, 1, 1);
    c.gpu.barrier(ubuf, 8, d3d::COPY_SOURCE);
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, ubuf, 0, CS_BYTES);
    let r = c.gpu.submit();
    let out = r.and_then(|_| Gpu::map_read(rb, CS_BYTES as usize).ok_or_else(|| "map".to_string())).map(|b| b.chunks(4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect());
    for k in [pso, rs, vh, ubuf, up, rb] {
        d3d::release(k);
    }
    out
}

pub struct CsCase {
    pub m: &'static str,
    pub op: Vec<u32>,
    pub words: Vec<u32>,
    pub threads: u32,
    pub mem: Vec<(usize, u32)>,
    pub init: Vec<u32>,
}

pub fn run_cs_cases(group: &str, cs: &[CsCase]) {
    run_cs_cases_fssm(group, cs, &[])
}

pub fn run_cs_cases_fssm(group: &str, cs: &[CsCase], fssm: &[u8]) {
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
        let words = rtw.as_deref().unwrap_or(&c.words);
        match exec_cs_fssm(words, c.threads, &c.init, fssm) {
            Ok(o) => {
                let wrong: Vec<String> = c.mem.iter().filter(|&&(k, w)| o[k] != w).take(6).map(|&(k, w)| format!("[{k}] {:08x} want {w:08x}", o[k])).collect();
                if !wrong.is_empty() {
                    let h = translate(&cs_extract_fssm(words, c.threads, fssm)).map(|t| t.hlsl).unwrap_or_default();
                    let body: String = h.lines().filter(|l| l.starts_with("    ") && !l.contains("// 0x")).collect::<Vec<_>>().join("\n");
                    fails.push(format!("#{i} {}{tag}: {}\n{body}", c.m, wrong.join(", ")));
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

pub fn verified<'a>(group: &str, all: &std::collections::BTreeSet<&'a str>, bad: &std::collections::BTreeSet<&'a str>) -> Vec<&'a str> {
    let ok: Vec<&str> = all.iter().filter(|m| !bad.contains(*m)).copied().collect();
    if let Some(dir) = std::env::var_os("TOMBOX_EXEC_VERIFIED") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{group}.txt")), ok.join("\n") + "\n");
    }
    ok
}
