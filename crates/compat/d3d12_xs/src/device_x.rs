use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::sync::{Arc, Mutex, OnceLock};
use compat_common::record;

use crate::device_wrap::{real_device, Guid};
use crate::pso_serial as ps;

const S_OK: i32 = 0;
const E_FAIL: i32 = 0x8000_4005u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
const E_POINTER: i32 = 0x8000_4003u32 as i32;

unsafe fn vt(obj: *mut c_void) -> *const usize {
    unsafe { *(obj as *const *const usize) }
}

fn once(counter: &AtomicUsize, limit: usize) -> bool {
    counter.fetch_add(1, Ordering::Relaxed) < limit
}

const IID_INFO_QUEUE: Guid =
    Guid { d1: 0x0742_a90b, d2: 0xc387, d3: 0x483f, d4: [0xb9, 0x46, 0x30, 0xa7, 0xe4, 0xe6, 0x14, 0x58] };
const INFO_QUEUE_SET_BREAK_ON_SEVERITY: usize = 31;
const SEVERITY_CORRUPTION: u32 = 0;
const SEVERITY_ERROR: u32 = 1;

pub fn break_on_error(flags: u32) -> bool {
    flags & 1 == 0
}

pub unsafe extern "system" fn set_debug_flags_x(this: *mut c_void, flags: u32) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    let brk = break_on_error(flags);
    let real = if this.is_null() { core::ptr::null_mut() } else { unsafe { real_device(this) } };
    let mut iq: *mut c_void = core::ptr::null_mut();
    let have = !real.is_null() && unsafe {
        let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
            core::mem::transmute(*vt(real));
        qi(real, &IID_INFO_QUEUE, &mut iq) >= 0
    } && !iq.is_null();
    if have {
        let set: unsafe extern "system" fn(*mut c_void, u32, i32) -> i32 =
            unsafe { core::mem::transmute(*vt(iq).add(INFO_QUEUE_SET_BREAK_ON_SEVERITY)) };
        unsafe {
            set(iq, SEVERITY_CORRUPTION, brk as i32);
            set(iq, SEVERITY_ERROR, brk as i32);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(iq).add(2));
            release(iq);
        }
    }
    if once(&N, 4) {
        record("d3d12_xs", "DebugFlagsX", &format!(
            "flags={flags:#x} breakOnError={brk} debugLayer={have}"));
    }
    S_OK
}

pub unsafe extern "system" fn unregister_page_pool_x(_this: *mut c_void, handle: u64) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    let hr = crate::kmd_slots::unregister_page_pool(handle);
    if hr != S_OK || once(&N, 8) {
        record("d3d12_xs", "UnregisterPagePoolX", &format!("pool {handle:#x} -> {hr:#010x} (IOCTL 0x7900c23b on the emulated KMD)"));
    }
    hr
}

pub unsafe extern "system" fn register_page_pool_x(_this: *mut c_void, va: u64, pages: u32, out: *mut u64) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    let hr = crate::kmd_slots::register_page_pool(va, pages, out);
    if hr != S_OK || once(&N, 8) {
        let h = if out.is_null() { 0 } else { unsafe { core::ptr::read_unaligned(out) } };
        record("d3d12_xs", "RegisterPagePoolX", &format!("va={va:#x} pages={pages} ({} KiB) -> {hr:#010x} pool {h:#x} (IOCTL 0x7900c23b on the emulated KMD)", pages as u64 * 64));
    }
    hr
}

const E_ACCESSDENIED: i32 = 0x8007_0005u32 as i32;
const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;

pub unsafe extern "system" fn serialize_pso_release_x(_this: *mut c_void, pso: *mut c_void, flags: u32, _out: *mut *mut c_void) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    if once(&N, 4) {
        record("d3d12_xs", "SerializePipelineStateX", &format!("pso={pso:p} flags={flags:#x} -> E_ACCESSDENIED (release xgs12 device slots 56/66 body at RVA 0x44950)"));
    }
    E_ACCESSDENIED
}

pub unsafe extern "system" fn device87_release_x(_this: *mut c_void, a: usize, b: usize, c: usize) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    if once(&N, 4) {
        record("d3d12_xs", "device87", &format!("args={a:#x},{b:#x},{c:#x} -> E_NOTIMPL (release xgs12 device slot 87 is the shared E_NOTIMPL stub at RVA 0x16c0)"));
    }
    E_NOTIMPL
}

const IID_DEVICE3: Guid = Guid { d1: 0x81da_dc15, d2: 0x2bad, d3: 0x4392, d4: [0x93, 0xc5, 0x10, 0x13, 0x45, 0xc4, 0xaa, 0x98] };

