//! Runtime shader translation for PSO creation.
//!
//! Xbox shader blob (DXBC wrapper → XDAT → zlib → FSSM → RDNA2) → HLSL (tombox-xshader)
//! → DXC (`dxcompiler.dll`, SM6 DXIL; FXC/`D3DCompile` SM5 DXBC as fallback) → cached bytecode.
//! Each unique shader is translated once per process (keyed by an FNV-1a hash of its

use compat_common::record;
use core::ffi::c_void;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use tombox_xshader::hlsl::InputInterp;

type Cached = Result<Arc<Vec<u8>>, Arc<String>>;
static CACHE: OnceLock<Mutex<HashMap<u64, Arc<OnceLock<Cached>>>>> = OnceLock::new();
static MESH_ROOT_CACHE: OnceLock<Mutex<HashMap<u64, HashMap<Vec<u8>, Arc<OnceLock<Cached>>>>>> = OnceLock::new();
static OK: AtomicUsize = AtomicUsize::new(0);
static FAIL: AtomicUsize = AtomicUsize::new(0);
static DUMPED: AtomicUsize = AtomicUsize::new(0);
/// Translated code pointer -> (hash, original Xbox container); kept only for the diagnostic
/// draw trace (`rtcap`).
static ORIGIN: OnceLock<Mutex<HashMap<usize, (u64, Arc<Vec<u8>>)>>> = OnceLock::new();

static ORIGIN_HASH: OnceLock<Mutex<HashMap<usize, u64>>> = OnceLock::new();
static TYPED_ATOMIC64: AtomicI32 = AtomicI32::new(-1);

const FEATURE_SHADER_MODEL: u32 = 7;
const FEATURE_OPTIONS9: u32 = 37;
const SHADER_MODEL_6_6: u32 = 0x66;
const IID_D3D12_DEVICE: crate::device_wrap::Guid = crate::device_wrap::Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };

unsafe fn is_d3d12_device(obj: *mut c_void) -> bool {
    if obj.is_null() {
        return false;
    }
    let vt = *(obj as *const *const usize);
    let qi: unsafe extern "system" fn(*mut c_void, *const crate::device_wrap::Guid, *mut *mut c_void) -> i32 = core::mem::transmute(*vt);
    let mut dev: *mut c_void = core::ptr::null_mut();
    if qi(obj, &IID_D3D12_DEVICE, &mut dev) < 0 || dev.is_null() {
        return false;
    }
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*(*(dev as *const *const usize)).add(2));
    release(dev);
    true
}

pub(crate) unsafe fn query_typed_atomic64(real_device: *mut c_void) -> Option<(bool, i32, u32, u32)> {
    if !is_d3d12_device(real_device) {
        return None;
    }
    let vt = *(real_device as *const *const usize);
    let check: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32 = core::mem::transmute(*vt.add(13));
    let mut o9 = [0u32; 6];
    let hr9 = check(real_device, FEATURE_OPTIONS9, o9.as_mut_ptr() as *mut c_void, 24);
    let mut sm = [SHADER_MODEL_6_6];
    let hr_sm = check(real_device, FEATURE_SHADER_MODEL, sm.as_mut_ptr() as *mut c_void, 4);
    let ok = hr9 >= 0 && o9[2] != 0 && hr_sm >= 0 && sm[0] >= SHADER_MODEL_6_6;
    Some((ok, hr9, o9[2], if hr_sm >= 0 { sm[0] } else { 0 }))
}

pub(crate) unsafe fn probe_typed_atomic64(real_device: *mut c_void) {
    let Some((ok, hr9, flag, sm)) = query_typed_atomic64(real_device) else {
        return;
    };
    let prev = TYPED_ATOMIC64.swap(ok as i32, Ordering::Relaxed);
    if prev != ok as i32 {
        record(
            "d3d12_xs",
            "shader_xlat",
            &format!(
                "OPTIONS9 hr={hr9:#010x} AtomicInt64OnTypedResourceSupported={flag} shader_model={sm:#x}: 64-bit typed image atomics {}",
                if ok { "supported" } else { "unsupported (shaders using them will be refused)" }
            ),
        );
    }
}

pub(crate) fn device_typed_atomic64() -> Option<bool> {
    match TYPED_ATOMIC64.load(Ordering::Relaxed) {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

pub(crate) fn typed_atomic64_refusal(uses_typed_atomic64: bool, supported: Option<bool>) -> Option<&'static str> {
    if uses_typed_atomic64 && supported == Some(false) {
        Some("64-bit typed image atomics require D3D12_FEATURE_DATA_D3D12_OPTIONS9::AtomicInt64OnTypedResourceSupported and Shader Model 6.6")
    } else {
        None
    }
}

pub fn note_origin_hash(code: usize, hash: u64) {
    ORIGIN_HASH.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner()).insert(code, hash);
}

pub fn origin_hash(code: usize) -> Option<u64> {
    if let Some(h) = ORIGIN_HASH.get().and_then(|m| m.lock().unwrap_or_else(|e| e.into_inner()).get(&code).copied()) {
        return Some(h);
    }
    origin(code).map(|(h, _)| h)
}

pub fn origin(code: usize) -> Option<(u64, Arc<Vec<u8>>)> {
    ORIGIN.get()?.lock().unwrap_or_else(|e| e.into_inner()).get(&code).cloned()
}

pub(crate) fn fnv1a(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

type D3DCompileFn = unsafe extern "system" fn(
    *const c_void, usize, *const u8, *const c_void, *mut c_void, *const u8, *const u8, u32, u32,
    *mut *mut c_void, *mut *mut c_void,
) -> i32;

fn d3dcompile() -> Option<D3DCompileFn> {
    static F: OnceLock<usize> = OnceLock::new();
    let p = *F.get_or_init(|| unsafe {
        extern "system" {
            fn LoadLibraryA(name: *const u8) -> *mut c_void;
            fn GetProcAddress(h: *mut c_void, name: *const u8) -> *mut c_void;
        }
        let h = LoadLibraryA(b"d3dcompiler_47.dll\0".as_ptr());
        if h.is_null() {
            return 0;
        }
        GetProcAddress(h, b"D3DCompile\0".as_ptr()) as usize
    });
    if p == 0 { None } else { Some(unsafe { core::mem::transmute::<usize, D3DCompileFn>(p) }) }
}

/// Read an ID3DBlob's bytes and release it.
unsafe fn take_blob(blob: *mut c_void) -> Vec<u8> {
    if blob.is_null() {
        return Vec::new();
    }
    let vt = *(blob as *const *const usize);
    let get_ptr: unsafe extern "system" fn(*mut c_void) -> *const u8 = core::mem::transmute(*vt.add(3));
    let get_size: unsafe extern "system" fn(*mut c_void) -> usize = core::mem::transmute(*vt.add(4));
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt.add(2));
    let (p, n) = (get_ptr(blob), get_size(blob));
    let v = if p.is_null() { Vec::new() } else { core::slice::from_raw_parts(p, n).to_vec() };
    release(blob);
    v
}

fn compile(hlsl: &str, profile: &str) -> Result<Vec<u8>, String> {
    if profile.split('_').nth(1) == Some("6") {
        return Err(format!("FXC cannot compile profile {profile}: shader model 6.x needs DXC"));
    }
    let f = d3dcompile().ok_or("d3dcompiler_47.dll / D3DCompile unavailable")?;
    let target = format!("{profile}\0");
    let mut code: *mut c_void = core::ptr::null_mut();
    let mut errs: *mut c_void = core::ptr::null_mut();
    let hr = unsafe {
        f(
            hlsl.as_ptr() as *const c_void,
            hlsl.len(),
            b"tombox\0".as_ptr(),
            core::ptr::null(),
            core::ptr::null_mut(),
            b"main\0".as_ptr(),
            target.as_ptr(),
            0,
            0,
            &mut code,
            &mut errs,
        )
    };
    let err_text = String::from_utf8_lossy(&unsafe { take_blob(errs) }).into_owned();
    let bytes = unsafe { take_blob(code) };
    if hr < 0 || bytes.is_empty() {
        return Err(format!("D3DCompile hr={hr:#010x}: {}", err_text.trim()));
    }
    Ok(bytes)
}

fn dump_failure(hash: u64, blob: &[u8], hlsl: Option<&str>, err: &str) {
    if DUMPED.fetch_add(1, Ordering::Relaxed) >= 400 {
        return;
    }
    let dir = std::env::temp_dir().join("tombox_shaders");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{hash:016x}.dxbc")), blob);
    let _ = std::fs::write(dir.join(format!("{hash:016x}.err.txt")), err);
    if let Some(h) = hlsl {
        let _ = std::fs::write(dir.join(format!("{hash:016x}.hlsl")), h);
    }
}

/// On-disk cache of compiled shaders, keyed by the *generated HLSL* (plus profile): a
/// translator change that alters a shader's HLSL misses the cache, anything else — including
/// rebuilding this DLL — reuses the compiled bytecode. Compiling is the expensive part
/// (seconds for the large dispatcher-style compute shaders; translation takes milliseconds).
fn disk_cache_dir() -> Option<&'static std::path::PathBuf> {
    static DIR: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("tombox_shader_cache").join("by_hlsl");
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_ref()
}

