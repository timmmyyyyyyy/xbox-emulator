use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use compat_common::record;

use crate::device_wrap::Guid;

const M: &str = "d3d12_xs";

static IID_DRED_SETTINGS: Guid = Guid { d1: 0x82BC_481C, d2: 0x6B9B, d3: 0x4030, d4: [0xAE, 0xDB, 0x7E, 0xE3, 0xD1, 0xDF, 0x1E, 0x63] };
static IID_DRED: Guid = Guid { d1: 0x9893_1D33, d2: 0x5AE8, d3: 0x4791, d4: [0xAA, 0x3C, 0x1A, 0x73, 0xA2, 0x93, 0x4E, 0x71] };
static CLSID_DRED: Guid = Guid { d1: 0x4a75_bbc4, d2: 0x9ff4, d3: 0x4ad8, d4: [0x9f, 0x18, 0xab, 0xae, 0x84, 0xdc, 0x5f, 0xf2] };
static IID_DRED_SETTINGS1: Guid = Guid { d1: 0xDBD5_AE51, d2: 0x3317, d3: 0x4F0A, d4: [0xAD, 0xF9, 0x1D, 0x7C, 0xED, 0xCA, 0xAE, 0x0B] };
static IID_DRED1: Guid = Guid { d1: 0x9727_A022, d2: 0xCF1D, d3: 0x4DDA, d4: [0x9E, 0xBA, 0xEF, 0xFA, 0x65, 0x3F, 0xC5, 0x06] };
static IID_FENCE: Guid = Guid { d1: 0x0a75_3dcf, d2: 0xc4d8, d3: 0x4b91, d4: [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76] };
const FORCED_ON: u32 = 2;
const MAX_NODES: usize = 256;
const MAX_ALLOCS: usize = 64;

extern "system" {
    fn CreateEventW(attr: *mut c_void, manual: i32, initial: i32, name: *const u16) -> *mut c_void;
    fn WaitForSingleObject(h: *mut c_void, ms: u32) -> u32;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Off,
    PageFaults,
    Full,
    Markers,
}

pub fn mode_from(v: Option<&str>) -> Mode {
    match v.map(str::trim) {
        Some("0") | Some("off") => Mode::Off,
        Some("1") | Some("full") => Mode::Full,
        Some("2") | Some("markers") => Mode::Markers,
        _ => Mode::PageFaults,
    }
}

pub fn mode() -> Mode {
    mode_from(std::env::var("TOMBOX_DRED").ok().as_deref())
}

pub fn requested() -> bool {
    mode() != Mode::Off
}

static DEVICE: AtomicUsize = AtomicUsize::new(0);
static DRED: AtomicUsize = AtomicUsize::new(0);
static REPORTED: AtomicBool = AtomicBool::new(false);
static REMOVED: AtomicBool = AtomicBool::new(false);

pub fn is_removal(hr: i32) -> bool {
    matches!(hr as u32, 0x887A_0005 | 0x887A_0006 | 0x887A_0007 | 0x887A_0020)
}