pub unsafe extern "system" fn open_existing_heap_from_address(
    this: *mut c_void, address: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    if ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    let real = unsafe { real_device(this) };
    let mut dev3: *mut c_void = core::ptr::null_mut();
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt(real)) };
    let hr = unsafe { qi(real, &IID_DEVICE3, &mut dev3) };
    if hr < 0 || dev3.is_null() {
        record("d3d12_xs", "OpenExistingHeapFromAddress", &format!("host device has no ID3D12Device3 (hr={hr:#010x})"));
        return hr;
    }
    let open: unsafe extern "system" fn(*mut c_void, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt(dev3).add(48)) };
    let hr = unsafe { open(dev3, address, riid, ppv) };
    let release: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*vt(dev3).add(2)) };
    unsafe { release(dev3) };
    if hr < 0 || once(&N, 4) {
        record("d3d12_xs", "OpenExistingHeapFromAddress", &format!("address={address:p} -> hr={hr:#010x} heap={:p}", unsafe { *ppv }));
    }
    hr
}

struct ComputeRecord {
    desc: Vec<u8>,
    code: Arc<Vec<u8>>,
}

fn compute_registry() -> &'static Mutex<HashMap<usize, Arc<ComputeRecord>>> {
    static R: OnceLock<Mutex<HashMap<usize, Arc<ComputeRecord>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn pso_parts_for_replay(pso: usize) -> Option<(bool, u64, Vec<Vec<u8>>)> {
    if let Some(rec) = compute_registry().lock().unwrap().get(&pso).cloned() {
        return Some((true, rd64(&rec.desc, 0), ps::encode_compute(&rec.desc, &rec.code)));
    }
    let (desc, _keep) = crate::device_wrap::gfx_pso_desc(pso)?;
    let mut code: [Option<Vec<u8>>; 5] = Default::default();
    for (k, &o) in ps::GFX_STAGES.iter().enumerate() {
        let (p, n) = (rd64(&desc, o) as *const u8, rd64(&desc, o + 8) as usize);
        if !p.is_null() && n != 0 {
            code[k] = Some(unsafe { core::slice::from_raw_parts(p, n) }.to_vec());
        }
    }
    let input = unsafe { gfx_input_of(&desc) };
    Some((false, rd64(&desc, 0), ps::encode_gfx(&desc, &input, &code)))
}

pub fn pso_stage_codes(pso: usize) -> Vec<(&'static str, usize)> {
    if let Some(rec) = compute_registry().lock().unwrap().get(&pso).cloned() {
        return vec![("cs", rec.code.as_ptr() as usize)];
    }
    let Some((desc, _keep)) = crate::device_wrap::gfx_pso_desc(pso) else { return Vec::new() };
    ["vs", "ps", "ds", "hs", "gs"].iter().zip(ps::GFX_STAGES.iter()).map(|(n, &o)| (*n, rd64(&desc, o) as usize)).filter(|e| e.1 != 0).collect()
}

pub fn is_compute(pso: usize) -> bool {
    compute_registry().lock().unwrap_or_else(|e| e.into_inner()).contains_key(&pso)
}

pub fn compute_desc(pso: usize) -> Option<Vec<u8>> {
    compute_registry().lock().unwrap().get(&pso).map(|r| r.desc.clone())
}

pub fn compute_code(pso: usize) -> Option<Arc<Vec<u8>>> {
    compute_registry().lock().unwrap().get(&pso).map(|r| r.code.clone())
}

pub fn register_compute(pso: *mut c_void, desc: &[u8], code: Arc<Vec<u8>>) {
    let mut desc = desc[..ps::COMPUTE_DESC_SIZE].to_vec();
    desc[0x20..0x30].fill(0);
    let rec = Arc::new(ComputeRecord { desc, code });
    compute_registry().lock().unwrap().insert(pso as usize, rec);
}

fn intern(name: &[u8]) -> *const u8 {
    static NAMES: OnceLock<Mutex<HashSet<&'static [u8]>>> = OnceLock::new();
    let mut set = NAMES.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap();
    let c = CString::new(name.to_vec()).unwrap_or_default();
    let bytes = c.as_bytes_with_nul();
    if let Some(s) = set.get(bytes) {
        return s.as_ptr();
    }
    let leaked: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
    set.insert(leaked);
    leaked.as_ptr()
}

unsafe fn c_name(p: *const u8) -> Vec<u8> {
    let mut v = Vec::new();
    if p.is_null() {
        return v;
    }
    while v.len() < 256 {
        let b = unsafe { *p.add(v.len()) };
        if b == 0 {
            break;
        }
        v.push(b);
    }
    v
}