pub fn canonical_decls(hlsl: &str) -> String {
    tombox_xshader::runtime_hlsl::canonical_decls(hlsl)
}

fn loop_grad() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| {
        let on = std::env::var("TOMBOX_LOOP_GRAD").map(|v| v != "0").unwrap_or(true);
        record("d3d12_xs", "shader_xlat", if on { "in-loop pixel samples use quad-lane gradients (TOMBOX_LOOP_GRAD=0 reverts to LOD 0)" } else { "TOMBOX_LOOP_GRAD=0: in-loop pixel samples use LOD 0 (pre-06:00 translator behaviour)" });
        on
    })
}

pub fn disable_loop_grad(hlsl: &str) -> String {
    tombox_xshader::runtime_hlsl::disable_loop_grad(hlsl)
}

fn translate_uncached(blob: &[u8], hash: u64, rtfmt: &[u8; 8], link: &InputInterp) -> Result<Vec<u8>, String> {
    translate_uncached_with(blob, hash, rtfmt, link, None, None)
}

pub(crate) fn translate_edited(blob: &[u8], hash: u64, rtfmt: &[u8; 8], edit: &dyn Fn(&str) -> Result<String, String>) -> Result<Vec<u8>, String> {
    translate_edited_linked(blob, hash, rtfmt, &InputInterp::default(), edit)
}

pub(crate) fn translate_edited_linked(blob: &[u8], hash: u64, rtfmt: &[u8; 8], link: &InputInterp, edit: &dyn Fn(&str) -> Result<String, String>) -> Result<Vec<u8>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| translate_uncached_with(blob, hash, rtfmt, link, Some(edit), None))).unwrap_or_else(|_| Err("translator panicked".to_string()))
}

pub(crate) struct RtSrc {
    pub hlsl: String,
    pub profile: String,
    pub use_dxc: bool,
}

pub(crate) fn runtime_mode() -> &'static tombox_xshader::pack_key::Mode {
    static M: OnceLock<tombox_xshader::pack_key::Mode> = OnceLock::new();
    M.get_or_init(|| {
        let xbox_one = crate::cbv_raw::xbox_one_title();
        let cbv_raw = crate::cbv_raw::pack_mode(xbox_one);
        let m = tombox_xshader::pack_key::Mode { cbv_raw, loop_grad: loop_grad(), fma: fma_mode() };
        let line = format!("{} xbox_one={}\n", m.line(), xbox_one as u8);
        let p = tombox_xshader::shader_cache::runtime_mode_path(&title_name());
        if std::fs::read_to_string(&p).ok().as_deref() != Some(line.as_str()) {
            write_atomic(&p, line.as_bytes());
        }
        record("d3d12_xs", "shader_xlat", &format!("pack key mode: {} xbox_one={} (written to {} for xshader_prelaunch)", m.line(), xbox_one as u8, p.display()));
        m
    })
}

fn prelaunch_records() -> &'static HashMap<String, String> {
    static R: OnceLock<HashMap<String, String>> = OnceLock::new();
    R.get_or_init(|| {
        let p = tombox_xshader::shader_cache::title_dir(&title_name()).join("prelaunch_keys.txt");
        std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| l.split_once(' ')).map(|(a, b)| (a.to_string(), b.to_string())).collect()
    })
}

fn log_pack_miss(hash: u64, rtfmt: &[u8; 8], linked: bool, src: &str, profile: &str, mode: &tombox_xshader::pack_key::Mode, more: &[&str]) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed) + 1;
    if n > 5 {
        return;
    }
    let ours = tombox_xshader::pack_key::KeyParts::of(src, profile, mode, more).line();
    let id = format!("{hash:016x}_{}", rtfmt_hex(rtfmt));
    let theirs = prelaunch_records().get(&id);
    let verdict = match (theirs, linked, more.is_empty()) {
        (_, true, _) => "linked variant: prelaunch only sees the unlinked dump, so it has no key for this one".to_string(),
        (_, _, false) => format!("edited variant ({}): prelaunch compiles only the plain translation", more.join(" ")),
        (None, _, _) => "prelaunch has no record of this variant (new shader, or prelaunch did not run)".to_string(),
        (Some(t), _, _) => match tombox_xshader::pack_key::KeyParts::first_difference(&ours, t) {
            Some(d) => format!("first differing component {d}"),
            None if t.split_whitespace().next() == ours.split_whitespace().next() => "same key as prelaunch; the entry is missing from the pack (prelaunch skipped or its compile failed)".to_string(),
            None => "components match but the keys differ".to_string(),
        },
    };
    record("d3d12_xs", "shader_xlat", &format!("pack miss #{n} {id}: {verdict}; runtime {ours}; prelaunch {}", theirs.map(String::as_str).unwrap_or("<none>")));
}

fn first_gather_lod_note(hash: u64) -> bool {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<u64>>> = OnceLock::new();
    SEEN.get_or_init(Default::default).lock().map(|mut s| s.insert(hash)).unwrap_or(false)
}

fn note_gather_lod_blind(hash: u64, offsets: &[u32]) {
    if offsets.is_empty() || !first_gather_lod_note(hash) {
        return;
    }
    record("d3d12_xs", "shader_xlat", &format!("GATHER4_L: hash={hash:016x} image_gather4_l at ISA {offsets:#x?} picks its footprint mip without the sampler LOD_BIAS/MIN_LOD/MAX_LOD: the S# is not visible to the translator"));
}

fn note_unknowns(hash: u64, unknowns: &[String]) {
    static SEEN: Mutex<std::collections::BTreeSet<u64>> = Mutex::new(std::collections::BTreeSet::new());
    if unknowns.is_empty() || !SEEN.lock().map_or(false, |mut s| s.insert(hash)) {
        return;
    }
    for u in unknowns {
        record("d3d12_xs", "shader_xlat", &format!("UNKNOWN hash={hash:016x}: {u}"));
    }
}