#[cfg(test)]
thread_local! {
    static T_REMOVED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

#[cfg(not(test))]
pub fn removed() -> bool {
    REMOVED.load(Ordering::Acquire)
}

#[cfg(test)]
pub fn removed() -> bool {
    T_REMOVED.with(|c| c.get())
}

#[cfg(not(test))]
pub fn mark_removed() {
    REMOVED.store(true, Ordering::Release);
}

#[cfg(test)]
pub fn mark_removed() {
    T_REMOVED.with(|c| c.set(true));
}

#[cfg(test)]
pub fn clear_removed_for_test() {
    T_REMOVED.with(|c| c.set(false));
}

pub fn on_failure(hr: i32, site: &str) {
    if is_removal(hr) {
        mark_removed();
        report_once(site, Some(hr));
    }
}

pub fn report_once(site: &str, hr: Option<i32>) -> bool {
    mark_removed();
    let dev = DEVICE.load(Ordering::Acquire) as *mut c_void;
    if dev.is_null() || REPORTED.swap(true, Ordering::AcqRel) {
        return false;
    }
    let what = hr.map_or(String::new(), |h| format!(" returned {h:#010x} ({})", removed_reason_name(h)));
    record(M, "dred.removed", &format!("first device-removal sign: {site}{what}"));
    unsafe { report(dev, DRED.load(Ordering::Acquire) as *mut c_void) };
    crate::hang_cap::dump_on_removal();
    schedule_exit(hr);
    true
}

extern "system" {
    fn ExitProcess(code: u32) -> !;
}

pub fn exit_delay_ms(v: Option<&str>) -> Option<u64> {
    match v.map(str::trim) {
        None | Some("") => Some(3000),
        Some("off") | Some("0") => None,
        Some(s) => s.parse().ok().or(Some(3000)),
    }
}

pub fn exit_code(hr: Option<i32>) -> u32 {
    hr.filter(|h| is_removal(*h)).map_or(0x887A_0006, |h| h as u32)
}

#[cfg(test)]
static TEST_EXIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn schedule_exit(hr: Option<i32>) {
    let Some(ms) = exit_delay_ms(std::env::var("TOMBOX_REMOVED_EXIT_MS").ok().as_deref()) else { return };
    let code = exit_code(hr);
    #[cfg(test)]
    {
        let _ = ms;
        TEST_EXIT.store(code, Ordering::Release);
        record(M, "dred.removed", &format!("test build: would exit with code {code:#010x}; the test process keeps running"));
    }
    #[cfg(not(test))]
    exit_after(ms, code);
}

#[cfg(not(test))]
fn exit_after(ms: u64, code: u32) {
    let log = std::env::var("TOMBOX_LOG").unwrap_or_else(|_| "tombox_compat.log".into());
    record(M, "dred.removed", &format!("the GPU device is lost ({}); the process exits with code {code:#010x} in {ms} ms instead of letting the title run on a dead device. DRED summary: dred.* lines in {log}; hang captures in {}", removed_reason_name(code as i32), crate::hang_cap::dump_dir().display()));
    let _ = std::thread::Builder::new().name("tombox-removed-exit".into()).spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        record(M, "dred.removed", &format!("exiting now with code {code:#010x} (device removed)"));
        unsafe { ExitProcess(code) }
    });
}

unsafe fn slot(obj: *mut c_void, i: usize) -> usize {
    *(*(obj as *const *const usize)).add(i)
}