fn rd64(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

fn rd32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

unsafe fn gfx_input_of(desc: &[u8]) -> ps::GfxInput {
    let mut g = ps::GfxInput::default();
    let (lp, ln) = (rd64(desc, 0x228) as *const u8, rd32(desc, 0x230) as usize);
    if !lp.is_null() {
        for i in 0..ln.min(64) {
            let e = unsafe { core::slice::from_raw_parts(lp.add(i * 32), 32) };
            g.elems.push(ps::InputElem {
                name: unsafe { c_name(rd64(e, 0) as *const u8) },
                index: rd32(e, 8), format: rd32(e, 12), slot: rd32(e, 16),
                offset: rd32(e, 20), class: rd32(e, 24), step: rd32(e, 28),
            });
        }
    }
    let (sp, sn) = (rd64(desc, 0x58) as *const u8, rd32(desc, 0x60) as usize);
    if !sp.is_null() {
        for i in 0..sn.min(64) {
            let e = unsafe { core::slice::from_raw_parts(sp.add(i * 24), 24) };
            g.so.push(ps::SoEntry {
                stream: rd32(e, 0), name: unsafe { c_name(rd64(e, 8) as *const u8) }, index: rd32(e, 16),
                start: e[20], count: e[21], slot: e[22],
            });
        }
    }
    let (tp, tn) = (rd64(desc, 0x68) as *const u32, rd32(desc, 0x70) as usize);
    if !tp.is_null() {
        for i in 0..tn.min(4) {
            g.strides.push(unsafe { *tp.add(i) });
        }
    }
    g
}

unsafe fn emit(parts: Vec<Vec<u8>>, out: *mut *mut c_void) {
    for (i, p) in parts.into_iter().enumerate() {
        unsafe { *out.add(i) = ps::new_blob(p) };
    }
}

pub unsafe extern "system" fn serialize_gfx_pso_x(_this: *mut c_void, pso: *mut c_void, flags: u32, out: *mut *mut c_void) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    if out.is_null() {
        return E_POINTER;
    }
    let Some((desc, _keep)) = crate::device_wrap::gfx_pso_desc(pso as usize) else {
        record("d3d12_xs", "SerializeGraphicsPSO_X", &format!("pso {pso:p} was not created through this layer -> E_INVALIDARG"));
        return E_INVALIDARG;
    };
    let mut code: [Option<Vec<u8>>; 5] = Default::default();
    for (k, &o) in ps::GFX_STAGES.iter().enumerate() {
        let (p, n) = (rd64(&desc, o) as *const u8, rd64(&desc, o + 8) as usize);
        if !p.is_null() && n != 0 {
            code[k] = Some(unsafe { core::slice::from_raw_parts(p, n) }.to_vec());
        }
    }
    let input = unsafe { gfx_input_of(&desc) };
    let parts = ps::encode_gfx(&desc, &input, &code);
    if once(&N, 4) {
        record("d3d12_xs", "SerializeGraphicsPSO_X", &format!(
            "pso={pso:p} flags={flags:#x} -> {} parts, {} input elements", parts.len(), input.elems.len()));
    }
    unsafe { emit(parts, out) };
    S_OK
}

pub unsafe extern "system" fn serialize_compute_pso_x(_this: *mut c_void, pso: *mut c_void, flags: u32, out: *mut *mut c_void) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    if out.is_null() {
        return E_POINTER;
    }
    let rec = compute_registry().lock().unwrap().get(&(pso as usize)).cloned();
    let Some(rec) = rec else {
        record("d3d12_xs", "SerializeComputePSO_X", &format!("pso {pso:p} was not created through this layer -> E_INVALIDARG"));
        return E_INVALIDARG;
    };
    if once(&N, 4) {
        record("d3d12_xs", "SerializeComputePSO_X", &format!("pso={pso:p} flags={flags:#x} cs={} bytes", rec.code.len()));
    }
    unsafe { emit(ps::encode_compute(&rec.desc, &rec.code), out) };
    S_OK
}

unsafe fn state_parts<'a>(state: *const u8, n: usize) -> Vec<&'a [u8]> {
    (0..n)
        .map(|i| {
            let p = unsafe { core::ptr::read_unaligned(state.add(8 + 16 * i) as *const u64) } as *const u8;
            let len = unsafe { core::ptr::read_unaligned(state.add(16 + 16 * i) as *const u64) } as usize;
            if p.is_null() || len == 0 || len > 64 << 20 {
                &[][..]
            } else {
                unsafe { core::slice::from_raw_parts(p, len) }
            }
        })
        .collect()
}

pub fn arg_size(ty: u32) -> Option<u32> {
    match ty {
        0 => Some(16),
        1 => Some(20),
        2 => Some(12),
        3 => Some(16),
        4 => Some(16),
        5 => None,
        6 | 7 | 8 => Some(8),
        9 => Some(104),
        10 => Some(12),
        11 => Some(0),
        _ => None,
    }
}

pub fn pc_resource_desc(x: &[u8; 0x38]) -> [u8; 0x38] {
    let mut d = *x;
    let dim = u32::from_le_bytes(d[0..4].try_into().unwrap());
    let flags = u32::from_le_bytes(d[0x30..0x34].try_into().unwrap());
    d[8..16].copy_from_slice(&0u64.to_le_bytes());
    d[0x2c..0x30].copy_from_slice(&(if dim == 1 { 1u32 } else { 0u32 }).to_le_bytes());
    d[0x30..0x34].copy_from_slice(&(flags & 0xFF).to_le_bytes());
    d
}

