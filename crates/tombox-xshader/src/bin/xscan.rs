//! xscan: run the extractor + RDNA2→HLSL translator over every Xbox shader container found
//! in a file (e.g. a .pak holding a ShaderArchive) and report coverage.
//!
//! Usage: xscan <file> [--examples <dir>] [--limit <n>] [--jobs <n>] [--compile 1]
//!
//! `--compile 1` also compiles every translated shader (DXC by default, `--compiler fxc` for
//! fxc.exe; 4 jobs at below-normal priority unless `--jobs` is given) and stores the
//! bytecode in the runtime shader cache, so a full sweep both proves the HLSL compiles and
//! pre-warms the cache the game reads.
//!
//! Finds `DXBC` containers by magic + validated header, de-duplicates by content hash,
//! translates each, and prints per-stage totals plus a histogram of failure reasons
//! (offsets normalised). With --examples, one sample blob per distinct failure is saved.

use std::collections::{BTreeMap, HashSet};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tombox_xshader::{extract, hlsl, Stage};

fn fnv1a(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h ^= x as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Strip run-specific details (offsets, register numbers) so failures group by cause.
fn normalise(e: &str) -> String {
    let mut out = String::new();
    let mut chars = e.chars().peekable();
    let mut in_angle = false;
    while let Some(c) = chars.next() {
        // Keep opcode numbers inside <...> (e.g. vop3<0x2ff>) — they identify the gap.
        if c == '<' {
            in_angle = true;
        } else if c == '>' {
            in_angle = false;
        }
        if in_angle {
            out.push(c);
            continue;
        }
        if c == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
                chars.next();
            }
            out.push('#');
        } else if c.is_ascii_digit() {
            while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                chars.next();
            }
            out.push('#');
        } else {
            out.push(c);
        }
    }
    out
}

/// Minimal LZ4 *block* decompressor (bounds-checked; returns None on malformed input).
fn lz4_block(src: &[u8], out_len: usize) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    let mut i = 0usize;
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            loop {
                let b = *src.get(i)?;
                i += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        out.extend_from_slice(src.get(i..i + lit)?);
        i += lit;
        if i >= src.len() {
            break; // last sequence has only literals
        }
        let off = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        let mut mlen = (token & 15) as usize;
        if mlen == 15 {
            loop {
                let b = *src.get(i)?;
                i += 1;
                mlen += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        mlen += 4;
        if off == 0 || off > out.len() {
            return None;
        }
        let start = out.len() - off;
        for k in 0..mlen {
            let b = out[start + k];
            out.push(b);
        }
        if out.len() > out_len {
            return None;
        }
    }
    if out.len() == out_len { Some(out) } else { None }
}

/// A UE `FSerializedShaderArchive` shader-entry table located inside a larger file.
struct Archive {
    /// (absolute file offset, compressed size, uncompressed size)
    entries: Vec<(usize, usize, usize)>,
}

/// Locate UE shader-archive entry tables structurally: a `TArray<FShaderCodeEntry>`
/// (i32 count, then 17-byte records {u64 Offset, u32 Size, u32 UncompressedSize, u8
/// Frequency}) whose offsets start at 0 and chain (Offset[i+1] == Offset[i] + Size[i]),
/// followed by `TArray<FFileCachePreloadEntry>` (16-byte) and `TArray<u32>` ShaderIndices,
/// after which the shader code begins.
fn find_archives(data: &[u8]) -> Vec<Archive> {
    let rd32 = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let rd64 = |o: usize| data.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()) as usize);
    let mut out = Vec::new();
    let mut p = 4usize;
    while p + 17 * 64 < data.len() {
        // Cheap prefilter: first record has Offset == 0 and a small Frequency byte.
        if data[p..p + 8] != [0u8; 8] || data[p + 16] > 8 {
            p += 1;
            continue;
        }
        let n = match rd32(p - 4) {
            Some(n) if (32..4_000_000).contains(&n) && p + n * 17 + 8 <= data.len() => n,
            _ => {
                p += 1;
                continue;
            }
        };
        let mut ok = true;
        let mut expect = 0usize;
        let mut recs = Vec::with_capacity(n);
        for k in 0..n {
            let r = p + k * 17;
            let (off, size, usize_) = (rd64(r).unwrap(), rd32(r + 8).unwrap(), rd32(r + 12).unwrap());
            if off != expect || size == 0 || size > usize_ || usize_ > 0x40_0000 || data[r + 16] > 8 {
                ok = false;
                break;
            }
            expect = off + size;
            recs.push((off, size, usize_));
        }
        if !ok {
            p += 1;
            continue;
        }
        let mut q = p + n * 17;
        let npre = match rd32(q) {
            Some(v) if v < 50_000_000 => v,
            _ => {
                p += 1;
                continue;
            }
        };
        q += 4 + npre * 16;
        let nidx = match rd32(q) {
            Some(v) if v < 200_000_000 => v,
            _ => {
                p += 1;
                continue;
            }
        };
        q += 4 + nidx * 4;
        if q + expect > data.len() {
            p += 1;
            continue;
        }
        eprintln!("shader archive: {n} entries, table @ {p:#x}, code @ {q:#x} ({expect} bytes)");
        out.push(Archive { entries: recs.into_iter().map(|(o, s, u)| (q + o, s, u)).collect() });
        p = q + expect;
    }
    out
}