unsafe fn release(obj: *mut c_void) {
    if obj.is_null() {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(slot(obj, 2));
    f(obj);
}

pub fn enable_before_create() -> bool {
    match mode() {
        Mode::Off => false,
        m => enable_now(m),
    }
}

pub(crate) fn enable_now(m: Mode) -> bool {
    let mut settings: *mut c_void = core::ptr::null_mut();
    let factory = crate::agility::factory();
    let (hr, via) = unsafe {
        if !factory.is_null() {
            let get: unsafe extern "system" fn(*mut c_void, *const Guid, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(factory, 7));
            (get(factory, &CLSID_DRED, &IID_DRED_SETTINGS, &mut settings), "Agility device factory")
        } else {
            let p = crate::host_proc(b"D3D12GetDebugInterface\0");
            if p == 0 {
                record(M, "dred", "host d3d12.dll has no D3D12GetDebugInterface; DRED not enabled");
                return false;
            }
            let get: unsafe extern "system" fn(*const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            (get(&IID_DRED_SETTINGS, &mut settings), "D3D12GetDebugInterface")
        }
    };
    if hr < 0 || settings.is_null() {
        record(M, "dred", &format!("ID3D12DeviceRemovedExtendedDataSettings unavailable via {via} hr={hr:#010x}; DRED not enabled"));
        return false;
    }
    unsafe {
        let crumbs: unsafe extern "system" fn(*mut c_void, u32) = core::mem::transmute(slot(settings, 3));
        let faults: unsafe extern "system" fn(*mut c_void, u32) = core::mem::transmute(slot(settings, 4));
        if m != Mode::PageFaults {
            crumbs(settings, FORCED_ON);
        }
        faults(settings, FORCED_ON);
        if m == Mode::Markers {
            let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(settings, 0));
            let mut s1: *mut c_void = core::ptr::null_mut();
            if qi(settings, &IID_DRED_SETTINGS1, &mut s1) >= 0 && !s1.is_null() {
                let ctx: unsafe extern "system" fn(*mut c_void, u32) = core::mem::transmute(slot(s1, 6));
                ctx(s1, FORCED_ON);
                release(s1);
            } else {
                record(M, "dred", "ID3D12DeviceRemovedExtendedDataSettings1 unavailable: breadcrumb contexts (PSO markers) stay off");
            }
        }
        release(settings);
    }
    let what = match m {
        Mode::Markers => "auto-breadcrumbs, breadcrumb contexts (PSO markers) and page-fault reporting",
        Mode::Full => "auto-breadcrumbs and page-fault reporting (TOMBOX_DRED=2 adds PSO and Dispatch group-count markers)",
        _ => "page-fault reporting (TOMBOX_DRED=1 adds auto-breadcrumbs, 2 PSO and Dispatch markers, 0 disables)",
    };
    record(M, "dred", &format!("DRED {what} FORCED_ON via {via} (before device creation)"));
    true
}

pub fn watch(device: *mut c_void) -> bool {
    !device.is_null() && arm(device)
}

fn arm(device: *mut c_void) -> bool {
    unsafe {
        let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(device, 0));
        let mut dred: *mut c_void = core::ptr::null_mut();
        let hr = qi(device, &IID_DRED, &mut dred);
        if hr < 0 || dred.is_null() {
            record(M, "dred", &format!("device has no ID3D12DeviceRemovedExtendedData (hr={hr:#010x}); a removal reports only GetDeviceRemovedReason"));
            dred = core::ptr::null_mut();
        }
        DEVICE.store(device as usize, Ordering::Release);
        DRED.store(dred as usize, Ordering::Release);
        let create_fence: unsafe extern "system" fn(*mut c_void, u64, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(device, 36));
        let mut fence: *mut c_void = core::ptr::null_mut();
        let hr = create_fence(device, 0, 0, &IID_FENCE, &mut fence);
        let ev = CreateEventW(core::ptr::null_mut(), 0, 0, core::ptr::null());
        if hr < 0 || fence.is_null() || ev.is_null() {
            record(M, "dred", &format!("removal watch fence/event failed (CreateFence hr={hr:#010x}, event={ev:p})"));
            release(dred);
            return false;
        }
        let on_done: unsafe extern "system" fn(*mut c_void, u64, *mut c_void) -> i32 = core::mem::transmute(slot(fence, 9));
        let hr = on_done(fence, u64::MAX, ev);
        if hr < 0 {
            record(M, "dred", &format!("removal watch SetEventOnCompletion(UINT64_MAX) hr={hr:#010x}"));
            release(fence);
            release(dred);
            return false;
        }
        let (e, f) = (ev as usize, fence as usize);
        let spawned = std::thread::Builder::new().name("tombox-dred-watch".into()).spawn(move || {
            WaitForSingleObject(e as *mut c_void, u32::MAX);
            report_once("removal watch fence signalled (device lost)", None);
            let _ = f;
        });
        if spawned.is_err() {
            record(M, "dred", "removal watch thread could not start");
            release(fence);
            release(dred);
            return false;
        }
    }
    record(M, "dred", "device-removal watch armed (fence UINT64_MAX)");
    true
}

unsafe fn report(device: *mut c_void, dred: *mut c_void) {
    let reason: unsafe extern "system" fn(*mut c_void) -> i32 = core::mem::transmute(slot(device, 37));
    let hr = reason(device);
    record(M, "dred.removed", &format!("device removed: GetDeviceRemovedReason={hr:#010x} ({})", removed_reason_name(hr)));
    if dred.is_null() {
        record(M, "dred.removed", "no DRED interface on this device: no breadcrumbs or page-fault data");
        return;
    }
    if mode() == Mode::Markers {
        for l in breadcrumb_report(device, false) {
            record(M, "dred.breadcrumb", &l);
        }
    } else {
    let mut crumbs: usize = 0;
    let get_crumbs: unsafe extern "system" fn(*mut c_void, *mut usize) -> i32 = core::mem::transmute(slot(dred, 3));
    let hr = get_crumbs(dred, &mut crumbs);
    if hr < 0 {
        record(M, "dred.breadcrumb", &format!("GetAutoBreadcrumbsOutput hr={hr:#010x}"));
    } else {
        for l in format_breadcrumbs(crumbs as *const u8) {
            record(M, "dred.breadcrumb", &l);
        }
    }
    }
    let mut pf = [0usize; 3];
    let get_pf: unsafe extern "system" fn(*mut c_void, *mut usize) -> i32 = core::mem::transmute(slot(dred, 4));
    let hr = get_pf(dred, pf.as_mut_ptr());
    if hr < 0 {
        record(M, "dred.pagefault", &format!("GetPageFaultAllocationOutput hr={hr:#010x}"));
    } else {
        for l in format_page_fault(pf.as_ptr() as *const u8) {
            record(M, "dred.pagefault", &l);
        }
        let va = pf[0] as u64;
        if va != 0 {
            for l in describe_fault(va, &crate::gpu_va::near(va, 3)) {
                record(M, "dred.pagefault", &l);
            }
        }
    }
}