pub unsafe extern "system" fn get_copyable_footprints_x(
    this: *mut c_void, desc: *const u8, first: u32, num: u32, base: u64,
    layouts: *mut c_void, rows: *mut u32, row_sizes: *mut u64, total: *mut u64,
) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let real = unsafe { real_device(this) };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u64, *mut c_void, *mut u32, *mut u64, *mut u64) =
        unsafe { core::mem::transmute(*vt(real).add(38)) };
    if desc.is_null() {
        unsafe { f(real, desc, first, num, base, layouts, rows, row_sizes, total) };
        return;
    }
    let x: [u8; 0x38] = unsafe { core::ptr::read_unaligned(desc as *const [u8; 0x38]) };
    let mut probe = u64::MAX;
    let t_out = if total.is_null() { &mut probe as *mut u64 } else { total };
    unsafe { f(real, x.as_ptr(), first, num, base, layouts, rows, row_sizes, t_out) };
    if unsafe { *t_out } != u64::MAX {
        return;
    }
    let d = pc_resource_desc(&x);
    unsafe { f(real, d.as_ptr(), first, num, base, layouts, rows, row_sizes, total) };
    if d != x && once(&N, 4) {
        let t = if total.is_null() { u64::MAX } else { unsafe { *total } };
        record("d3d12_xs", "GetCopyableFootprints", &format!(
            "Xbox desc flags={:#x} layout={} mapped to the PC resource desc this layer creates; sub {first}+{num} total={t:#x}",
            u32::from_le_bytes(x[0x30..0x34].try_into().unwrap()), u32::from_le_bytes(x[0x2c..0x30].try_into().unwrap())));
    }
}

pub const XBOX_ARG_DISPATCH_X: u32 = 105;
pub const XBOX_ARG_DISPATCH_MESH_X: u32 = 107;

pub fn pc_argument_type(xbox: u32) -> u32 {
    match xbox {
        XBOX_ARG_DISPATCH_X => 2,
        XBOX_ARG_DISPATCH_MESH_X => 10,
        t => t,
    }
}

pub fn signature_needs_root(args: &[[u32; 4]]) -> bool {
    args.iter().any(|a| matches!(a[0], 5 | 6 | 7 | 8 | 11))
}

pub fn signature_min_stride(args: &[[u32; 4]]) -> Option<u32> {
    args.iter().try_fold(0u32, |acc, a| {
        let n = if a[0] == 5 { Some(a[3] * 4) } else { arg_size(a[0]) };
        n.map(|n| acc + n)
    })
}

pub unsafe extern "system" fn create_command_signature_x(this: *mut c_void, desc: *const u8, root: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    static N: AtomicUsize = AtomicUsize::new(0);
    if desc.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    let stride = unsafe { core::ptr::read_unaligned(desc as *const u32) };
    let n = unsafe { core::ptr::read_unaligned(desc.add(4) as *const u32) } as usize;
    let pargs = unsafe { core::ptr::read_unaligned(desc.add(8) as *const u64) } as *const [u32; 4];
    let xargs: Vec<[u32; 4]> = if pargs.is_null() || n > 64 { Vec::new() } else { (0..n).map(|i| unsafe { core::ptr::read_unaligned(pargs.add(i)) }).collect() };
    let args: Vec<[u32; 4]> = xargs.iter().map(|a| { let mut b = *a; b[0] = pc_argument_type(a[0]); b }).collect();
    let pass_root = if signature_needs_root(&args) { root } else { core::ptr::null_mut() };
    let mut pc_desc = [0u8; 24];
    unsafe { core::ptr::copy_nonoverlapping(desc, pc_desc.as_mut_ptr(), 24) };
    pc_desc[8..16].copy_from_slice(&(if args.is_empty() { 0u64 } else { args.as_ptr() as u64 }).to_le_bytes());
    let real = unsafe { real_device(this) };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt(real).add(41)) };
    let hr = unsafe { f(real, pc_desc.as_ptr(), pass_root, riid, ppv) };
    if hr >= 0 {
        crate::replay::note_signature(unsafe { *ppv } as usize, stride, &args, pass_root as usize);
    }
    if hr < 0 || once(&N, 6) {
        record("d3d12_xs", "CreateCommandSignature", &format!(
            "stride={stride} xboxArgs={xargs:?} pcArgs={args:?} minStride={:?} root={root:p} passedRoot={pass_root:p} -> hr={hr:#010x}", signature_min_stride(&args)));
    }
    if xargs.iter().any(|a| a[0] == XBOX_ARG_DISPATCH_MESH_X) {
        static MESH_NOTE: AtomicUsize = AtomicUsize::new(0);
        if once(&MESH_NOTE, 1) {
            record("d3d12_xs", "CreateCommandSignature", &format!(
                "0x6b -> DISPATCH_MESH (hypothesis: {stride}-byte record, group counts first) -> hr={hr:#010x}"));
        }
    }
    hr
}

pub fn fnv64(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ x as u64).wrapping_mul(0x100_0000_01b3))
}

pub fn dxbc_offsets(b: &[u8]) -> Vec<usize> {
    b.windows(4).enumerate().filter(|(_, w)| *w == b"DXBC").map(|(i, _)| i).take(8).collect()
}