pub(crate) fn runtime_source(blob: &[u8], hash: u64, rtfmt: &[u8; 8], link: &InputInterp, root: Option<&[u8]>, mode: &tombox_xshader::pack_key::Mode) -> Result<RtSrc, String> {
    let mut ex = tombox_xshader::extract(blob).map_err(|e| {
        dump_failure(hash, blob, None, &e);
        format!("extract: {e}")
    })?;
    if let Some(root) = root.filter(|_| !ex.has_root_sig) {
        ex.root_sig = Some(tombox_xshader::parse_root_signature(root).map_err(|e| format!("pipeline root signature: {e}"))?);
    }
    let mut t = tombox_xshader::pack_key::translate(&ex, Some(link), mode.cbv_raw).map_err(|e| {
        dump_failure(hash, blob, None, &e);
        e
    })?;
    note_unknowns(hash, &t.unknowns);
    if mode.cbv_raw.rewrites() {
        if let Some((slot, lim, _)) = crate::cbv_raw::clamp_knob().filter(|k| k.2.map_or(true, |h| h == hash)) {
            t.hlsl = crate::cbv_raw::clamp_reads(&t.hlsl, slot, lim);
        }
        if crate::cbv_raw::hoist_hashes().contains(&hash) {
            let (h, n) = crate::cbv_raw::hoist_const_loads(&t.hlsl);
            t.hlsl = h;
            record("d3d12_xs", "cbv_raw", &format!("HOIST: hash={hash:016x} reads {n} constant raw constant-buffer rows once before the dispatcher loop (TOMBOX_CBRAW_HOIST)"));
        }
    }
    if mode.cbv_raw == tombox_xshader::pack_key::CbvRaw::Copy {
        crate::cbv_copy::note_shader_cb(hash, crate::cbv_copy::meta_of(&t));
    }
    crate::fmask_check::note_translation(hash, &t.hlsl, &t.fmask);
    note_gather_lod_blind(hash, &t.gather_lod_blind);
    RUNTIME_SOURCES.with(|c| c.set(c.get() + 1));
    let needs = crate::mip_bind::Needs::of(&t);
    crate::mip_bind::note(hash, &needs);
    if let Some(why) = needs.refusal() {
        dump_failure(hash, blob, None, &why);
        return Err(format!("bind: {why}"));
    }
    if let Some(why) = crate::global_bind::refusal(t.needs_global) {
        dump_failure(hash, blob, None, &why);
        return Err(format!("bind: {why}"));
    }
    if let Some(why) = typed_atomic64_refusal(t.uses_typed_atomic64, device_typed_atomic64()) {
        record("d3d12_xs", "shader_xlat", &format!("REFUSE hash={hash:016x}: {why}"));
        dump_failure(hash, blob, None, why);
        return Err(format!("bind: {why}"));
    }
    // Development aid: `%TEMP%	ombox_shader_override<hash>.hlsl` replaces the generated
    // HLSL for that shader (to instrument a translation while debugging). Logged every time.
    if let Ok(h) = std::fs::read_to_string(std::env::temp_dir().join("tombox_shader_override").join(format!("{hash:016x}.hlsl"))) {
        record("d3d12_xs", "shader_xlat", &format!("OVERRIDE: hash={hash:016x} uses hand-edited HLSL ({} bytes)", h.len()));
        t.hlsl = h;
    }
    if stub_hashes().contains(&hash) {
        if let Some(k) = t.hlsl.find(" o = (SOut)0;") {
            let e = k + t.hlsl[k..].find('\n').map_or(0, |x| x + 1);
            t.hlsl.insert_str(e, "    return o;\n");
            record("d3d12_xs", "shader_xlat", &format!("STUB: hash={hash:016x} returns its zeroed outputs (TOMBOX_XS_STUB)"));
        }
    }
    if let Some(&(_, lo, hi)) = drop_reads().iter().find(|(h, _, _)| *h == hash) {
        let (out, n) = drop_read_lines(&t.hlsl, lo, hi);
        t.hlsl = out;
        record("d3d12_xs", "shader_xlat", &format!("DROP: hash={hash:016x} dropped {n} memory reads in ISA {lo:#06x}..={hi:#06x} (TOMBOX_XS_DROP)"));
    }
    if let Some(&(_, off)) = stub_at().iter().find(|(h, _)| *h == hash) {
        let tag = format!("// {off:#06x} ");
        if let Some(k) = t.hlsl.find(&tag) {
            let ls = t.hlsl[..k].rfind('\n').map_or(0, |x| x + 1);
            t.hlsl.insert_str(ls, "    return o;\n");
            record("d3d12_xs", "shader_xlat", &format!("STUB_AT: hash={hash:016x} returns before ISA {off:#06x} (TOMBOX_XS_STUB_AT)"));
        } else {
            record("d3d12_xs", "shader_xlat", &format!("STUB_AT: hash={hash:016x} has no ISA line {off:#06x}"));
        }
    }
    t.hlsl = tombox_xshader::pack_key::finalize(&t.hlsl, rtfmt, mode);
    // Compiler: DXC (SM6 DXIL) when dxcompiler.dll + dxil.dll are available, otherwise FXC
    // (SM5 DXBC). FXC needs minutes for some of the generated compute shaders; DXC about a
    // second. `TOMBOX_SHADER_COMPILER=fxc` forces the old path.
    let sm6_only = t.profile.split('_').nth(1) == Some("6");
    let use_dxc = tombox_xshader::dxc::available()
        && (sm6_only || std::env::var("TOMBOX_SHADER_COMPILER").map(|v| v != "fxc").unwrap_or(true));
    if sm6_only && !use_dxc {
        return Err(format!("shader needs profile {} (shader model 6.x features); FXC compiles only shader model 5 and dxcompiler.dll/dxil.dll are not available", t.profile));
    }
    static ANNOUNCED: std::sync::Once = std::sync::Once::new();
    ANNOUNCED.call_once(|| {
        record("d3d12_xs", "shader_xlat", if use_dxc { "compiler: DXC (shader model 6.0, DXIL)" } else { "compiler: FXC (shader model 5, DXBC) - DXC not available or disabled" });
    });
    let profile = if use_dxc { tombox_xshader::dxc::profile_for(t.profile) } else { t.profile.to_string() };
    Ok(RtSrc { hlsl: t.hlsl, profile, use_dxc })
}

fn translate_uncached_with(blob: &[u8], hash: u64, rtfmt: &[u8; 8], link: &InputInterp, edit: Option<&dyn Fn(&str) -> Result<String, String>>, root: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let t_start = std::time::Instant::now();
    let mode = runtime_mode();
    let t = runtime_source(blob, hash, rtfmt, link, root, mode)?;
    let (profile, use_dxc) = (t.profile.clone(), t.use_dxc);
    if let Some(edit) = edit {
        if !use_dxc {
            return Err("edited translation needs DXC".to_string());
        }
        let h = edit(&t.hlsl)?;
        if !edited_pack() {
            return tombox_xshader::dxc::compile(&h, &profile);
        }
        let more = ["edited-plain-compile"];
        return tombox_xshader::shader_cache::compile_cached_with(&h, &profile, mode.fma, &tombox_xshader::pack_key::extra(mode, &more), || {
            log_pack_miss(hash, rtfmt, false, &h, &profile, mode, &more);
            tombox_xshader::dxc::compile(&h, &profile)
        })
        .map(|c| c.code);
    }
    if use_dxc {
        let xlat_ms = t_start.elapsed().as_secs_f64() * 1000.0;
        let dir = std::env::temp_dir().join("tombox_shaders");
        let inflight = dir.join(format!("inflight_{hash:016x}.dxbc"));
        let fma = mode.fma;
        let r = tombox_xshader::shader_cache::compile_cached_with(&t.hlsl, &profile, fma, &tombox_xshader::pack_key::extra(mode, &[]), || {
            log_pack_miss(hash, rtfmt, !link.0.is_empty(), &t.hlsl, &profile, mode, &[]);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&inflight, blob);
            let compiled = tombox_xshader::dxc::compile_fma(&t.hlsl, &profile, fma, &[]);
            let _ = std::fs::remove_file(&inflight);
            compiled
        });
        return match r {
            Ok(c) => {
                note_timing(xlat_ms, c.lookup_ms, c.dxc_ms, c.source == tombox_xshader::shader_cache::Source::Pack);
                if c.dxc_ms > 1500.0 {
                    record("d3d12_xs", "shader_xlat", &format!("SLOW: hash={hash:016x} took {:.0} ms to compile ({} bytes in, {} HLSL lines; translate {xlat_ms:.1} ms, fast fma {})", c.dxc_ms, blob.len(), t.hlsl.lines().count(), fma.preserve));
                    if c.dxc_ms > 10_000.0 {
                        let _ = std::fs::write(dir.join(format!("slow_{hash:016x}.dxbc")), blob);
                    }
                }
                Ok(c.code)
            }
            Err(e) => {
                dump_failure(hash, blob, Some(&t.hlsl), &e);
                Err(e)
            }
        };
    }
    let _ = disk_cache_dir();
    let cached = Some(tombox_xshader::dxc::cache_path(&t.hlsl, &profile));
    if let Some(p) = &cached {
        if let Ok(v) = std::fs::read(p) {
            if !v.is_empty() {
                return Ok(v);
            }
        }
    }
    // Leave the input on disk while it is being compiled: if FXC hangs or the title's
    // watchdog fires first, the file that remains identifies the shader.
    let dir = std::env::temp_dir().join("tombox_shaders");
    let _ = std::fs::create_dir_all(&dir);
    let inflight = dir.join(format!("inflight_{hash:016x}.dxbc"));
    let _ = std::fs::write(&inflight, blob);
    let t0 = std::time::Instant::now();
    let r = compile(&t.hlsl, &profile);
    let _ = std::fs::remove_file(&inflight);
    let ms = t0.elapsed().as_millis();
    if ms > 1500 {
        record("d3d12_xs", "shader_xlat", &format!("SLOW: hash={hash:016x} took {ms} ms to compile ({} bytes in, {} HLSL lines)", blob.len(), t.hlsl.lines().count()));
        if ms > 10_000 {
            let _ = std::fs::write(dir.join(format!("slow_{hash:016x}.dxbc")), blob);
        }
    }
    match r {
        Ok(v) => {
            if let Some(p) = &cached {
                let _ = std::fs::write(p, &v);
            }
            Ok(v)
        }
        Err(e) => {
            dump_failure(hash, blob, Some(&t.hlsl), &e);
            Err(e)
        }
    }
}

/// Translate an Xbox shader blob to PC bytecode (cached). `ptr/len` come from a
/// D3D12_SHADER_BYTECODE in the title's PSO desc.
pub unsafe fn translate(ptr: *const u8, len: usize) -> Cached {
    translate_for_targets(ptr, len, [0; 8], &no_link())
}

pub fn retranslate(blob: &Arc<Vec<u8>>, hash: u64, rtfmt: [u8; 8], link: &InputInterp) -> Result<Arc<Vec<u8>>, String> {
    let v = std::panic::catch_unwind(|| translate_uncached(blob, hash, &rtfmt, link)).unwrap_or_else(|_| Err("translator panicked".to_string()))?;
    let v = Arc::new(v);
    ORIGIN.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner()).insert(v.as_ptr() as usize, (hash, blob.clone()));
    Ok(v)
}

pub fn export_formats(num_rt: u32, formats: &[u32; 8]) -> [u8; 8] {
    static SW: crate::live_sw::Sw = crate::live_sw::Sw::new("TOMBOX_EXPORT_FMT", "export_fmt", true);
    let mut k = [0u8; 8];
    if !SW.on() {
        return k;
    }
    let ps = crate::blendclamp::ps_mode();
    for i in 0..(num_rt as usize).min(8) {
        k[i] = export_kind(formats[i], ps);
    }
    k
}