pub unsafe fn breadcrumb_report(device: *mut c_void, all_contexts: bool) -> Vec<String> {
    let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(device, 0));
    let mut d1: *mut c_void = core::ptr::null_mut();
    let hr = qi(device, &IID_DRED1, &mut d1);
    if hr < 0 || d1.is_null() {
        return vec![format!("device has no ID3D12DeviceRemovedExtendedData1 (hr={hr:#010x}): no breadcrumb contexts")];
    }
    let mut head: usize = 0;
    let get: unsafe extern "system" fn(*mut c_void, *mut usize) -> i32 = core::mem::transmute(slot(d1, 5));
    let hr = get(d1, &mut head);
    let mut out = vec![format!("{} PSO marker(s) written since start", crate::dred_marks::marks())];
    out.extend(if hr < 0 { vec![format!("GetAutoBreadcrumbsOutput1 hr={hr:#010x}")] } else { format_breadcrumbs1(head as *const u8, all_contexts) });
    release(d1);
    out
}

unsafe fn contexts(node: *const u8) -> Vec<(u32, String)> {
    let n: u32 = rd(node, 80);
    let p: *const u8 = rd(node, 88);
    if p.is_null() {
        return Vec::new();
    }
    (0..n.min(1 << 20) as usize).map(|i| (rd::<u32>(p, i * 16), name_at(p, i * 16 + 8, i * 16 + 8))).collect()
}

pub unsafe fn format_breadcrumbs1(head: *const u8, all_contexts: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut node = head;
    let mut seen = 0usize;
    let mut used = Vec::new();
    while !node.is_null() && seen < MAX_NODES {
        seen += 1;
        let count: u32 = rd(node, 48);
        let last_p: *const u32 = rd(node, 56);
        let hist_p: *const u32 = rd(node, 64);
        let last = if last_p.is_null() { 0 } else { *last_p };
        let mut ctx = contexts(node);
        if all_contexts {
            for (k, s) in &ctx {
                out.push(format!("cmdlist={:#x} context #{k} {s}", rd::<usize>(node, 32)));
            }
        }
        if last < count && !hist_p.is_null() {
            let hist = core::slice::from_raw_parts(hist_p, count as usize);
            let mut src = String::from("contexts from DRED");
            if !ctx.iter().any(|(_, s)| s.starts_with(crate::dred_marks::PREFIX)) {
                let (c, why) = crate::dred_marks::fallback_contexts(rd::<usize>(node, 32), hist, &mut used);
                if !c.is_empty() {
                    ctx.extend(c);
                }
                src = why;
            }
            out.push(src);
            out.push(format!("PSO markers of incomplete command list cmdlist={:#x} (first unfinished op #{last} of {count}, {} context(s)):", rd::<usize>(node, 32), ctx.len()));
            for l in crate::dred_marks::window_lines(hist, last, &ctx) {
                out.push(format!("  {l}"));
            }
        }
        node = rd(node, 72);
    }
    out.extend(format_breadcrumbs(head));
    out
}

pub fn describe_fault(va: u64, near: &[crate::gpu_va::Entry]) -> Vec<String> {
    if near.is_empty() {
        return vec![format!("no tracked resource range near VA {va:#x}")];
    }
    near.iter().map(|e| {
        let end = e.start.saturating_add(e.len);
        let rel = if va >= e.start && va < end {
            format!("CONTAINS fault (+{:#x})", va - e.start)
        } else if va >= end {
            format!("ends {:#x} below fault", va - end)
        } else {
            format!("starts {:#x} above fault", e.start - va)
        };
        format!("tracked {} res={:#x} [{:#x}, {end:#x}) len={:#x}: {rel}", if e.buffer { "buffer" } else { "texture" }, e.res, e.start, e.len)
    }).collect()
}

pub fn removed_reason_name(hr: i32) -> &'static str {
    match hr as u32 {
        0 => "S_OK (not removed)",
        0x887A_0005 => "DXGI_ERROR_DEVICE_REMOVED",
        0x887A_0006 => "DXGI_ERROR_DEVICE_HUNG",
        0x887A_0007 => "DXGI_ERROR_DEVICE_RESET",
        0x887A_0020 => "DXGI_ERROR_DRIVER_INTERNAL_ERROR",
        0x8007_0057 => "E_INVALIDARG",
        0x8007_000E => "E_OUTOFMEMORY",
        _ => "other",
    }
}