// ---- optional FXC compile (Windows) -------------------------------------------------------------

#[cfg(windows)]
mod fxc {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    extern "system" {
        fn LoadLibraryA(n: *const u8) -> *mut c_void;
        fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
    }
    type D3DCompileFn = unsafe extern "system" fn(
        *const c_void, usize, *const u8, *const c_void, *mut c_void, *const u8, *const u8, u32, u32,
        *mut *mut c_void, *mut *mut c_void,
    ) -> i32;
    fn func() -> Option<D3DCompileFn> {
        static F: OnceLock<usize> = OnceLock::new();
        let p = *F.get_or_init(|| unsafe {
            let h = LoadLibraryA(b"d3dcompiler_47.dll\0".as_ptr());
            if h.is_null() { 0 } else { GetProcAddress(h, b"D3DCompile\0".as_ptr()) as usize }
        });
        if p == 0 { None } else { Some(unsafe { std::mem::transmute::<usize, D3DCompileFn>(p) }) }
    }
    unsafe fn take(blob: *mut c_void) -> Vec<u8> {
        if blob.is_null() {
            return Vec::new();
        }
        let vt = *(blob as *const *const usize);
        let ptr: unsafe extern "system" fn(*mut c_void) -> *const u8 = std::mem::transmute(*vt.add(3));
        let len: unsafe extern "system" fn(*mut c_void) -> usize = std::mem::transmute(*vt.add(4));
        let rel: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vt.add(2));
        let v = std::slice::from_raw_parts(ptr(blob), len(blob)).to_vec();
        rel(blob);
        v
    }
    pub fn compile(hlsl: &str, profile: &str) -> Result<Vec<u8>, String> {
        let f = func().ok_or("d3dcompiler_47.dll unavailable")?;
        let target = format!("{profile}\0");
        let (mut code, mut errs) = (std::ptr::null_mut(), std::ptr::null_mut());
        let hr = unsafe {
            f(hlsl.as_ptr() as *const c_void, hlsl.len(), b"tombox\0".as_ptr(), std::ptr::null(), std::ptr::null_mut(),
                b"main\0".as_ptr(), target.as_ptr(), 0, 0, &mut code, &mut errs)
        };
        let err = String::from_utf8_lossy(&unsafe { take(errs) }).into_owned();
        let bytes = unsafe { take(code) };
        if hr < 0 || bytes.is_empty() {
            // First "error" line is the cause; warnings precede it.
            let line = err.lines().find(|l| l.contains("error")).unwrap_or(err.lines().next().unwrap_or("")).to_string();
            let msg = line.split("): ").nth(1).unwrap_or(&line).to_string();
            return Err(format!("fxc: {msg}"));
        }
        Ok(bytes)
    }
}
#[cfg(not(windows))]
mod fxc {
    pub fn compile(_hlsl: &str, _profile: &str) -> Result<Vec<u8>, String> {
        Err("fxc: compile mode needs Windows".into())
    }
}