fn dump_native_parts(kind: &str, parts: &[&[u8]]) {
    static N: AtomicUsize = AtomicUsize::new(0);
    static SEEN: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    if cfg!(test) {
        return;
    }
    let key = parts.iter().fold(0u64, |h, p| h.rotate_left(7) ^ fnv64(p));
    if !SEEN.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap_or_else(|e| e.into_inner()).insert(key) {
        return;
    }
    if !once(&N, 96) {
        return;
    }
    let dir = std::env::temp_dir().join("tombox").join("psodump");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut info = String::new();
    for (i, p) in parts.iter().enumerate().filter(|(_, p)| !p.is_empty()) {
        let _ = std::fs::write(dir.join(format!("{kind}_{key:016x}_{i}.bin")), p);
        info.push_str(&format!(" [{i}] {}B dxbc@{:x?}", p.len(), dxbc_offsets(p)));
    }
    record("d3d12_xs", kind, &format!("native blob {key:016x} dumped to {}:{info}", dir.display()));
}

pub fn describe_native_parts(parts: &[&[u8]]) -> String {
    let mut s = String::new();
    for (i, p) in parts.iter().enumerate().filter(|(_, p)| !p.is_empty()) {
        let head: String = p.iter().take(32).map(|b| format!("{b:02x}")).collect();
        s.push_str(&format!(" [{i}] {}B {head}", p.len()));
    }
    s
}

pub unsafe extern "system" fn deserialize_gfx_pso_x(this: *mut c_void, state: *const u8, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    static OK: AtomicUsize = AtomicUsize::new(0);
    static FAIL: AtomicUsize = AtomicUsize::new(0);
    if state.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    let parts = unsafe { state_parts(state, ps::GFX_PARTS) };
    let plan = match ps::decode_gfx(&parts) {
        Ok(p) => p,
        Err(e) => {
            let n = FAIL.fetch_add(1, Ordering::Relaxed);
            if n < 8 {
                record("d3d12_xs", "DeserializeGraphicsPSO_X", &format!("rejected: {e} -> E_INVALIDARG"));
            }
            if n < 3 {
                record("d3d12_xs", "DeserializeGraphicsPSO_X", &format!("native parts:{}", describe_native_parts(&parts)));
            }
            dump_native_parts("gfx", &parts);
            return E_INVALIDARG;
        }
    };
    let mut desc = plan.desc;
    let root = unsafe { core::ptr::read_unaligned(state as *const u64) };
    desc[0..8].copy_from_slice(&root.to_le_bytes());
    let shaders: Vec<Arc<Vec<u8>>> = plan.code.into_iter().map(|c| Arc::new(c.unwrap_or_default())).collect();
    for (k, &o) in ps::GFX_STAGES.iter().enumerate() {
        let c = &shaders[k];
        let p = if c.is_empty() { 0 } else { c.as_ptr() as u64 };
        desc[o..o + 8].copy_from_slice(&p.to_le_bytes());
        desc[o + 8..o + 16].copy_from_slice(&(c.len() as u64).to_le_bytes());
    }
    let mut layout = vec![0u8; plan.input.elems.len() * 32];
    for (i, e) in plan.input.elems.iter().enumerate() {
        let b = &mut layout[i * 32..i * 32 + 32];
        b[0..8].copy_from_slice(&(intern(&e.name) as u64).to_le_bytes());
        for (j, v) in [e.index, e.format, e.slot, e.offset, e.class, e.step].iter().enumerate() {
            b[8 + 4 * j..12 + 4 * j].copy_from_slice(&v.to_le_bytes());
        }
    }
    let mut so = vec![0u8; plan.input.so.len() * 24];
    for (i, s) in plan.input.so.iter().enumerate() {
        let b = &mut so[i * 24..i * 24 + 24];
        b[0..4].copy_from_slice(&s.stream.to_le_bytes());
        b[8..16].copy_from_slice(&(intern(&s.name) as u64).to_le_bytes());
        b[16..20].copy_from_slice(&s.index.to_le_bytes());
        b[20] = s.start;
        b[21] = s.count;
        b[22] = s.slot;
    }
    let strides: Vec<u32> = plan.input.strides.clone();
    let ptr_or_null = |v: &[u8]| if v.is_empty() { 0u64 } else { v.as_ptr() as u64 };
    desc[0x228..0x230].copy_from_slice(&ptr_or_null(&layout).to_le_bytes());
    desc[0x230..0x234].copy_from_slice(&(plan.input.elems.len() as u32).to_le_bytes());
    desc[0x58..0x60].copy_from_slice(&ptr_or_null(&so).to_le_bytes());
    desc[0x60..0x64].copy_from_slice(&(plan.input.so.len() as u32).to_le_bytes());
    let sp = if strides.is_empty() { 0u64 } else { strides.as_ptr() as u64 };
    desc[0x68..0x70].copy_from_slice(&sp.to_le_bytes());
    desc[0x70..0x74].copy_from_slice(&(strides.len() as u32).to_le_bytes());
    let real = unsafe { real_device(this) };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt(real).add(10)) };
    unsafe { crate::cl2_state::patch_gfx_pso_desc(real, &mut desc) };
    let hr = unsafe { f(real, desc.as_ptr(), riid, ppv) };
    if hr >= 0 && !unsafe { *ppv }.is_null() {
        unsafe { crate::device_wrap::register_gfx_pso(*ppv, desc, shaders) };
        if once(&OK, 4) {
            record("d3d12_xs", "DeserializeGraphicsPSO_X", "PSO rebuilt from TomBox serialized parts");
        }
    } else if once(&FAIL, 8) {
        record("d3d12_xs", "DeserializeGraphicsPSO_X", &format!("real CreateGraphicsPipelineState FAILED hr={hr:#010x}"));
    }
    drop((layout, so, strides));
    if hr < 0 { hr } else if unsafe { *ppv }.is_null() { E_FAIL } else { hr }
}