pub fn export_kind(format: u32, ps_clamp: bool) -> u8 {
    match format {
        10 | 34 | 54 => if ps_clamp { 3 } else { 1 },
        26 => if ps_clamp { 4 } else { 2 },
        _ => 0,
    }
}

pub fn cache_key(hash: u64, rtfmt: &[u8; 8]) -> u64 {
    if rtfmt.iter().any(|&k| k != 0) { hash ^ fnv1a(rtfmt).rotate_left(17) } else { hash }
}

pub fn link_key(key: u64, link: &InputInterp) -> u64 {
    if link.0.is_empty() { key } else { key ^ fnv1a(format!("{:?}", link.0).as_bytes()).rotate_left(29) }
}

pub fn ps_link(ps: &[u8], pc_centroid: bool) -> Arc<InputInterp> {
    static LINKS: LazyLock<Mutex<HashMap<u64, Arc<InputInterp>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
    let h = fnv1a(ps) ^ pc_centroid as u64;
    if let Some(l) = LINKS.lock().unwrap_or_else(|e| e.into_inner()).get(&h) {
        return l.clone();
    }
    let ex = std::panic::catch_unwind(|| tombox_xshader::extract(ps)).ok().and_then(Result::ok);
    let l = ex.map(|ex| tombox_xshader::hlsl::ps_input_interp(&ex)).unwrap_or_default();
    let l = Arc::new(if pc_centroid { l.pc_centroid() } else { l });
    LINKS.lock().unwrap_or_else(|e| e.into_inner()).insert(h, l.clone());
    l
}

pub fn no_link() -> Arc<InputInterp> {
    static NONE: LazyLock<Arc<InputInterp>> = LazyLock::new(Default::default);
    NONE.clone()
}

pub fn rtfmt_hex(rtfmt: &[u8; 8]) -> String {
    rtfmt.iter().map(|k| format!("{k:02x}")).collect()
}

pub fn parse_src_name(name: &str) -> Option<(u64, [u8; 8])> {
    let stem = name.strip_suffix(".xbox")?;
    let (h, f) = stem.split_once('_')?;
    if h.len() != 16 || f.len() != 16 {
        return None;
    }
    let hash = u64::from_str_radix(h, 16).ok()?;
    let mut rt = [0u8; 8];
    for i in 0..8 {
        rt[i] = u8::from_str_radix(&f[2 * i..2 * i + 2], 16).ok()?;
    }
    Some((hash, rt))
}

pub(crate) fn title_name_pub() -> String {
    title_name()
}

fn title_name() -> String {
    std::env::current_exe().ok().and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())).unwrap_or_else(|| "title".into())
}

fn src_dir() -> std::path::PathBuf {
    tombox_xshader::shader_cache::src_dir(&title_name())
}

fn legacy_src_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("tombox_shader_cache").join("src").join(title_name())
}

fn src_path(hash: u64, rtfmt: &[u8; 8]) -> std::path::PathBuf {
    src_dir().join(format!("{hash:016x}_{}.xbox", rtfmt_hex(rtfmt)))
}

fn fma_mode() -> tombox_xshader::shader_cache::Fma {
    static F: OnceLock<tombox_xshader::shader_cache::Fma> = OnceLock::new();
    *F.get_or_init(|| {
        let t0 = std::time::Instant::now();
        let p = tombox_xshader::fma_probe::probe();
        record("d3d12_xs", "shader_xlat", &format!(
            "fma probe ({}, {:.0} ms, {}): fast hardware fma for denorm-preserve shaders {}, for denorm-any shaders {}, hybrid fma for denorm-any shaders {}, native f16 rounding {} [{}]; cache {}",
            p.adapter, t0.elapsed().as_secs_f64() * 1000.0, if p.cached { "cached result" } else { "measured" },
            if p.fma.preserve { "ON" } else { "off (exact f64 path)" }, if p.fma.any { "ON" } else { "off (exact f64 path)" }, if p.fma.hybrid { "ON" } else { "off" }, if p.fma.f2h { "ON" } else { "off (software rounding)" }, p.detail,
            tombox_xshader::shader_cache::pack_path().display()));
        p.fma
    })
}

static T_XLAT: AtomicU64 = AtomicU64::new(0);
static T_LOOKUP: AtomicU64 = AtomicU64::new(0);
static T_DXC: AtomicU64 = AtomicU64::new(0);
static N_COMPILED: AtomicUsize = AtomicUsize::new(0);
static N_PACK: AtomicUsize = AtomicUsize::new(0);

fn note_timing(xlat_ms: f64, lookup_ms: f64, dxc_ms: f64, hit: bool) {
    T_XLAT.fetch_add((xlat_ms * 1000.0) as u64, Ordering::Relaxed);
    T_LOOKUP.fetch_add((lookup_ms * 1000.0) as u64, Ordering::Relaxed);
    T_DXC.fetch_add((dxc_ms * 1000.0) as u64, Ordering::Relaxed);
    let n = if hit { N_PACK.fetch_add(1, Ordering::Relaxed) } else { N_COMPILED.fetch_add(1, Ordering::Relaxed) } + 1;
    let total = N_PACK.load(Ordering::Relaxed) + N_COMPILED.load(Ordering::Relaxed);
    if n == 1 || total % 100 == 0 {
        record("d3d12_xs", "shader_xlat", &timing_summary());
    }
}

pub fn timing_summary() -> String {
    let st = tombox_xshader::shader_cache::stats();
    let (np, nc) = (N_PACK.load(Ordering::Relaxed), N_COMPILED.load(Ordering::Relaxed));
    let ms = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1000.0;
    format!(
        "compile totals: {} shaders, disk hits {np}, disk misses {nc} (compiled); translate {:.0} ms, cache key+load {:.0} ms, dxc {:.0} ms (avg {:.0} ms per compiled); pack {} records, {} written, {} bad crc, {} bytes truncated; caller blocked {:.0} ms in {} waits",
        np + nc, ms(&T_XLAT), ms(&T_LOOKUP), ms(&T_DXC), if nc > 0 { ms(&T_DXC) / nc as f64 } else { 0.0 },
        st.records, st.writes, st.bad_crc, st.truncated, ms(&BLOCKED_US), BLOCKED_N.load(Ordering::Relaxed))
}

static BLOCKED_US: AtomicU64 = AtomicU64::new(0);
static BLOCKED_N: AtomicUsize = AtomicUsize::new(0);

fn write_atomic(p: &std::path::Path, data: &[u8]) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, data).is_ok() && std::fs::rename(&tmp, p).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

static PREWARMED: AtomicUsize = AtomicUsize::new(0);

fn edited_pack() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        let on = std::env::var("TOMBOX_EDITED_PACK").map(|v| v != "0").unwrap_or(true);
        record("d3d12_xs", "shader_xlat", if on { "edited variants (vs_snorm, ps depth/stencil, dbcount strip) are compiled through the shader pack keyed by their final HLSL (TOMBOX_EDITED_PACK=0 compiles them every time)" } else { "TOMBOX_EDITED_PACK=0: edited variants are compiled every time" });
        on
    })
}

fn index_ctx() -> &'static str {
    static C: OnceLock<String> = OnceLock::new();
    C.get_or_init(|| {
        let f = fma_mode();
        format!("dxc={} cbv_raw={} loop_grad={} fma={}{}{}{}", tombox_xshader::dxc::available(), if crate::cbv_raw::copy_on() { "copy2".to_string() } else { crate::cbv_raw::enabled().to_string() }, loop_grad(), f.preserve as u8, f.any as u8, f.hybrid as u8, f.f2h as u8)
    })
}