const OPS: [&str; 49] = [
    "SETMARKER", "BEGINEVENT", "ENDEVENT", "DRAWINSTANCED", "DRAWINDEXEDINSTANCED", "EXECUTEINDIRECT", "DISPATCH", "COPYBUFFERREGION",
    "COPYTEXTUREREGION", "COPYRESOURCE", "COPYTILES", "RESOLVESUBRESOURCE", "CLEARRENDERTARGETVIEW", "CLEARUNORDEREDACCESSVIEW",
    "CLEARDEPTHSTENCILVIEW", "RESOURCEBARRIER", "EXECUTEBUNDLE", "PRESENT", "RESOLVEQUERYDATA", "BEGINSUBMISSION", "ENDSUBMISSION",
    "DECODEFRAME", "PROCESSFRAMES", "ATOMICCOPYBUFFERUINT", "ATOMICCOPYBUFFERUINT64", "RESOLVESUBRESOURCEREGION", "WRITEBUFFERIMMEDIATE",
    "DECODEFRAME1", "SETPROTECTEDRESOURCESESSION", "DECODEFRAME2", "PROCESSFRAMES1", "BUILDRAYTRACINGACCELERATIONSTRUCTURE",
    "EMITRAYTRACINGACCELERATIONSTRUCTUREPOSTBUILDINFO", "COPYRAYTRACINGACCELERATIONSTRUCTURE", "DISPATCHRAYS", "INITIALIZEMETACOMMAND",
    "EXECUTEMETACOMMAND", "ESTIMATEMOTION", "RESOLVEMOTIONVECTORHEAP", "SETPIPELINESTATE1", "INITIALIZEEXTENSIONCOMMAND",
    "EXECUTEEXTENSIONCOMMAND", "DISPATCHMESH", "ENCODEFRAME", "RESOLVEENCODEROUTPUTMETADATA", "BARRIER", "BEGIN_COMMAND_LIST",
    "DISPATCHGRAPH", "SETPROGRAM",
];

pub fn op_name(op: u32) -> String {
    match op {
        52 => "PROCESSFRAMES2".into(),
        o if (o as usize) < OPS.len() => OPS[o as usize].into(),
        o => format!("OP{o}"),
    }
}

pub fn alloc_type_name(t: u32) -> String {
    let n = match t {
        19 => "COMMAND_QUEUE", 20 => "COMMAND_ALLOCATOR", 21 => "PIPELINE_STATE", 22 => "COMMAND_LIST", 23 => "FENCE",
        24 => "DESCRIPTOR_HEAP", 25 => "HEAP", 27 => "QUERY_HEAP", 28 => "COMMAND_SIGNATURE", 29 => "PIPELINE_LIBRARY",
        30 => "VIDEO_DECODER", 32 => "VIDEO_PROCESSOR", 34 => "RESOURCE", 35 => "PASS", 36 => "CRYPTOSESSION",
        37 => "CRYPTOSESSIONPOLICY", 38 => "PROTECTEDRESOURCESESSION", 39 => "VIDEO_DECODER_HEAP", 40 => "COMMAND_POOL",
        41 => "COMMAND_RECORDER", 42 => "STATE_OBJECT", 43 => "METACOMMAND", 44 => "SCHEDULINGGROUP", 45 => "VIDEO_MOTION_ESTIMATOR",
        46 => "VIDEO_MOTION_VECTOR_HEAP", 47 => "VIDEO_EXTENSION_COMMAND", 48 => "VIDEO_ENCODER", 49 => "VIDEO_ENCODER_HEAP",
        _ => return format!("TYPE{t}"),
    };
    n.into()
}

unsafe fn rd<T: Copy>(p: *const u8, off: usize) -> T {
    core::ptr::read_unaligned(p.add(off) as *const T)
}

unsafe fn name_at(p: *const u8, a: usize, w: usize) -> String {
    let wp: *const u16 = rd(p, w);
    if !wp.is_null() {
        let mut n = 0;
        while n < 256 && *wp.add(n) != 0 {
            n += 1;
        }
        return String::from_utf16_lossy(core::slice::from_raw_parts(wp, n));
    }
    let ap: *const u8 = rd(p, a);
    if !ap.is_null() {
        let mut n = 0;
        while n < 256 && *ap.add(n) != 0 {
            n += 1;
        }
        return String::from_utf8_lossy(core::slice::from_raw_parts(ap, n)).into_owned();
    }
    String::new()
}