/// Compile with `fxc.exe` in a child process that is killed after `limit` seconds: an
/// in-process D3DCompile cannot be cancelled, and some generated shaders take minutes.
/// Returns Ok(seconds) or Err(reason).
fn fxc_child(hlsl: &str, profile: &str, out: &std::path::Path, limit: u64) -> Result<f64, String> {
    use std::process::{Command, Stdio};
    static FXC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let exe = FXC.get_or_init(|| {
        std::env::var("FXC").unwrap_or_else(|_| r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\fxc.exe".to_string())
    });
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join("tombox_sweep_tmp");
    let _ = std::fs::create_dir_all(&dir);
    let id = N.fetch_add(1, Ordering::Relaxed);
    let src = dir.join(format!("{}_{id}.hlsl", std::process::id()));
    let tmp_out = dir.join(format!("{}_{id}.cso", std::process::id()));
    std::fs::write(&src, hlsl).map_err(|e| format!("fxc: cannot write temp file: {e}"))?;
    let t0 = std::time::Instant::now();
    let mut child = Command::new(exe)
        .args(["/nologo", "/T", profile, "/E", "main", "/Fo"])
        .arg(&tmp_out)
        .arg(&src)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("fxc: cannot start {exe}: {e}"))?;
    // Drain stderr on a helper thread so a chatty compiler cannot block on a full pipe.
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = std::io::Read::read_to_string(e, &mut s);
        }
        s
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if t0.elapsed().as_secs() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => return Err(format!("fxc: wait failed: {e}")),
        }
    };
    let err = reader.join().unwrap_or_default();
    let secs = t0.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(&src);
    let res = match status {
        None => Err(format!("fxc: timeout after {limit} s")),
        Some(_) if tmp_out.exists() => std::fs::rename(&tmp_out, out).map(|_| secs).map_err(|e| format!("fxc: cannot store result: {e}")),
        Some(_) => {
            let line = err.lines().find(|l| l.contains("error")).unwrap_or(err.lines().next().unwrap_or("no output")).to_string();
            Err(format!("fxc: {}", line.split("): ").nth(1).unwrap_or(&line)))
        }
    };
    let _ = std::fs::remove_file(&tmp_out);
    res
}

/// Same key and directory as the runtime's compiled-shader cache (d3d12_xs/shader_xlat.rs),
/// so a compile sweep pre-warms the cache the game uses.
fn cache_path(hlsl: &str, profile: &str) -> std::path::PathBuf {
    let key = fnv1a(hlsl.as_bytes()) ^ fnv1a(profile.as_bytes()).rotate_left(17);
    std::env::temp_dir().join("tombox_shader_cache").join("by_hlsl").join(format!("{key:016x}.cso"))
}

fn stage_name(s: &Stage) -> &'static str {
    match s {
        Stage::Vertex => "VS",
        Stage::Pixel => "PS",
        Stage::Compute => "CS",
        Stage::Geometry => "GS",
        Stage::Hull => "HS",
        Stage::Domain => "DS",
        Stage::Mesh => "MS",
        Stage::Amplification => "AS",
        Stage::Unknown(_) => "??",
    }
}

/// Slowest compiles seen in this run: (seconds, hash, HLSL lines).
static SLOW: Mutex<Vec<(f64, u64, usize)>> = Mutex::new(Vec::new());
static LIMIT: AtomicUsize = AtomicUsize::new(120);
/// 1 = compile with fxc.exe (SM5 DXBC) instead of the default DXC (SM6 DXIL).
static USE_FXC: AtomicUsize = AtomicUsize::new(0);

/// `--mnemonics 1`: also print, per instruction mnemonic, how many shaders use it and how
/// many of those translate.
static MNEM: AtomicUsize = AtomicUsize::new(0);
static MNEM_USE: std::sync::Mutex<std::collections::BTreeMap<String, (u64, u64)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

static FSSM_ROOT: AtomicUsize = AtomicUsize::new(0);
static FSSM_CHECK: AtomicUsize = AtomicUsize::new(0);
static FSSM_TALLY: Mutex<BTreeMap<String, (usize, u64)>> = Mutex::new(BTreeMap::new());