thread_local! {
    pub(crate) static RUNTIME_SOURCES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const CB_META_SALT: u128 = 0x7462_6362_6d65_7461_0000_0000_0000_0001;

fn copy_meta_hit(ik: u128, hash: u64) -> bool {
    match tombox_xshader::shader_cache::get(ik ^ CB_META_SALT).as_deref().and_then(crate::cbv_copy::decode_meta) {
        Some(m) => {
            crate::cbv_copy::note_shader_cb(hash, m);
            true
        }
        None => false,
    }
}

fn produce(blob: &[u8], hash: u64, rtfmt: &[u8; 8], link: &InputInterp, _key: u64) -> Result<Vec<u8>, String> {
    let ik = crate::xlat_index::key(blob, hash, rtfmt, link, index_ctx());
    let copy = crate::cbv_raw::copy_on();
    let hit = ik.and_then(crate::xlat_index::get).filter(|_| !copy || ik.is_some_and(|k| copy_meta_hit(k, hash)));
    let r = match hit {
        Some(v) => Ok(v),
        None => {
            let t0 = std::time::Instant::now();
            let r = std::panic::catch_unwind(|| translate_uncached(blob, hash, rtfmt, link)).unwrap_or_else(|_| Err("translator panicked".to_string()));
            if let (Some(k), Ok(v)) = (ik, &r) {
                crate::xlat_index::put(k, v, t0.elapsed().as_secs_f64() * 1000.0);
                if let Some(m) = copy.then(|| crate::cbv_copy::shader_cb(hash)).flatten() {
                    let _ = tombox_xshader::shader_cache::put(k ^ CB_META_SALT, &crate::cbv_copy::encode_meta(&m));
                }
            }
            r
        }
    };
    if let Ok(v) = &r {
        crate::dbcount::note_translated(v, blob, hash, rtfmt);
        let sp = src_path(hash, rtfmt);
        if link.0.is_empty() && !GUESS.with(|g| g.get()) && !sp.exists() {
            write_atomic(&sp, blob);
        }
    }
    r
}

thread_local! {
    static GUESS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

const BG_RESERVE: u64 = 2 << 30;
const BG_PER_JOB: u64 = 256 << 20;
static BG_ACTIVE: AtomicUsize = AtomicUsize::new(0);
static BG_THROTTLED: AtomicUsize = AtomicUsize::new(0);

pub fn bg_allowed(total: u64, used: u64, cap: usize) -> usize {
    let head = total.saturating_sub(used).saturating_sub(BG_RESERVE);
    ((head / BG_PER_JOB) as usize).min(cap)
}

pub struct BgSlot;

impl Drop for BgSlot {
    fn drop(&mut self) {
        BG_ACTIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn bg_slot(cap: usize) -> BgSlot {
    let mut waited = false;
    loop {
        let allowed = match compat_common::gdk::memstatus::title_view() {
            Some(v) => bg_allowed(v.total, v.used, cap),
            None => cap,
        };
        let cur = BG_ACTIVE.load(Ordering::Relaxed);
        if cur < allowed && BG_ACTIVE.compare_exchange(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            return BgSlot;
        }
        if !waited {
            waited = true;
            let n = BG_THROTTLED.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 4 || n % 100 == 0 {
                let v = compat_common::gdk::memstatus::title_view();
                record("d3d12_xs", "shader_xlat", &format!("background shader work paused for title memory (#{n}): {} of {cap} jobs allowed, title used {} MB of {} MB, {} MB reserved, {} MB per job", allowed, v.map(|v| v.used >> 20).unwrap_or(0), v.map(|v| v.total >> 20).unwrap_or(0), BG_RESERVE >> 20, BG_PER_JOB >> 20));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn drop_if_unclaimed(key: u64, c: &Arc<OnceLock<Cached>>) {
    let Some(cache) = CACHE.get() else { return };
    let mut m = cache.lock().unwrap_or_else(|e| e.into_inner());
    if m.get(&key).is_some_and(|e| Arc::ptr_eq(e, c)) && Arc::strong_count(c) == 2 {
        m.remove(&key);
    }
}

fn cell(key: u64) -> Arc<OnceLock<Cached>> {
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    cache.lock().unwrap_or_else(|e| e.into_inner()).entry(key).or_insert_with(|| Arc::new(OnceLock::new())).clone()
}

fn mesh_root_cell(hash: u64, root: &[u8]) -> Arc<OnceLock<Cached>> {
    let cache = MESH_ROOT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    let roots = cache.entry(hash).or_default();
    if let Some(entry) = roots.get(root) {
        return entry.clone();
    }
    let entry = Arc::new(OnceLock::new());
    roots.insert(root.to_vec(), entry.clone());
    entry
}

fn finish(res: Result<Vec<u8>, String>, hash: u64, len: usize, blob: &[u8]) -> Cached {
    finish_with(res, hash, len, Some(blob))
}

fn finish_with(res: Result<Vec<u8>, String>, hash: u64, len: usize, origin_blob: Option<&[u8]>) -> Cached {
    match res {
        Ok(v) => {
            let n = OK.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 8 || n % 50 == 0 {
                record("d3d12_xs", "shader_xlat", &format!("ok #{n} hash={hash:016x} {len}B -> {}B DXBC (disk hits {} misses {} prewarmed {})", v.len(), N_PACK.load(Ordering::Relaxed), N_COMPILED.load(Ordering::Relaxed), PREWARMED.load(Ordering::Relaxed)));
            }
            let v = Arc::new(v);
            note_origin_hash(v.as_ptr() as usize, hash);
            if let Some(blob) = origin_blob.filter(|_| crate::rtcap::enabled() || crate::hot_reload::enabled()) {
                ORIGIN.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner()).insert(v.as_ptr() as usize, (hash, Arc::new(blob.to_vec())));
            }
            Ok(v)
        }
        Err(e) => {
            let n = FAIL.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 40 || n % 50 == 0 {
                let first = e.lines().next().unwrap_or("");
                record("d3d12_xs", "shader_xlat", &format!("FAIL #{n} hash={hash:016x}: {first}"));
            }
            Err(Arc::new(e))
        }
    }
}

extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn SetThreadPriority(h: *mut c_void, p: i32) -> i32;
    fn GetThreadPriority(h: *mut c_void) -> i32;
    fn GetCurrentThreadId() -> u32;
    fn OpenThread(access: u32, inherit: i32, id: u32) -> *mut c_void;
}

const PREWARM_PRIORITY: i32 = -1;
const THREAD_SET_QUERY: u32 = 0x0020 | 0x0040;
static BOOSTS: AtomicUsize = AtomicUsize::new(0);

fn prewarm_busy() -> &'static Mutex<HashMap<u64, usize>> {
    static M: OnceLock<Mutex<HashMap<u64, usize>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn boost_prewarm(key: u64) {
    let Some(h) = prewarm_busy().lock().unwrap_or_else(|e| e.into_inner()).get(&key).copied() else { return };
    let want = unsafe { GetThreadPriority(GetCurrentThread()) }.clamp(0, 2);
    unsafe { SetThreadPriority(h as *mut c_void, want) };
    let n = BOOSTS.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 8 || n % 50 == 0 {
        record("d3d12_xs", "shader_xlat", &format!("a waiting thread boosted the prewarm worker translating key {key:016x} to priority {want} (#{n})"));
    }
}

const GAME_PS_RTFMTS: [[u8; 8]; 3] = [[0x80, 0, 0, 0, 0, 0, 0, 0], [0x02, 0, 0, 0, 0, 0, 0, 0], [0x82, 0, 0, 0, 0, 0, 0, 0]];

fn is_pixel(blob: &[u8]) -> bool {
    std::panic::catch_unwind(|| tombox_xshader::extract(blob).map(|e| matches!(e.stage, tombox_xshader::Stage::Pixel)).unwrap_or(false)).unwrap_or(false)
}

fn prewarm_order<T>(known: Vec<T>, from_game: Vec<T>) -> Vec<T> {
    let mut v: Vec<T> = from_game.into_iter().rev().collect();
    v.extend(known.into_iter().rev());
    v
}

fn prewarm_workers(cores: usize, from_game: usize) -> usize {
    if from_game > 0 { cores.saturating_sub(4).max(1) } else { (cores / 3).clamp(1, 6) }
}

pub fn start_prewarm() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if std::env::var("TOMBOX_SHADER_PREWARM").map(|v| v == "0").unwrap_or(false) {
            record("d3d12_xs", "shader_xlat", "prewarm disabled (TOMBOX_SHADER_PREWARM=0)");
            return;
        }
        let _ = fma_mode();
        let dir = src_dir();
        let mut todo: Vec<(std::path::PathBuf, u64, [u8; 8])> = Vec::new();
        let mut seen: std::collections::HashSet<(u64, [u8; 8])> = std::collections::HashSet::new();
        let mut migrated = 0usize;
        let game = tombox_xshader::shader_cache::game_scan_dir(&title_name());
        let mut from_game: Vec<(std::path::PathBuf, u64, [u8; 8])> = Vec::new();
        for d in [dir.clone(), legacy_src_dir()] {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some((h, rt)) = parse_src_name(&name) else { continue };
                if !seen.insert((h, rt)) {
                    continue;
                }
                let mut p = e.path();
                if d != dir {
                    let np = dir.join(&name);
                    if let Ok(b) = std::fs::read(&p) {
                        write_atomic(&np, &b);
                        migrated += 1;
                        p = np;
                    }
                }
                todo.push((p, h, rt));
            }
        }
        let known = todo.len();
        let game_on = std::env::var("TOMBOX_GAME_SCAN_PREWARM").map(|v| v.trim() == "1").unwrap_or(false);
        if let Some(rd) = game_on.then(|| std::fs::read_dir(&game).ok()).flatten() {
            let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            for name in names {
                let Some((h, rt)) = parse_src_name(&name) else { continue };
                if seen.insert((h, rt)) {
                    from_game.push((game.join(&name), h, rt));
                }
            }
        }
        let extra = from_game.len();
        todo = prewarm_order(todo, from_game);
        record("d3d12_xs", "shader_xlat", &format!("prewarm: {known} known (shader, render-target format) variants from {} ({migrated} migrated from the old temp manifest) + {extra} not yet seen from the title's own files in {}; pack {} with {} records", dir.display(), game.display(), tombox_xshader::shader_cache::pack_path().display(), tombox_xshader::shader_cache::stats().records));
        if todo.is_empty() {
            return;
        }
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let workers = prewarm_workers(cores, extra);
        let queue = Arc::new(Mutex::new(todo));
        let seen_rt = Arc::new(Mutex::new(seen));
        for _ in 0..workers {
            let q = queue.clone();
            let seen_rt = seen_rt.clone();
            let game_dir = game.clone();
            let _ = std::thread::Builder::new().name("tombox-shader-prewarm".into()).spawn(move || {
                unsafe { SetThreadPriority(GetCurrentThread(), PREWARM_PRIORITY) };
                let me = unsafe { OpenThread(THREAD_SET_QUERY, 0, GetCurrentThreadId()) } as usize;
                let t0 = std::time::Instant::now();
                let mut done = 0usize;
                loop {
                    let next = q.lock().unwrap_or_else(|e| e.into_inner()).pop();
                    let Some((p, h, rt)) = next else { break };
                    let Ok(blob) = std::fs::read(&p) else { continue };
                    if fnv1a(&blob) != h {
                        let _ = std::fs::remove_file(&p);
                        continue;
                    }
                    if rt == [0u8; 8] && p.starts_with(&game_dir) && is_pixel(&blob) {
                        let mut g = q.lock().unwrap_or_else(|e| e.into_inner());
                        for v in GAME_PS_RTFMTS {
                            if seen_rt.lock().unwrap_or_else(|e| e.into_inner()).insert((h, v)) {
                                g.insert(0, (p.clone(), h, v));
                            }
                        }
                    }
                    let guess = p.starts_with(&game_dir);
                    let key = cache_key(h, &rt);
                    let c = cell(key);
                    let mut ran = false;
                    let slot = (c.get().is_none()).then(|| bg_slot(workers));
                    GUESS.with(|g| g.set(guess));
                    c.get_or_init(|| {
                        ran = true;
                        if me != 0 {
                            prewarm_busy().lock().unwrap_or_else(|e| e.into_inner()).insert(key, me);
                        }
                        let r = finish(produce(&blob, h, &rt, &InputInterp::default(), key), h, blob.len(), &blob);
                        if me != 0 {
                            prewarm_busy().lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
                            unsafe { SetThreadPriority(GetCurrentThread(), PREWARM_PRIORITY) };
                        }
                        r
                    });
                    GUESS.with(|g| g.set(false));
                    drop(slot);
                    if guess {
                        drop_if_unclaimed(key, &c);
                    }
                    if ran {
                        done += 1;
                        PREWARMED.fetch_add(1, Ordering::Relaxed);
                    }
                }
                record("d3d12_xs", "shader_xlat", &format!("prewarm worker finished: {done} shaders in {:.1}s", t0.elapsed().as_secs_f64()));
            });
        }
    });
}

type Job = Box<dyn FnOnce() + Send>;

fn helper_pool() -> &'static Mutex<std::sync::mpsc::Sender<Job>> {
    static POOL: OnceLock<Mutex<std::sync::mpsc::Sender<Job>>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        for _ in 0..cores.saturating_sub(2).clamp(1, 8) {
            let rx = rx.clone();
            let _ = std::thread::Builder::new().name("tombox-shader-stage".into()).spawn(move || loop {
                let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                match job {
                    Ok(j) => j(),
                    Err(_) => break,
                }
            });
        }
        Mutex::new(tx)
    })
}