pub unsafe fn format_breadcrumbs(head: *const u8) -> Vec<String> {
    let mut out = Vec::new();
    let (mut node, mut seen, mut complete) = (head, 0usize, 0usize);
    while !node.is_null() && seen < MAX_NODES {
        seen += 1;
        let list = name_at(node, 0, 8);
        let queue = name_at(node, 16, 24);
        let count: u32 = rd(node, 48);
        let last_p: *const u32 = rd(node, 56);
        let hist: *const u32 = rd(node, 64);
        let last = if last_p.is_null() { 0 } else { *last_p };
        if last >= count || hist.is_null() {
            complete += 1;
        } else {
            let lo = last.saturating_sub(4);
            let hi = (last + 3).min(count);
            let ops: Vec<String> = (lo..hi).map(|i| {
                let n = op_name(*hist.add(i as usize));
                if i == last { format!(">>{n}<<") } else { n }
            }).collect();
            out.push(format!(
                "incomplete command list list={:?} queue={:?} cmdlist={:#x} completed {last}/{count}, first unfinished op #{last} {}; ops {lo}..{hi}: {}",
                list, queue, rd::<usize>(node, 32), op_name(*hist.add(last as usize)), ops.join(" ")));
        }
        node = rd(node, 72);
    }
    out.push(format!("{seen} breadcrumb node(s){}, {complete} complete, {} incomplete", if !node.is_null() { " (truncated)" } else { "" }, seen - complete));
    out
}

unsafe fn alloc_list(mut n: *const u8, what: &str, out: &mut Vec<String>) {
    let mut k = 0;
    while !n.is_null() && k < MAX_ALLOCS {
        out.push(format!("{what} allocation {} {:?}", alloc_type_name(rd::<u32>(n, 16)), name_at(n, 0, 8)));
        n = rd(n, 24);
        k += 1;
    }
    if !n.is_null() {
        out.push(format!("{what} allocations truncated after {MAX_ALLOCS}"));
    }
}