fn fssm_check(h: u64, st: &str, ex: &tombox_xshader::ShaderExtract, tr: &Result<hlsl::Translated, String>) {
    let Some(rs) = ex.root_sig.as_ref() else { return };
    let (class, detail) = match tombox_xshader::fssm_bind::parse(&ex.fssm) {
        Err(e) => (format!("table unreadable: {}", normalise(&e)), e),
        Ok(None) => ("no table".to_string(), String::new()),
        Ok(Some(b)) => {
            let d = b.disagreements(rs);
            let mut bare = ex.clone();
            bare.root_sig = None;
            bare.has_root_sig = false;
            let mine = hlsl::translate_with_fssm_root(&bare);
            let tag = if d.is_empty() { "bindings agree" } else { "bindings DISAGREE" };
            match (tr, mine) {
                (Ok(a), Ok(b)) if a.hlsl == b.hlsl => (format!("{tag}, same HLSL"), d.join("; ")),
                (Ok(_), Ok(_)) => (format!("{tag}, different HLSL"), d.join("; ")),
                (Ok(_), Err(e)) => (format!("{tag}, table translation fails: {}", normalise(&e)), e),
                (Err(_), Ok(_)) => (format!("{tag}, only the table translation succeeds"), d.join("; ")),
                (Err(_), Err(_)) => (format!("{tag}, both fail"), d.join("; ")),
            }
        }
    };
    if !class.contains("agree, same HLSL") && !class.contains("both fail") {
        eprintln!("FSSMCHK {h:016x} {st} {class} | {detail}");
    }
    FSSM_TALLY.lock().unwrap().entry(format!("{st} {class}")).or_insert((0, h)).0 += 1;
}