pub unsafe fn prefetch(ptr: *const u8, len: usize, rtfmt: [u8; 8], link: &Arc<InputInterp>) -> bool {
    if ptr.is_null() || len < 32 || len > 0x40_0000 {
        return false;
    }
    let blob = core::slice::from_raw_parts(ptr, len);
    let hash = fnv1a(blob);
    let key = link_key(cache_key(hash, &rtfmt), link);
    let c = cell(key);
    if c.get().is_some() {
        return false;
    }
    let blob = Arc::new(blob.to_vec());
    let link = link.clone();
    let job: Job = Box::new(move || {
        c.get_or_init(|| crate::perf::timed(crate::perf::SHADER, || finish(produce(&blob, hash, &rtfmt, &link, key), hash, blob.len(), &blob)));
    });
    helper_pool().lock().unwrap_or_else(|e| e.into_inner()).send(job).is_ok()
}

pub unsafe fn translate_for_targets(ptr: *const u8, len: usize, rtfmt: [u8; 8], link: &InputInterp) -> Cached {
    if ptr.is_null() || len < 32 || len > 0x40_0000 {
        return Err(Arc::new(format!("bad shader blob ({ptr:p}, {len})")));
    }
    start_prewarm();
    let blob = core::slice::from_raw_parts(ptr, len);
    let hash = fnv1a(blob);
    crate::shader_dump::note(blob, hash, &rtfmt, None);
    let fmt_key = rtfmt.iter().any(|&k| k != 0);
    let key = link_key(cache_key(hash, &rtfmt), link);
    let c = cell(key);
    if let Some(hit) = c.get() {
        return hit.clone();
    }
    if fmt_key {
        static FMTN: AtomicUsize = AtomicUsize::new(0);
        if FMTN.fetch_add(1, Ordering::Relaxed) < 12 {
            record("d3d12_xs", "shader_xlat", &format!("export-format PS hash={hash:016x} rtfmt={rtfmt:?} (1=fp16 RTZ, 2=r11g11b10)"));
        }
    }
    if !link.0.is_empty() {
        static LINKN: AtomicUsize = AtomicUsize::new(0);
        if LINKN.fetch_add(1, Ordering::Relaxed) < 12 {
            record("d3d12_xs", "shader_xlat", &format!("hash={hash:016x} feeds a pixel shader with interpolation modifiers {:?}", link.0));
        }
    }
    let t0 = std::time::Instant::now();
    boost_prewarm(key);
    let entry = c.get_or_init(|| crate::perf::timed(crate::perf::SHADER, || finish(produce(blob, hash, &rtfmt, link, key), hash, len, blob))).clone();
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    if ms > 1.0 {
        BLOCKED_US.fetch_add((ms * 1000.0) as u64, Ordering::Relaxed);
        BLOCKED_N.fetch_add(1, Ordering::Relaxed);
    }
    if ms > 50.0 {
        static SL: AtomicUsize = AtomicUsize::new(0);
        if crate::slowcall::should_log(SL.fetch_add(1, Ordering::Relaxed) as u64 + 1) {
            record("d3d12_xs", "shader_xlat", &format!("caller thread blocked {ms:.0} ms on shader {hash:016x} ({len}B)"));
        }
    }
    entry
}

pub fn needs_external_root_data(blob: &[u8]) -> bool {
    (32..=0x40_0000).contains(&blob.len())
        && tombox_xshader::Dxbc::parse(blob).is_ok_and(|dxbc| dxbc.chunk(b"RTS0").is_none())
}

pub fn translate_mesh_with_root(blob: &[u8], root: &[u8], link: &InputInterp) -> Cached {
    let hash = fnv1a(blob);
    crate::shader_dump::note(blob, hash, &[0; 8], Some(root));
    let entry = mesh_root_cell(link_key(hash, link), root);
    entry.get_or_init(|| {
        crate::perf::timed(crate::perf::SHADER, || {
            let result = std::panic::catch_unwind(|| translate_uncached_with(blob, hash, &[0; 8], link, None, Some(root)))
                .unwrap_or_else(|_| Err("translator panicked".to_string()));
            finish_with(result, hash, blob.len(), None)
        })
    }).clone()
}