pub unsafe fn format_page_fault(pf: *const u8) -> Vec<String> {
    let va: u64 = rd(pf, 0);
    let mut out = vec![if va == 0 { "no GPU page fault recorded (PageFaultVA=0)".to_string() } else { format!("GPU page fault at VA {va:#018x}") }];
    alloc_list(rd(pf, 8), "existing", &mut out);
    alloc_list(rd(pf, 16), "recently freed", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_exit_delay_and_code() {
        assert_eq!(exit_delay_ms(None), Some(3000));
        assert_eq!(exit_delay_ms(Some("off")), None);
        assert_eq!(exit_delay_ms(Some("0")), None);
        assert_eq!(exit_delay_ms(Some("750")), Some(750));
        assert_eq!(exit_code(Some(0x887A_0006u32 as i32)), 0x887A_0006);
        assert_eq!(exit_code(Some(0x887A_0005u32 as i32)), 0x887A_0005);
        assert_eq!(exit_code(None), 0x887A_0006);
        assert_eq!(exit_code(Some(0x8007_0057u32 as i32)), 0x887A_0006);
    }

    #[test]
    fn removed_flag_is_set_by_a_removal_failure_only() {
        clear_removed_for_test();
        on_failure(0x8007_0057u32 as i32, "test");
        assert!(!removed());
        on_failure(0x887A_0006u32 as i32, "test");
        assert!(removed());
        clear_removed_for_test();
    }

    #[repr(C)]
    struct Node {
        la: *const u8,
        lw: *const u16,
        qa: *const u8,
        qw: *const u16,
        list: usize,
        queue: usize,
        count: u32,
        last: *const u32,
        hist: *const u32,
        next: *const Node,
    }

    #[repr(C)]
    struct Alloc {
        a: *const u8,
        w: *const u16,
        ty: u32,
        next: *const Alloc,
    }

    #[test]
    fn breadcrumbs_name_the_first_unfinished_op_of_incomplete_lists() {
        let hist = [19u32, 15, 3, 4, 6, 17, 20];
        let done = 7u32;
        let stuck = 3u32;
        let qn: Vec<u16> = "Direct".encode_utf16().chain(Some(0)).collect();
        let n2 = Node { la: b"post\0".as_ptr(), lw: core::ptr::null(), qa: core::ptr::null(), qw: qn.as_ptr(), list: 0x20, queue: 0x30, count: 7, last: &done, hist: hist.as_ptr(), next: core::ptr::null() };
        let n1 = Node { la: core::ptr::null(), lw: core::ptr::null(), qa: b"gfx\0".as_ptr(), qw: core::ptr::null(), list: 0x10, queue: 0x30, count: 7, last: &stuck, hist: hist.as_ptr(), next: &n2 };
        let out = unsafe { format_breadcrumbs(&n1 as *const Node as *const u8) };
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].contains("queue=\"gfx\""), "{}", out[0]);
        assert!(out[0].contains("completed 3/7, first unfinished op #3 DRAWINDEXEDINSTANCED"), "{}", out[0]);
        assert!(out[0].contains("BEGINSUBMISSION RESOURCEBARRIER DRAWINSTANCED >>DRAWINDEXEDINSTANCED<< DISPATCH PRESENT"), "{}", out[0]);
        assert_eq!(out[1], "2 breadcrumb node(s), 1 complete, 1 incomplete");
    }

    #[repr(C)]
    struct Node1 {
        base: Node,
        nctx: u32,
        ctx: *const Ctx,
    }

    #[repr(C)]
    struct Ctx {
        index: u32,
        s: *const u16,
    }

    #[test]
    fn node1_report_names_the_pso_markers_around_the_hung_draw() {
        let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
        let a = w("tbpso 0x10 vs=3d3ac0c8e461a6f6 ps=00000000000000bb");
        let b = w("tbpso 0x20 vs=00000000000000cc ps=00000000000000dd PLACEHOLDER (real PSO still building)");
        let ctx = [Ctx { index: 1, s: a.as_ptr() }, Ctx { index: 4, s: b.as_ptr() }];
        let hist = [46u32, 0, 4, 4, 0, 4, 4, 4];
        let stuck = 5u32;
        let n = Node1 {
            base: Node { la: core::ptr::null(), lw: core::ptr::null(), qa: core::ptr::null(), qw: core::ptr::null(), list: 0x67d5_8b60, queue: 0x30, count: 8, last: &stuck, hist: hist.as_ptr(), next: core::ptr::null() },
            nctx: 2,
            ctx: ctx.as_ptr(),
        };
        let out = unsafe { format_breadcrumbs1(&n as *const Node1 as *const u8, false) };
        let all = out.join("
");
        assert!(out[0] == "contexts from DRED", "{all}");
        assert!(out[1].starts_with("PSO markers of incomplete command list cmdlist=0x67d58b60 (first unfinished op #5 of 8, 2 context(s))"), "{all}");
        assert_eq!(out[2], "  op #2 DRAWINDEXEDINSTANCED done: tbpso 0x10 vs=3d3ac0c8e461a6f6 ps=00000000000000bb");
        assert_eq!(out[4], "  op #5 DRAWINDEXEDINSTANCED FIRST UNFINISHED: tbpso 0x20 vs=00000000000000cc ps=00000000000000dd PLACEHOLDER (real PSO still building)");
        assert!(all.contains("op #7 DRAWINDEXEDINSTANCED queued: tbpso 0x20"), "{all}");
        assert!(all.contains("2 distinct PSO(s) in ops 1..8; 2 PSO marker(s) in this list"), "{all}");
        assert!(all.contains("completed 5/8, first unfinished op #5 DRAWINDEXEDINSTANCED"), "{all}");
        let bare = Node1 { nctx: 0, ctx: core::ptr::null(), ..n };
        let out = unsafe { format_breadcrumbs1(&bare as *const Node1 as *const u8, true) };
        assert!(out[0].starts_with("no breadcrumb contexts and no kept recording with 2 markers"), "{:?}", out);
        assert!(out[2].ends_with("no PSO marker before this op"), "{:?}", out);
    }

    #[test]
    fn page_fault_lists_va_and_allocations() {
        let rn: Vec<u16> = "SceneColor".encode_utf16().chain(Some(0)).collect();
        let freed = Alloc { a: b"Staging\0".as_ptr(), w: core::ptr::null(), ty: 34, next: core::ptr::null() };
        let a2 = Alloc { a: core::ptr::null(), w: core::ptr::null(), ty: 25, next: core::ptr::null() };
        let a1 = Alloc { a: core::ptr::null(), w: rn.as_ptr(), ty: 34, next: &a2 };
        let pf: [usize; 3] = [0x1_2345_6000, &a1 as *const Alloc as usize, &freed as *const Alloc as usize];
        let out = unsafe { format_page_fault(pf.as_ptr() as *const u8) };
        assert_eq!(out, vec![
            "GPU page fault at VA 0x0000000123456000".to_string(),
            "existing allocation RESOURCE \"SceneColor\"".to_string(),
            "existing allocation HEAP \"\"".to_string(),
            "recently freed allocation RESOURCE \"Staging\"".to_string(),
        ]);
        let none: [usize; 3] = [0, 0, 0];
        assert_eq!(unsafe { format_page_fault(none.as_ptr() as *const u8) }, vec!["no GPU page fault recorded (PageFaultVA=0)".to_string()]);
    }

    #[test]
    fn names_and_env_gate() {
        assert_eq!(op_name(45), "BARRIER");
        assert_eq!(op_name(52), "PROCESSFRAMES2");
        assert_eq!(op_name(50), "OP50");
        assert_eq!(alloc_type_name(42), "STATE_OBJECT");
        assert_eq!(alloc_type_name(26), "TYPE26");
        assert_eq!(removed_reason_name(0x887A_0006u32 as i32), "DXGI_ERROR_DEVICE_HUNG");
        assert_eq!(mode_from(None), Mode::PageFaults);
        assert_eq!(mode_from(Some("1")), Mode::Full);
        assert_eq!(mode_from(Some(" 0 ")), Mode::Off);
        assert_eq!(mode_from(Some("off")), Mode::Off);
        assert_eq!(mode_from(Some("yes")), Mode::PageFaults);
        assert_eq!(mode_from(Some("2")), Mode::Markers);
        assert_eq!(mode_from(Some("markers")), Mode::Markers);
        assert!(!watch(core::ptr::null_mut()));
        assert!(is_removal(0x887A_0005u32 as i32) && is_removal(0x887A_0006u32 as i32) && is_removal(0x887A_0020u32 as i32));
        assert!(!is_removal(0) && !is_removal(0x8007_0057u32 as i32));
    }

    #[test]
    fn fault_lookup_marks_the_containing_range_and_neighbours() {
        use crate::gpu_va::{near_in, Entry};
        let mut m = std::collections::BTreeMap::new();
        for (s, l, r) in [(0x6000_0000u64, 0x1000u64, 1usize), (0x64f9_0000, 0x1fe_0000, 2), (0x6637_c000, 0x10_0000, 3), (0x7000_0000, 0x1000, 4)] {
            m.insert(s, Entry { start: s, len: l, res: r, buffer: r == 3 });
        }
        let va = 0x6640_0000;
        let n = near_in(&m, va, 2);
        assert_eq!(n.iter().map(|e| e.res).collect::<Vec<_>>(), vec![2, 3, 4]);
        let out = describe_fault(va, &n);
        assert_eq!(out[0], "tracked texture res=0x2 [0x64f90000, 0x66f70000) len=0x1fe0000: CONTAINS fault (+0x1470000)");
        assert_eq!(out[1], "tracked buffer res=0x3 [0x6637c000, 0x6647c000) len=0x100000: CONTAINS fault (+0x84000)");
        assert_eq!(out[2], "tracked texture res=0x4 [0x70000000, 0x70001000) len=0x1000: starts 0x9c00000 above fault");
        assert_eq!(describe_fault(va, &[]), vec!["no tracked resource range near VA 0x66400000".to_string()]);
        let below = describe_fault(0x6000_2000, &near_in(&m, 0x6000_2000, 1));
        assert_eq!(below[0], "tracked texture res=0x1 [0x60000000, 0x60001000) len=0x1000: ends 0x1000 below fault");
    }

    #[test]
    fn report_is_one_shot_and_needs_an_armed_device() {
        let first = report_once("test", None);
        assert!(!report_once("test", None));
        if first {
            assert_eq!(TEST_EXIT.load(Ordering::Acquire), 0x887A_0006, "a report arms the exit, which a test build records instead of exiting");
        }
    }

    #[test]
    fn host_runtime_accepts_dred_settings_and_a_warp_device_arms_the_watch() {
        assert!(enable_now(Mode::Full));
        let create = crate::host_proc(b"D3D12CreateDevice\0");
        assert_ne!(create, 0);
        let create: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(create) };
        let mut dev: *mut c_void = core::ptr::null_mut();
        let hr = unsafe { create(core::ptr::null_mut(), 0xb000, crate::IID_ID3D12Device.as_ptr() as *const c_void, &mut dev) };
        if hr < 0 || dev.is_null() {
            return;
        }
        assert!(arm(dev));
    }
}