/// Translate (and optionally compile) one container. Returns (stage, result, compiled_now).
fn process(blob: &[u8], compile: bool) -> (String, Result<(), String>, bool) {
    let r = std::panic::catch_unwind(|| match extract(blob) {
        Err(e) => (String::from("?"), Err(format!("extract: {e}")), false),
        Ok(ex) => {
            let st = stage_name(&ex.stage).to_string();
            let tr = if FSSM_ROOT.load(Ordering::Relaxed) != 0 { hlsl::translate_with_fssm_root(&ex) } else { hlsl::translate(&ex) };
            if FSSM_CHECK.load(Ordering::Relaxed) != 0 {
                fssm_check(fnv1a(blob), &st, &ex, &tr);
            }
            if MNEM.load(Ordering::Relaxed) != 0 {
                // Instruction usage: (shaders using it, of which translated).
                let names: std::collections::BTreeSet<String> =
                    tombox_xshader::rdna2::disassemble(&ex.isa).into_iter().map(|i| i.mnemonic).collect();
                let mut g = MNEM_USE.lock().unwrap();
                for n in names {
                    let e = g.entry(n).or_insert((0u64, 0u64));
                    e.0 += 1;
                    e.1 += tr.is_ok() as u64;
                }
            }
            if let (Ok(t), Ok(pat)) = (&tr, std::env::var("TB_SCAN_GREP")) {
                if t.hlsl.contains(pat.as_str()) {
                    eprintln!("GREP {:016x} mip_uavs={:?} gmem={}", fnv1a(blob), t.mip_uavs, t.needs_gmem);
                }
            }
            match tr {
                Err(e) => (st, Err(e), false),
                Ok(t) if !compile => {
                    let _ = t;
                    (st, Ok(()), false)
                }
                Ok(t) if USE_FXC.load(Ordering::Relaxed) != 0 && t.profile.split('_').nth(1) != Some("6") => {
                    let path = cache_path(&t.hlsl, t.profile);
                    if path.exists() {
                        return (st, Ok(()), false);
                    }
                    let _ = fxc::compile; // in-process FXC kept for reference; FXC runs as a killable child
                    match fxc_child(&t.hlsl, t.profile, &path, LIMIT.load(Ordering::Relaxed) as u64) {
                        Ok(secs) => {
                            if secs > 5.0 {
                                SLOW.lock().unwrap().push((secs, fnv1a(blob), t.hlsl.lines().count()));
                            }
                            (st, Ok(()), true)
                        }
                        Err(e) => (st, Err(e), true),
                    }
                }
                #[cfg(windows)]
                Ok(t) => {
                    // Default: DXC (shader model 6.0), in process.
                    let profile = tombox_xshader::dxc::profile_for(t.profile);
                    let path = tombox_xshader::dxc::cache_path(&t.hlsl, &profile);
                    if tombox_xshader::dxc::cache_load(&t.hlsl, &profile).is_some() {
                        return (st, Ok(()), false);
                    }
                    let t0 = std::time::Instant::now();
                    match tombox_xshader::dxc::compile(&t.hlsl, &profile) {
                        Ok(code) => {
                            let _ = std::fs::write(&path, code);
                            let secs = t0.elapsed().as_secs_f64();
                            if secs > 5.0 {
                                SLOW.lock().unwrap().push((secs, fnv1a(blob), t.hlsl.lines().count()));
                            }
                            (st, Ok(()), true)
                        }
                        Err(e) => (st, Err(e), true),
                    }
                }
                #[cfg(not(windows))]
                Ok(_) => (st, Err("shader compile mode needs Windows".into()), false),
            }
        }
    });
    r.unwrap_or_else(|_| (String::from("?"), Err("PANIC in translator".to_string()), false))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: {} <file> [--examples <dir>] [--limit <n>] [--jobs <n>] [--compile 1] [--fssm-root 1] [--fssm-check 1]", args[0]);
        return ExitCode::FAILURE;
    }
    let mut examples: Option<String> = None;
    let mut limit = usize::MAX;
    let mut jobs = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut compile = false;
    let mut dump: Option<String> = None;
    let mut dump_every = 1usize;
    let mut i = 2;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--examples" => examples = Some(args[i + 1].clone()),
            "--limit" => limit = args[i + 1].parse().unwrap_or(usize::MAX),
            "--jobs" => jobs = args[i + 1].parse().unwrap_or(jobs).max(1),
            "--compile" => compile = args[i + 1] == "1",
            "--dump" => dump = Some(args[i + 1].clone()),
            "--dump-every" => dump_every = args[i + 1].parse().unwrap_or(1).max(1),
            "--mnemonics" => MNEM.store((args[i + 1] == "1") as usize, Ordering::Relaxed),
            "--fxc-limit" => LIMIT.store(args[i + 1].parse().unwrap_or(120), Ordering::Relaxed),
            "--compiler" => USE_FXC.store((args[i + 1] == "fxc") as usize, Ordering::Relaxed),
            "--fssm-root" => FSSM_ROOT.store((args[i + 1] == "1") as usize, Ordering::Relaxed),
            "--fssm-check" => FSSM_CHECK.store((args[i + 1] == "1") as usize, Ordering::Relaxed),
            other => eprintln!("warning: ignoring {other}"),
        }
        i += 2;
    }
    #[cfg(not(windows))]
    if compile {
        eprintln!("shader compile mode needs Windows");
        return ExitCode::FAILURE;
    }

    let data = match std::fs::read(&args[1]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {}: {e}", args[1]);
            return ExitCode::FAILURE;
        }
    };
    if let Some(d) = &examples {
        let _ = std::fs::create_dir_all(d);
    }
    if compile {
        let _ = std::fs::create_dir_all(std::env::temp_dir().join("tombox_shader_cache").join("by_hlsl"));
        // A full sweep runs for a long time on someone's desktop: stay out of the way.
        #[cfg(windows)]
        unsafe {
            extern "system" {
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
                fn SetPriorityClass(h: *mut std::ffi::c_void, class: u32) -> i32;
            }
            SetPriorityClass(GetCurrentProcess(), 0x4000); // BELOW_NORMAL_PRIORITY_CLASS
        }
        if !args.iter().any(|a| a == "--jobs") {
            jobs = jobs.min(4);
        }
    }
    // Gather candidate blobs: UE shader-archive entries (LZ4 blocks) if the file holds
    // an archive, otherwise raw DXBC containers found by magic.
    let mut blobs: Vec<Vec<u8>> = Vec::new();
    let archives = find_archives(&data);
    let mut lz_fail = 0usize;
    for a in &archives {
        for &(off, size, usize_) in &a.entries {
            let src = &data[off..off + size];
            let dec = if size == usize_ { Some(src.to_vec()) } else { lz4_block(src, usize_) };
            match dec {
                Some(d) => blobs.push(d),
                None => lz_fail += 1,
            }
        }
    }
    if !archives.is_empty() {
        eprintln!("decompressed {} archive entries ({lz_fail} LZ4 failures)", blobs.len());
    } else {
        blobs.push(data.clone());
    }
    // Unique containers.
    let mut seen: HashSet<u64> = HashSet::new();
    let mut found = 0usize;
    let mut work: Vec<(u64, &[u8])> = Vec::new();
    for buf in &blobs {
        let data = &buf[..];
        let mut p = 0usize;
        while p + 32 <= data.len() && seen.len() < limit {
            if &data[p..p + 4] != b"DXBC" {
                p += 1;
                continue;
            }
            let total = u32::from_le_bytes(data[p + 24..p + 28].try_into().unwrap()) as usize;
            let nchunks = u32::from_le_bytes(data[p + 28..p + 32].try_into().unwrap()) as usize;
            if total < 64 || total > 0x40_0000 || p + total > data.len() || nchunks == 0 || nchunks > 32 {
                p += 4;
                continue;
            }
            let blob = &data[p..p + total];
            found += 1;
            p += total;
            let h = fnv1a(blob);
            if seen.insert(h) {
                work.push((h, blob));
            }
        }
    }
    if let Some(d) = &dump {
        let _ = std::fs::create_dir_all(d);
        for (h, blob) in work.iter().step_by(dump_every) {
            let _ = std::fs::write(std::path::Path::new(d).join(format!("{h:016x}.dxbc")), blob);
        }
        return ExitCode::SUCCESS;
    }
    // Translate (and compile) in parallel.
    type Tally = (BTreeMap<String, (usize, usize)>, BTreeMap<String, (usize, u64)>, usize);
    let tally: Mutex<Tally> = Mutex::new((BTreeMap::new(), BTreeMap::new(), 0));
    let next = AtomicUsize::new(0);
    let started = std::time::Instant::now();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= work.len() {
                    break;
                }
                let (h, blob) = work[i];
                let (stage, res, compiled) = process(blob, compile);
                let mut t = tally.lock().unwrap();
                if compiled {
                    t.2 += 1;
                }
                let e = t.0.entry(stage).or_insert((0, 0));
                match res {
                    Ok(()) => e.0 += 1,
                    Err(msg) => {
                        e.1 += 1;
                        let key = normalise(&msg);
                        let ent = t.1.entry(key).or_insert((0, h));
                        if ent.0 < 3 {
                            if let Some(d) = &examples {
                                let _ = std::fs::write(format!("{d}/{h:016x}.dxbc"), blob);
                            }
                        }
                        ent.0 += 1;
                    }
                }
                drop(t);
                if (i + 1) % 5000 == 0 {
                    eprintln!("  {}/{} ({:.0} s)", i + 1, work.len(), started.elapsed().as_secs_f64());
                }
            });
        }
    });
    let (by_stage, errors, compiled_now) = tally.into_inner().unwrap();
    let (ok, fail) = by_stage.values().fold((0, 0), |a, v| (a.0 + v.0, a.1 + v.1));
    println!("containers found: {found}   unique: {}   {}: {ok}   failed: {fail}   ({:.2}% ok)",
        seen.len(), if compile { "translated+compiled" } else { "translated" }, 100.0 * ok as f64 / (ok + fail).max(1) as f64);
    if compile {
        println!("  compiled in this run: {compiled_now} (the rest came from the cache)");
    }
    for (st, (o, f)) in &by_stage {
        println!("  {st}: ok={o} fail={f}");
    }
    let mut v: Vec<_> = errors.into_iter().collect();
    v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    let mut slow = SLOW.lock().unwrap().clone();
    slow.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    if !slow.is_empty() {
        println!("--- {} compiles took more than 5 s; slowest ---", slow.len());
        for (secs, h, lines) in slow.iter().take(25) {
            println!("{secs:>8.1} s  {h:016x}  {lines} HLSL lines");
        }
    }
    println!("--- failure reasons ({} distinct) ---", v.len());
    for (reason, (count, ex)) in v.iter().take(80) {
        println!("{count:>7}  {reason}   [e.g. {ex:016x}]");
    }
    let fssm = FSSM_TALLY.lock().unwrap();
    if !fssm.is_empty() {
        println!("--- FSSM binding table vs root signature (shaders with a root signature) ---");
        for (k, (n, h)) in fssm.iter() {
            println!("{n:>7}  {k}   [e.g. {h:016x}]");
        }
    }
    drop(fssm);
    if MNEM.load(Ordering::Relaxed) != 0 {
        println!("--- mnemonics (shaders using, translated) ---");
        for (n, (used, ok)) in MNEM_USE.lock().unwrap().iter() {
            println!("MNEM {n} {used} {ok}");
        }
    }
    ExitCode::SUCCESS
}