pub fn counts() -> (usize, usize) {
    (OK.load(Ordering::Relaxed), FAIL.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_lod_blind_note_is_once_per_shader_hash() {
        let h = 0x6a7e_4000_0000_0001u64;
        assert!(first_gather_lod_note(h));
        assert!(!first_gather_lod_note(h));
        assert!(first_gather_lod_note(h + 1));
        note_gather_lod_blind(h + 2, &[]);
        assert!(first_gather_lod_note(h + 2));
    }

    #[test]
    fn background_jobs_follow_title_memory_headroom() {
        let gb = 1u64 << 30;
        assert_eq!(bg_allowed(14 * gb, 4 * gb, 14), 14);
        assert_eq!(bg_allowed(14 * gb, 11 * gb, 14), 4);
        assert_eq!(bg_allowed(14 * gb, 12 * gb, 14), 0);
        assert_eq!(bg_allowed(14 * gb, 20 * gb, 14), 0);
        assert_eq!(bg_allowed(14 * gb, 0, 3), 3);
    }

    #[test]
    fn guessed_variants_are_not_promoted_and_unclaimed_cells_are_dropped() {
        GUESS.with(|g| g.set(true));
        assert!(GUESS.with(|g| g.get()));
        GUESS.with(|g| g.set(false));
        let key = 0x7b5e_0000_0000_0001u64;
        let c = cell(key);
        let _ = c.set(Err(Arc::new("x".to_string())));
        drop_if_unclaimed(key, &c);
        assert!(!Arc::ptr_eq(&cell(key), &c));
        let held = cell(key);
        let c2 = cell(key);
        drop_if_unclaimed(key, &c2);
        assert!(Arc::ptr_eq(&cell(key), &held));
    }

    #[test]
    fn prewarm_takes_known_variants_before_game_scan_ones_and_widens_only_for_a_game_scan() {
        let mut q = prewarm_order(vec!["k1", "k2"], vec!["g1", "g2", "g3"]);
        let mut popped = Vec::new();
        while let Some(x) = q.pop() {
            popped.push(x);
        }
        assert_eq!(popped, vec!["k1", "k2", "g1", "g2", "g3"]);
        assert_eq!(prewarm_workers(18, 0), 6);
        assert_eq!(prewarm_workers(18, 2741), 14);
        assert_eq!(prewarm_workers(4, 10), 1);
        assert_eq!(prewarm_workers(2, 0), 1);
    }

    fn key_fixtures() -> Vec<(String, Vec<u8>)> {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/shaders");
        ["xdefiant_vs_swappc_s4.dxbc", "xdefiant_ps_frontface_quad.dxbc", "fortnite_vs_sample.dxbc", "fortnite_ps_sample.dxbc", "fortnite_cs_clear.dxbc"].iter().filter_map(|n| std::fs::read(base.join(n)).ok().map(|b| (n.to_string(), b))).collect()
    }

    #[test]
    fn prelaunch_key_equals_runtime_key_for_the_same_variant() {
        use tombox_xshader::pack_key::{self, CbvRaw, Mode};
        let fx = key_fixtures();
        if fx.is_empty() {
            assert!(!std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/shaders").is_dir(), "analysis/shaders exists but holds none of the key fixtures");
            return;
        }
        let fma = tombox_xshader::shader_cache::Fma { preserve: true, any: false, hybrid: true, f2h: true };
        let mut compared = 0;
        for cbv in [CbvRaw::Off, CbvRaw::Forced, CbvRaw::On, CbvRaw::Copy] {
            for loop_grad in [true, false] {
                let mode = Mode { cbv_raw: cbv, loop_grad, fma };
                for (name, blob) in &fx {
                    for rt in [[0u8; 8], [1, 0, 0, 0, 0, 0, 0, 0]] {
                        let rtm = runtime_source(blob, fnv1a(blob), &rt, &InputInterp::default(), None, &mode);
                        let pre = pack_key::prelaunch(blob, None, &rt, None, &mode);
                        match (rtm, pre) {
                            (Ok(r), Ok(p)) => {
                                assert!(r.use_dxc || !tombox_xshader::dxc::available());
                                if !r.use_dxc {
                                    continue;
                                }
                                assert_eq!(r.profile, p.profile, "{name} {cbv:?}");
                                assert_eq!(pack_key::key(&r.hlsl, &r.profile, &mode, &[]), p.parts.key, "{name} {cbv:?} loop_grad {loop_grad}: {}", pack_key::KeyParts::first_difference(&pack_key::KeyParts::of(&r.hlsl, &r.profile, &mode, &[]).line(), &p.parts.line()).unwrap_or_default());
                                compared += 1;
                            }
                            (Err(a), Err((_, b))) => assert_eq!(a, b, "{name} {cbv:?}"),
                            (a, b) => panic!("{name} {cbv:?}: runtime {:?} vs prelaunch {:?}", a.map(|r| r.profile), b.map(|p| p.profile)),
                        }
                    }
                }
            }
        }
        eprintln!("compared {compared} runtime and prelaunch keys");
        if tombox_xshader::dxc::available() {
            assert!(compared > 0);
        }
    }

    #[test]
    fn runtime_cbv_mode_matches_the_prelaunch_decision() {
        use tombox_xshader::pack_key::CbvRaw;
        for setting in [None, Some(""), Some("auto"), Some("1"), Some("on"), Some("0"), Some("off"), Some("junk"), Some("copy")] {
            for xbox_one in [false, true] {
                let mode = crate::cbv_raw::parse_mode(setting).unwrap_or(crate::cbv_raw::Mode::Copy);
                let enabled = match mode {
                    crate::cbv_raw::Mode::Auto => xbox_one,
                    crate::cbv_raw::Mode::On => true,
                    crate::cbv_raw::Mode::Off | crate::cbv_raw::Mode::Copy => false,
                };
                assert_eq!(crate::cbv_raw::pack_mode_of(mode, enabled, xbox_one), CbvRaw::decide(setting, xbox_one), "{setting:?} xbox_one {xbox_one}");
                assert_eq!(mode == crate::cbv_raw::Mode::Copy, CbvRaw::decide(setting, xbox_one) == CbvRaw::Copy, "{setting:?}");
                let refuses = crate::cbv_raw::shader_kind_refusal(xbox_one, false).is_some();
                assert_eq!(enabled && refuses, CbvRaw::of(enabled, xbox_one) == CbvRaw::On, "{setting:?} xbox_one {xbox_one}");
            }
        }
    }

    fn poison<T: Send + 'static>(m: &'static Mutex<T>) {
        let _ = std::thread::spawn(move || {
            let _g = m.lock();
            panic!("poison");
        })
        .join();
        assert!(m.is_poisoned());
    }

    #[test]
    fn poisoned_shader_globals_recover_instead_of_panicking() {
        poison(ORIGIN.get_or_init(|| Mutex::new(HashMap::new())));
        poison(CACHE.get_or_init(|| Mutex::new(HashMap::new())));
        poison(MESH_ROOT_CACHE.get_or_init(|| Mutex::new(HashMap::new())));
        let out = finish(Ok(vec![4, 3, 2, 1]), 0x0dd0_0000_0000_0109, 4, &[1, 2, 3, 4]).unwrap();
        assert_eq!(origin_hash(out.as_ptr() as usize), Some(0x0dd0_0000_0000_0109));
        assert!(origin(1).is_none());
        let a = cell(0x0109);
        assert!(Arc::ptr_eq(&a, &cell(0x0109)));
        let r = mesh_root_cell(0x0109, &[9]);
        assert!(Arc::ptr_eq(&r, &mesh_root_cell(0x0109, &[9])));
    }

    #[test]
    fn mesh_pipeline_root_cache_separates_layouts_and_keeps_embedded_roots() {
        let body = [0; 24];
        let embedded = crate::device_wrap::wrap_root_signature(&body);
        assert!(!needs_external_root_data(&embedded));
        let mut stripped = embedded[..32].to_vec();
        stripped[24..28].copy_from_slice(&32u32.to_le_bytes());
        stripped[28..32].fill(0);
        assert!(needs_external_root_data(&stripped));
        assert!(!needs_external_root_data(&stripped[..31]));
        let hash = fnv1a(&stripped);
        let root_a = mesh_root_cell(hash, &[1, 2, 3]);
        let root_b = mesh_root_cell(hash, &[1, 2, 4]);
        assert!(Arc::ptr_eq(&root_a, &mesh_root_cell(hash, &[1, 2, 3])));
        assert!(!Arc::ptr_eq(&root_a, &root_b));
        assert!(!Arc::ptr_eq(&root_a, &cell(cache_key(hash, &[0; 8]))));
    }

    #[test]
    fn mesh_root_translation_is_not_recorded_as_a_replayable_origin() {
        let out = finish_with(Ok(vec![5, 6, 7, 8]), 0x0bad_cafe_0000_0001, 4, None).unwrap();
        assert!(ORIGIN.get().map_or(true, |m| !m.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&(out.as_ptr() as usize))));
    }

    #[test]
    fn fxc_refuses_sm6_profiles() {
        for p in ["ps_6_7", "ps_6_8", "cs_6_8"] {
            let e = compile("float4 main() : SV_Target { return 0; }", p).unwrap_err();
            assert!(e.contains("needs DXC"), "{p}: {e}");
        }
    }

    #[test]
    fn prefetch_rejects_bad_blobs_and_known_shaders() {
        assert!(!unsafe { prefetch(core::ptr::null(), 64, [0; 8], &no_link()) });
        let blob = vec![7u8; 16];
        assert!(!unsafe { prefetch(blob.as_ptr(), blob.len(), [0; 8], &no_link()) });
        let known = vec![0x5au8; 64];
        let key = cache_key(fnv1a(&known), &[0; 8]);
        let _ = cell(key).set(Ok(Arc::new(vec![1])));
        assert!(!unsafe { prefetch(known.as_ptr(), known.len(), [0; 8], &no_link()) });
        let sent = std::sync::mpsc::channel::<u8>();
        let job: Job = Box::new(move || { let _ = sent.0.send(9); });
        assert!(helper_pool().lock().unwrap_or_else(|e| e.into_inner()).send(job).is_ok());
        assert_eq!(sent.1.recv_timeout(std::time::Duration::from_secs(5)), Ok(9));
    }

    #[test]
    fn translated_code_keeps_console_hash_for_profiler() {
        let out = finish(Ok(vec![1, 2, 3, 4]), 0x1234_5678_9abc_def0, 4, &[9, 9, 9, 9]).unwrap();
        assert_eq!(origin_hash(out.as_ptr() as usize), Some(0x1234_5678_9abc_def0));
        assert_eq!(origin_hash(1), None);
    }

    #[test]
    fn offline_precompile_uses_the_runtime_hlsl_steps() {
        assert_eq!(tombox_xshader::runtime_hlsl::RTFMT_PC_CENTROID, crate::centroid_bind::RTFMT_PC_CENTROID);
        for rt in [[0u8; 8], [0x80, 0, 0, 0, 0, 0, 0, 0], [0x81, 2, 0, 1, 0, 0, 0, 0]] {
            assert_eq!(tombox_xshader::runtime_hlsl::pc_define(&rt), crate::centroid_bind::pc_define(&rt));
        }
    }

    #[test]
    fn src_names_round_trip() {
        let rt = [1u8, 0, 2, 0, 0, 0, 0, 0];
        let name = format!("{:016x}_{}.xbox", 0xdead_beef_0123_4567u64, rtfmt_hex(&rt));
        assert_eq!(parse_src_name(&name), Some((0xdead_beef_0123_4567, rt)));
        assert_eq!(parse_src_name("x_y.xbox"), None);
        assert_eq!(parse_src_name(&name.replace(".xbox", ".bin")), None);
        assert_eq!(canonical_decls("a\n    bool tb_c395 = false;\n    bool tb_c72 = false;\nb"), "a\n    bool tb_c72 = false;\n    bool tb_c395 = false;\nb");
        assert_eq!(disable_loop_grad("#if defined(__SHADER_TARGET_MAJOR) && __SHADER_TARGET_MAJOR >= 6\n    #if defined(__SHADER_TARGET_MAJOR) && __SHADER_TARGET_MAJOR >= 6\nx"), "#if defined(__SHADER_TARGET_MAJOR) && __SHADER_TARGET_MAJOR >= 6\n    #if 0\nx");
        assert_eq!(cache_key(5, &[0; 8]), 5);
        assert_ne!(cache_key(5, &rt), 5);
    }

    #[test]
    fn feeding_stage_key_follows_the_pixel_shader_modifiers() {
        let centroid = InputInterp(vec![("TEXCOORD1_centroid0".into(), "centroid".into())]);
        let other = InputInterp(vec![("TEXCOORD2_centroid0".into(), "centroid".into())]);
        let key = cache_key(5, &[0; 8]);
        assert_eq!(link_key(key, &no_link()), key);
        assert_ne!(link_key(key, &centroid), key);
        assert_ne!(link_key(key, &centroid), link_key(key, &other));
        assert_eq!(link_key(key, &centroid), link_key(key, &centroid.clone()));
        let junk = vec![0x11u8; 64];
        assert!(ps_link(&junk, false).0.is_empty());
        assert!(Arc::ptr_eq(&ps_link(&junk, true), &ps_link(&junk, true)));
        assert_eq!(InputInterp(vec![("T0".into(), "TB_CEN_Q".into())]).pc_centroid().0[0].1, "centroid");
    }

    #[test]
    fn typed_atomic64_gate_refuses_only_when_device_lacks_support() {
        assert!(typed_atomic64_refusal(true, Some(false)).is_some_and(|s| s.contains("AtomicInt64OnTypedResourceSupported")));
        assert_eq!(typed_atomic64_refusal(true, Some(true)), None);
        assert_eq!(typed_atomic64_refusal(true, None), None);
        assert_eq!(typed_atomic64_refusal(false, Some(false)), None);
        assert_eq!(typed_atomic64_refusal(false, Some(true)), None);

        crate::enable_experimental_shader_models();
        let create = crate::host_proc(b"D3D12CreateDevice\0");
        assert!(create != 0, "d3d12.dll missing");
        let f: unsafe extern "system" fn(*mut c_void, u32, *const crate::device_wrap::Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(create) };
        let iid = crate::device_wrap::Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
        let mut dev: *mut c_void = core::ptr::null_mut();
        let hr = unsafe { f(core::ptr::null_mut(), 0xc000, &iid, &mut dev) };
        assert!(hr >= 0 && !dev.is_null(), "D3D12CreateDevice hr={hr:#010x}");
        let (ok, hr9, flag, sm) = unsafe { query_typed_atomic64(dev) }.expect("a D3D12 device answers IID_ID3D12Device");
        assert!(hr9 >= 0, "CheckFeatureSupport(OPTIONS9) hr={hr9:#010x}");
        assert_eq!(ok, flag != 0 && sm >= SHADER_MODEL_6_6);
        let rel: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*(*(dev as *const *const usize)).add(2)) };
        unsafe { rel(dev) };
    }

    static SHORT_QI: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "system" fn short_qi_refuse(_t: *mut c_void, _g: *const crate::device_wrap::Guid, _p: *mut *mut c_void) -> i32 {
        SHORT_QI.fetch_add(1, Ordering::SeqCst);
        0x8000_4002u32 as i32
    }
    unsafe extern "system" fn short_qi_null(_t: *mut c_void, _g: *const crate::device_wrap::Guid, p: *mut *mut c_void) -> i32 {
        SHORT_QI.fetch_add(1, Ordering::SeqCst);
        unsafe { *p = core::ptr::null_mut() };
        0
    }
    unsafe extern "system" fn short_ref(_t: *mut c_void) -> u32 {
        1
    }

    #[test]
    fn typed_atomic64_probe_skips_objects_that_are_not_d3d12_devices() {
        for qi in [short_qi_refuse as usize, short_qi_null as usize] {
            let vt: &'static [usize; 3] = Box::leak(Box::new([qi, short_ref as usize, short_ref as usize]));
            let p = Box::leak(Box::new(vt.as_ptr())) as *mut *const usize as *mut c_void;
            let before = SHORT_QI.load(Ordering::SeqCst);
            assert_eq!(unsafe { query_typed_atomic64(p) }, None);
            unsafe { probe_typed_atomic64(p) };
            let w = crate::device_wrap::wrap_device(p);
            assert!(!w.is_null());
            assert_eq!(SHORT_QI.load(Ordering::SeqCst) - before, 3);
        }
        assert_eq!(unsafe { query_typed_atomic64(core::ptr::null_mut()) }, None);
    }
}