pub unsafe extern "system" fn deserialize_compute_pso_x(this: *mut c_void, state: *const u8, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    static OK: AtomicUsize = AtomicUsize::new(0);
    static FAIL: AtomicUsize = AtomicUsize::new(0);
    if state.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    let parts = unsafe { state_parts(state, ps::COMPUTE_PARTS) };
    let (mut desc, code) = match ps::decode_compute(&parts) {
        Ok(v) => v,
        Err(e) => {
            if once(&FAIL, 8) {
                record("d3d12_xs", "DeserializeComputePSO_X", &format!("rejected: {e} -> E_INVALIDARG"));
            }
            dump_native_parts("compute", &parts);
            return E_INVALIDARG;
        }
    };
    let code = Arc::new(code);
    let root = unsafe { core::ptr::read_unaligned(state as *const u64) };
    desc[0..8].copy_from_slice(&root.to_le_bytes());
    desc[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    desc[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let real = unsafe { real_device(this) };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*vt(real).add(11)) };
    let hr = unsafe { f(real, desc.as_ptr(), riid, ppv) };
    if hr >= 0 && !unsafe { *ppv }.is_null() {
        register_compute(unsafe { *ppv }, &desc, code);
        if once(&OK, 4) {
            record("d3d12_xs", "DeserializeComputePSO_X", "compute PSO rebuilt from TomBox serialized parts");
        }
    } else if once(&FAIL, 8) {
        record("d3d12_xs", "DeserializeComputePSO_X", &format!("real CreateComputePipelineState FAILED hr={hr:#010x}"));
    }
    if hr < 0 { hr } else if unsafe { *ppv }.is_null() { E_FAIL } else { hr }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxbc_part_finds_root_signature_chunk() {
        let mut b = vec![0u8; 56];
        b[0..4].copy_from_slice(b"DXBC");
        b[28..32].copy_from_slice(&2u32.to_le_bytes());
        b[32..36].copy_from_slice(&40u32.to_le_bytes());
        b[36..40].copy_from_slice(&48u32.to_le_bytes());
        b[40..44].copy_from_slice(b"SHEX");
        b[44..48].copy_from_slice(&0u32.to_le_bytes());
        b[48..52].copy_from_slice(b"RTS0");
        b[52..56].copy_from_slice(&4u32.to_le_bytes());
        b.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(b.len(), 60);
        assert_eq!(crate::device_wrap::dxbc_part(&b, b"RTS0"), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(crate::device_wrap::dxbc_part(&b, b"ISG1"), None);
        let w = crate::device_wrap::wrap_root_signature(&[9, 9]);
        assert_eq!(crate::device_wrap::dxbc_part(&w, b"RTS0"), Some(&[9u8, 9][..]));
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()) as usize, w.len());
    }

    #[test]
    fn footprint_desc_drops_xbox_bits() {
        let mut x = [0u8; 0x38];
        x[0..4].copy_from_slice(&3u32.to_le_bytes());
        x[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
        x[0x2c..0x30].copy_from_slice(&261u32.to_le_bytes());
        x[0x30..0x34].copy_from_slice(&0x300020u32.to_le_bytes());
        let d = pc_resource_desc(&x);
        assert_eq!(u32::from_le_bytes(d[0x30..0x34].try_into().unwrap()), 0x20);
        assert_eq!(u32::from_le_bytes(d[0x2c..0x30].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(d[8..16].try_into().unwrap()), 0);
    }

    #[test]
    fn command_signature_root_rules() {
        let draw = [[1u32, 0, 0, 0]];
        assert!(!signature_needs_root(&draw));
        assert_eq!(signature_min_stride(&draw), Some(20));
        let with_const = [[5u32, 0, 0, 2], [2, 0, 0, 0]];
        assert!(signature_needs_root(&with_const));
        assert_eq!(signature_min_stride(&with_const), Some(20));
        let rays = [[9u32, 0, 0, 0]];
        assert!(!signature_needs_root(&rays));
        assert_eq!(signature_min_stride(&rays), Some(104));
        let mesh = [[10u32, 0, 0, 0]];
        assert!(!signature_needs_root(&mesh));
        assert_eq!(signature_min_stride(&mesh), Some(12));
        let inc_const = [[11u32, 0, 0, 0], [2, 0, 0, 0]];
        assert!(signature_needs_root(&inc_const));
        assert_eq!(signature_min_stride(&inc_const), Some(12));
        assert_eq!(pc_argument_type(105), 2);
        assert_eq!(pc_argument_type(107), 10);
        for t in 0..=11 {
            assert_eq!(pc_argument_type(t), t);
        }
        assert_eq!(pc_argument_type(0x70), 0x70);
    }

    #[test]
    fn command_signature_translation_on_real_device() {
        const IID_DEVICE: Guid = Guid { d1: 0x189819f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
        const IID_SIGNATURE: Guid = Guid { d1: 0xc36a797c, d2: 0xec80, d3: 0x4f0a, d4: [0x89, 0x85, 0xa7, 0xb2, 0x47, 0x50, 0x82, 0xd1] };
        const IID_ROOT_SIGNATURE: Guid = Guid { d1: 0xc54a6b66, d2: 0x72df, d3: 0x4ee8, d4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14] };
        const D3D12_FEATURE_D3D12_OPTIONS7: u32 = 32;
        let create = crate::host_proc(b"D3D12CreateDevice\0");
        if create == 0 {
            eprintln!("skipped: no d3d12.dll");
            return;
        }
        unsafe {
            let create: unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 =
                core::mem::transmute(create);
            let mut dev = core::ptr::null_mut();
            if create(core::ptr::null_mut(), 0xb000, &IID_DEVICE, &mut dev) < 0 || dev.is_null() {
                eprintln!("skipped: no D3D12 device");
                return;
            }
            type CreateSig = unsafe extern "system" fn(*mut c_void, *const u8, *mut c_void, *const Guid, *mut *mut c_void) -> i32;
            let real_create: CreateSig = core::mem::transmute(*vt(dev).add(41));
            let wrapper = crate::device_wrap::wrap_device(dev);
            let xbox_create: CreateSig = core::mem::transmute(*vt(wrapper).add(41));
            let release = |obj: *mut c_void| {
                let r: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt(obj).add(2));
                r(obj);
            };

            let mut arg = [0u8; 16];
            arg[..4].copy_from_slice(&XBOX_ARG_DISPATCH_X.to_le_bytes());
            let mut desc = [0u8; 24];
            desc[0..4].copy_from_slice(&12u32.to_le_bytes());
            desc[4..8].copy_from_slice(&1u32.to_le_bytes());
            desc[8..16].copy_from_slice(&(arg.as_ptr() as u64).to_le_bytes());
            desc[16..20].copy_from_slice(&1u32.to_le_bytes());
            let mut sig = core::ptr::null_mut();
            assert!(real_create(dev, desc.as_ptr(), core::ptr::null_mut(), &IID_SIGNATURE, &mut sig) < 0, "desktop rejects 105");
            assert_eq!(xbox_create(wrapper, desc.as_ptr(), core::ptr::null_mut(), &IID_SIGNATURE, &mut sig), S_OK, "wrapper translates 105");
            assert!(!sig.is_null());
            assert_eq!(u32::from_le_bytes(arg[..4].try_into().unwrap()), XBOX_ARG_DISPATCH_X, "caller desc untouched");
            release(sig);

            arg[..4].copy_from_slice(&XBOX_ARG_DISPATCH_MESH_X.to_le_bytes());
            desc[0..4].copy_from_slice(&16u32.to_le_bytes());
            sig = core::ptr::null_mut();
            assert!(real_create(dev, desc.as_ptr(), core::ptr::null_mut(), &IID_SIGNATURE, &mut sig) < 0, "desktop rejects 107");
            let mut opts7 = [0u32; 2];
            let check: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32 = core::mem::transmute(*vt(dev).add(13));
            let has_mesh = check(dev, D3D12_FEATURE_D3D12_OPTIONS7, opts7.as_mut_ptr().cast(), 8) >= 0 && opts7[0] != 0;
            if has_mesh {
                assert_eq!(xbox_create(wrapper, desc.as_ptr(), core::ptr::null_mut(), &IID_SIGNATURE, &mut sig), S_OK, "wrapper translates 107");
                assert!(!sig.is_null());
                assert_eq!(u32::from_le_bytes(arg[..4].try_into().unwrap()), XBOX_ARG_DISPATCH_MESH_X, "caller desc untouched");
                release(sig);
            }

            let empty = [0u8; 0x28];
            let (mut blob, mut err) = (core::ptr::null_mut(), core::ptr::null_mut());
            assert_eq!(crate::D3D12SerializeRootSignature(empty.as_ptr().cast(), 1, &mut blob, &mut err), S_OK);
            let ptr: unsafe extern "system" fn(*mut c_void) -> *const c_void = core::mem::transmute(*vt(blob).add(3));
            let len: unsafe extern "system" fn(*mut c_void) -> usize = core::mem::transmute(*vt(blob).add(4));
            let create_rs: unsafe extern "system" fn(*mut c_void, u32, *const c_void, usize, *const Guid, *mut *mut c_void) -> i32 =
                core::mem::transmute(*vt(dev).add(16));
            let mut rs = core::ptr::null_mut();
            assert_eq!(create_rs(dev, 0, ptr(blob), len(blob), &IID_ROOT_SIGNATURE, &mut rs), S_OK);
            arg[..4].copy_from_slice(&2u32.to_le_bytes());
            desc[0..4].copy_from_slice(&12u32.to_le_bytes());
            sig = core::ptr::null_mut();
            assert!(real_create(dev, desc.as_ptr(), rs, &IID_SIGNATURE, &mut sig) < 0, "desktop rejects unused root signature");
            assert_eq!(xbox_create(wrapper, desc.as_ptr(), rs, &IID_SIGNATURE, &mut sig), S_OK, "wrapper drops unused root signature");
            assert!(!sig.is_null());
            for obj in [sig, rs, blob, wrapper] {
                release(obj);
            }
        }
    }

    #[test]
    fn dxbc_offsets_finds_embedded_containers() {
        let mut b = vec![0u8; 40];
        b[8..12].copy_from_slice(b"DXBC");
        b[30..34].copy_from_slice(b"DXBC");
        assert_eq!(dxbc_offsets(&b), vec![8, 30]);
        assert_ne!(fnv64(b"a"), fnv64(b"b"));
    }

    #[test]
    fn debug_flags_fortnite_values() {
        assert!(break_on_error(0x200));
        assert!(!break_on_error(0x201));
        assert_eq!(unsafe { set_debug_flags_x(core::ptr::null_mut(), 0x201) }, S_OK);
    }

    #[test]
    fn native_part_dump_skips_empty_parts() {
        let a = [0x44u8, 0x58, 0x42, 0x43, 1];
        let parts: Vec<&[u8]> = vec![&a, &[], &[0xff; 40]];
        let s = describe_native_parts(&parts);
        assert!(s.contains("[0] 5B 4458424301"));
        assert!(!s.contains("[1]"));
        assert!(s.contains("[2] 40B ") && s.matches("ff").count() == 32);
    }

    #[test]
    fn page_pool_slots_use_the_emulated_kmd() {
        let _g = crate::kmd_slots::test_serial();
        let mem = vec![0u8; 0x5_0000];
        let va = ((mem.as_ptr() as u64) + 0xffff) & !0xffff;
        let (mut a, mut b) = (0u64, 0u64);
        assert_eq!(unsafe { register_page_pool_x(core::ptr::null_mut(), va, 2, &mut a) }, S_OK);
        assert_eq!(unsafe { register_page_pool_x(core::ptr::null_mut(), va, 2, &mut b) }, S_OK);
        assert_ne!(a, b);
        assert_eq!(crate::kmd_slots::page_pool(a), Some((va, 2)));
        assert_eq!(unsafe { register_page_pool_x(core::ptr::null_mut(), 0, 2, &mut a) }, E_INVALIDARG);
        assert_eq!(unsafe { unregister_page_pool_x(core::ptr::null_mut(), b) }, S_OK);
        drop(mem);
    }

    #[test]
    fn unregister_page_pool_matches_release_driver() {
        let _g = crate::kmd_slots::test_serial();
        let mem = vec![0u8; 0x3_0000];
        let va = ((mem.as_ptr() as u64) + 0xffff) & !0xffff;
        let mut i = 0u64;
        assert_eq!(unsafe { register_page_pool_x(core::ptr::null_mut(), va, 1, &mut i) }, S_OK);
        assert_eq!(unsafe { unregister_page_pool_x(core::ptr::null_mut(), i) }, S_OK);
        assert_eq!(crate::kmd_slots::page_pool(i), None);
        assert_eq!(unsafe { unregister_page_pool_x(core::ptr::null_mut(), i) }, E_FAIL);
        assert_eq!(unsafe { unregister_page_pool_x(core::ptr::null_mut(), 0xffff_0000_0000) }, E_FAIL);
        drop(mem);
    }

    #[test]
    fn interned_names_are_nul_terminated_and_shared() {
        let a = intern(b"POSITION");
        let b = intern(b"POSITION");
        assert_eq!(a, b);
        assert_eq!(unsafe { c_name(a) }, b"POSITION".to_vec());
    }

    #[test]
    fn deserialize_rejects_foreign_state() {
        let junk = [0x11u8; 32];
        let mut state = vec![0u8; 8 + 16 * ps::GFX_PARTS];
        state[8..16].copy_from_slice(&(junk.as_ptr() as u64).to_le_bytes());
        state[16..24].copy_from_slice(&(junk.len() as u64).to_le_bytes());
        let mut out: *mut c_void = 1usize as *mut c_void;
        let hr = unsafe { deserialize_gfx_pso_x(core::ptr::null_mut(), state.as_ptr(), core::ptr::null(), &mut out) };
        assert_eq!(hr, E_INVALIDARG);
        assert!(out.is_null());
        let hr = unsafe { deserialize_compute_pso_x(core::ptr::null_mut(), state.as_ptr(), core::ptr::null(), &mut out) };
        assert_eq!(hr, E_INVALIDARG);
    }

    #[test]
    fn serialize_unknown_pso_is_rejected() {
        let mut out = [core::ptr::null_mut::<c_void>(); 16];
        let hr = unsafe { serialize_compute_pso_x(core::ptr::null_mut(), 0x1234 as *mut c_void, 0, out.as_mut_ptr()) };
        assert_eq!(hr, E_INVALIDARG);
        assert!(out.iter().all(|p| p.is_null()));
    }
}