fn stub_hashes() -> &'static Vec<u64> {
    static S: OnceLock<Vec<u64>> = OnceLock::new();
    S.get_or_init(|| std::env::var("TOMBOX_XS_STUB").map(|v| v.split(',').filter_map(|h| u64::from_str_radix(h.trim(), 16).ok()).collect()).unwrap_or_default())
}

fn stub_at() -> &'static Vec<(u64, u32)> {
    static S: OnceLock<Vec<(u64, u32)>> = OnceLock::new();
    S.get_or_init(|| {
        std::env::var("TOMBOX_XS_STUB_AT")
            .map(|v| {
                v.split(',')
                    .filter_map(|e| {
                        let (h, o) = e.trim().split_once(':')?;
                        Some((u64::from_str_radix(h, 16).ok()?, u32::from_str_radix(o.trim_start_matches("0x"), 16).ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn drop_reads() -> &'static Vec<(u64, u32, u32)> {
    static S: OnceLock<Vec<(u64, u32, u32)>> = OnceLock::new();
    S.get_or_init(|| {
        std::env::var("TOMBOX_XS_DROP")
            .map(|v| {
                v.split(',')
                    .filter_map(|e| {
                        let (h, r) = e.trim().split_once(':')?;
                        let (a, b) = r.split_once('-')?;
                        let p = |s: &str| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok();
                        Some((u64::from_str_radix(h, 16).ok()?, p(a)?, p(b)?))
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

pub fn drop_read_lines(hlsl: &str, lo: u32, hi: u32) -> (String, usize) {
    let mut out = String::with_capacity(hlsl.len());
    let mut dropping = false;
    let mut n = 0;
    for l in hlsl.split_inclusive('\n') {
        let s = l.trim_start();
        if let Some(rest) = s.strip_prefix("// 0x") {
            let mut it = rest.split_whitespace();
            let off = it.next().and_then(|o| u32::from_str_radix(o, 16).ok());
            let m = it.next().unwrap_or("");
            dropping = off.is_some_and(|o| o >= lo && o <= hi) && drop_ops().iter().any(|p| m.starts_with(p.as_str()));
            if dropping {
                n += 1;
            }
            out.push_str(l);
            continue;
        }
        let keep = !dropping || s.starts_with('#') || s.contains("tb_pc") || s.contains("tb_sel") || s.trim_end().chars().all(|c| c == '}' || c == '{' || c == ' ');
        if keep {
            out.push_str(l);
        }
    }
    (out, n)
}

fn drop_ops() -> &'static Vec<String> {
    static S: OnceLock<Vec<String>> = OnceLock::new();
    S.get_or_init(|| std::env::var("TOMBOX_XS_DROP_OPS").unwrap_or_else(|_| "image_,buffer_,tbuffer_".into()).split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
}
