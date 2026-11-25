use super::{cstr, data_root, log, put_str, setting, title, xasync, Guid, Iface, This, E_ABORT, E_INVALIDARG, E_NOT_FOUND, S_OK};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub const XSYSTEM: Guid = Guid::parse("e349bd1a-fc20-4e40-b99c-4178cc6b409f");
pub const XSYSTEM_V2: Guid = Guid::parse("67ce4bfc-b1d1-4ac7-bc3a-cb9219a97a85");
pub const ANALYTICS: Guid = Guid::parse("b884675d-b738-4a9c-815d-9a9a1e0c6c9b");
pub const XGAME: Guid = Guid::parse("973a344e-24bf-4d0f-8457-56c534892b29");
pub const XGAME_V1: Guid = Guid::parse("50849859-0ad8-4f81-80e4-5bc78626f852");
pub const XGAME_V2: Guid = Guid::parse("2549f142-6419-4a06-97b5-931aab7c2f34");
pub const LAUNCH_URI: Guid = Guid::parse("1b339674-328d-4283-a200-3171f18d3639");
pub const INVITE: Guid = Guid::parse("0651aae2-4012-4077-bf84-8b9097090e2c");
pub const EVENT: Guid = Guid::parse("bbfbdcc7-bfe7-409b-a5ca-edf054960b4d");
pub const PLS: Guid = Guid::parse("f4faf4d4-2d04-4fce-b3e0-474a713a3e84");
pub const XSYSTEM_V3: Guid = Guid::parse("6fd71f09-7513-49f0-89bc-bfaf5df6f852");
pub const PLS_IID: Guid = Guid::parse("d29411df-0794-4553-8b27-95fc02d0f75d");

extern "system" {
    fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
}

pub fn console_id() -> String {
    let x = super::user::user().xuid;
    let y = x.rotate_left(17) ^ 0x5842_4F58_454D_5500;
    format!("{:08X}.{:08X}.{:08X}.{:08X}.{:02X}", (x >> 32) as u32, x as u32, (y >> 32) as u32, y as u32, (x ^ y) as u8)
}

pub const CONSOLE_ID_BYTES: usize = 0x27;
pub const SANDBOX_ID_BYTES: usize = 0x10;
pub const APP_DEVICE_ID_BYTES: usize = 0x2d;

fn field(s: &str, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let b = s.as_bytes();
    let k = b.len().min(n - 1);
    v[..k].copy_from_slice(&b[..k]);
    v
}

unsafe fn put_field(s: &str, n: usize, fixed_used: bool, size: usize, buf: *mut u8, used: *mut usize) -> i32 {
    if size < n {
        return super::E_NOT_SUFFICIENT_BUFFER;
    }
    let v = field(s, n);
    core::ptr::copy_nonoverlapping(v.as_ptr(), buf, n);
    if !used.is_null() {
        *used = if fixed_used { n } else { v.iter().position(|&c| c == 0).unwrap_or(n - 1) + 1 };
    }
    S_OK
}

pub fn sandbox_id() -> String {
    setting("sandbox").unwrap_or_else(|| "RETAIL".into())
}

pub fn device_form() -> &'static str {
    super::identity::device_form()
}

unsafe extern "system" fn get_console_id(_t: This, size: usize, buf: *mut u8, used: *mut usize) -> i32 {
    put_field(&console_id(), CONSOLE_ID_BYTES, true, size, buf, used)
}

unsafe extern "system" fn get_sandbox_id(_t: This, size: usize, buf: *mut u8, used: *mut usize) -> i32 {
    put_field(&sandbox_id(), SANDBOX_ID_BYTES, false, size, buf, used)
}

pub fn analytics_bytes() -> [u8; 144] {
    let mut b = [0u8; 144];
    let os: u64 = super::package::xversion([10, 0, 25398, 0]);
    b[0..8].copy_from_slice(&os.to_le_bytes());
    b[8..16].copy_from_slice(&os.to_le_bytes());
    b[16..28].copy_from_slice(b"Windows.Xbox");
    let form = device_form().as_bytes();
    b[80..80 + form.len()].copy_from_slice(form);
    b
}

unsafe extern "system" fn get_analytics_info(_t: This, out: *mut u8) -> *mut u8 {
    if !out.is_null() {
        let b = analytics_bytes();
        core::ptr::copy_nonoverlapping(b.as_ptr(), out, b.len());
    }
    out
}

unsafe extern "system" fn get_title_id(_t: This, out: *mut u32) -> i32 {
    let Some(id) = title().title_id else { return E_NOT_FOUND };
    if out.is_null() {
        return E_INVALIDARG;
    }
    *out = id;
    S_OK
}

pub fn app_device_id() -> String {
    let seed = format!("{}|{}", console_id(), title().publisher);
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut raw = Vec::with_capacity(32);
    for round in 0..4u64 {
        for b in seed.bytes().chain(round.to_le_bytes()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        raw.extend_from_slice(&h.to_be_bytes());
    }
    crate::online::util::b64(&raw)
}

unsafe extern "system" fn get_app_device_id(_t: This, size: usize, buf: *mut u8, used: *mut usize) -> i32 {
    put_field(&app_device_id(), APP_DEVICE_ID_BYTES, false, size, buf, used)
}

pub(crate) fn split_args(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => {
                if !cur.is_empty() {
                    v.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        v.push(cur);
    }
    v
}

unsafe extern "system" fn launch_new_game(_t: This, exe: *const u8, args: *const u8, _u: usize) -> i32 {
    let Some(exe) = cstr(exe).filter(|e| !e.is_empty()) else { return E_INVALIDARG };
    let args = cstr(args).unwrap_or_default();
    if let Some(hr) = super::launcher::host_launch_new_game(&exe, &args) {
        log("XLaunchNewGame", &format!("'{exe}' '{args}' -> GameHostService message 0x67 hr={hr:#010x}"));
        return hr;
    }
    let path = super::launcher::resolve(&exe);
    match super::launcher::spawn(&path, &args) {
        Ok(pid) => {
            log("XLaunchNewGame", &format!("'{}' '{args}' -> pid {pid}; exiting the current title", path.display()));
            std::process::exit(0)
        }
        Err(hr) => {
            log("XLaunchNewGame", &format!("'{}' '{args}' -> hr={hr:#010x}", path.display()));
            hr
        }
    }
}

unsafe extern "system" fn launch_uri(_t: This, _u: usize, uri: *const u8) -> i32 {
    let Some(u) = cstr(uri).filter(|u| !u.is_empty()) else { return E_INVALIDARG };
    let lower = u.to_ascii_lowercase();
    if lower.starts_with("ms-xbl-") && lower.contains("invite") {
        log("XLaunchUri", &format!("'{u}' -> delivered to this title's invite handlers"));
        super::social::deliver_uri(u);
        return S_OK;
    }
    let hr = super::win::shell_open(&u, None);
    log("XLaunchUri", &format!("'{u}' -> shell open hr={hr:#010x}"));
    hr
}

fn deferrals() -> &'static Mutex<HashMap<usize, super::win::PowerRequest>> {
    static D: std::sync::OnceLock<Mutex<HashMap<usize, super::win::PowerRequest>>> = std::sync::OnceLock::new();
    D.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe extern "system" fn acquire_display_deferral(_t: This, out: *mut usize) -> i32 {
    static NEXT: AtomicUsize = AtomicUsize::new(0x4450_0001);
    if out.is_null() {
        return E_INVALIDARG;
    }
    match super::win::PowerRequest::display(&format!("{} is presenting content", title().display)) {
        Ok(r) => {
            let h = NEXT.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut d) = deferrals().lock() {
                d.insert(h, r);
            }
            *out = h;
            log("XDisplayAcquireTimeoutDeferral", &format!("-> {h:#x} (display kept on)"));
            S_OK
        }
        Err(hr) => hr,
    }
}

unsafe extern "system" fn close_display_deferral(_t: This, h: usize) {
    let removed = deferrals().lock().map(|mut d| d.remove(&h).is_some()).unwrap_or(false);
    log("XDisplayCloseTimeoutDeferralHandle", &format!("{h:#x} released={removed}"));
}

static EVENT_LOCK: Mutex<()> = Mutex::new(());

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn event_write(_t: This, user: usize, scid: *const u8, session: *const u8, name: *const u8, dims: *const u8, meas: *const u8) -> i32 {
    let Some(name) = cstr(name).filter(|n| !n.is_empty()) else { return E_INVALIDARG };
    let parse = |p: *const u8| cstr(p).map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap_or(serde_json::Value::String(t))).unwrap_or(serde_json::Value::Null);
    let rec = serde_json::json!({
        "time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        "user": format!("{user:#x}"),
        "xuid": super::user::user().xuid.to_string(),
        "scid": cstr(scid).unwrap_or_default(),
        "playSession": cstr(session).unwrap_or_default(),
        "name": name,
        "dimensions": parse(dims),
        "measurements": parse(meas),
    });
    let dir = data_root().join("events");
    let _g = EVENT_LOCK.lock();
    let r = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{}.jsonl", super::sanitize(&title().key)))))
        .and_then(|mut f| writeln!(f, "{rec}"));
    match r {
        Ok(()) => S_OK,
        Err(e) => super::win::hr_from_win32(e.raw_os_error().unwrap_or(29) as u32),
    }
}

pub fn pls_dir() -> PathBuf {
    data_root().join("pls").join(super::sanitize(&title().key))
}

fn pls_path() -> String {
    let d = pls_dir();
    let _ = std::fs::create_dir_all(&d);
    let mut s = d.to_string_lossy().into_owned();
    if !s.ends_with('\\') {
        s.push('\\');
    }
    s
}

unsafe extern "system" fn pls_path_size(_t: This, out: *mut usize) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    *out = pls_path().len() + 1;
    S_OK
}

unsafe extern "system" fn pls_get_path(_t: This, size: usize, buf: *mut u8, used: *mut usize) -> i32 {
    put_str(&pls_path(), size, buf, used)
}

fn pls_space() -> (u64, u64, u64) {
    let p: Vec<u16> = pls_path().encode_utf16().chain(Some(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    if unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut avail, &mut total, &mut free) } == 0 {
        avail = 16 << 30;
        free = avail;
        total = avail;
    }
    (avail, free, total)
}

unsafe extern "system" fn pls_space_info(_t: This, out: *mut u64) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let (avail, free, total) = pls_space();
    *out = avail;
    *out.add(1) = free;
    *out.add(2) = 0;
    *out.add(3) = total;
    S_OK
}

const OP_PLS_PROMPT: &[u8] = b"XPersistentLocalStoragePromptUserForSpaceAsync\0";

pub fn pls_prompt_result(requested: u64, available: u64) -> i32 {
    if available >= requested { S_OK } else { E_ABORT }
}

pub const PLS_PROMPT_MIN_BYTES: u64 = 0xa0_0000;

pub fn pls_prompt_needed(requested: u64, free: u64) -> u64 {
    if requested > free { requested - free } else { PLS_PROMPT_MIN_BYTES }
}

pub fn pls_prompt_check(requested: u64, total: u64) -> i32 {
    if requested > total { E_INVALIDARG } else { S_OK }
}

unsafe extern "system" fn pls_prompt_async(_t: This, requested: u64, block: *mut core::ffi::c_void) -> i32 {
    let (_, _, total) = pls_space();
    let hr = pls_prompt_check(requested, total);
    if hr < 0 {
        return hr;
    }
    xasync::run_with(
        block,
        OP_PLS_PROMPT,
        Box::new(move |_c| {
            let (avail, free, _) = pls_space();
            let hr = pls_prompt_result(requested, avail);
            log("XPersistentLocalStoragePromptUserForSpaceAsync", &format!("requested {requested} bytes, free {free}, manage-space ask {} bytes, available {avail} -> {hr:#010x} (no manage-space UI on PC)", pls_prompt_needed(requested, free)));
            xasync::Payload::status(hr)
        }),
    )
}

unsafe extern "system" fn pls_prompt_result_slot(_t: This, block: *mut core::ffi::c_void) -> i32 {
    xasync::status(block)
}

pub fn interfaces() -> Vec<Iface> {
    vec![
        Iface { clsid: XSYSTEM, iid: XSYSTEM, name: "XSystem(e349bd1a)", slots: vec![(3, get_console_id as usize), (4, get_sandbox_id as usize), (5, get_app_device_id as usize)] },
        Iface { clsid: XSYSTEM, iid: XSYSTEM_V2, name: "XSystem(67ce4bfc)", slots: vec![(3, get_console_id as usize), (4, get_sandbox_id as usize), (5, get_app_device_id as usize)] },
        Iface { clsid: XSYSTEM, iid: XSYSTEM_V3, name: "XSystem(6fd71f09)", slots: vec![(3, get_console_id as usize), (4, get_sandbox_id as usize), (5, get_app_device_id as usize)] },
        Iface { clsid: ANALYTICS, iid: ANALYTICS, name: "XSystemAnalytics(b884675d)", slots: vec![(3, get_analytics_info as usize)] },
        Iface { clsid: XGAME, iid: XGAME_V1, name: "XGame(50849859)", slots: vec![(3, get_title_id as usize), (4, launch_new_game as usize)] },
        Iface { clsid: XGAME, iid: XGAME_V2, name: "XGame(2549f142)", slots: vec![(3, get_title_id as usize), (4, launch_new_game as usize)] },
        Iface { clsid: LAUNCH_URI, iid: LAUNCH_URI, name: "XLaunchUri(1b339674)", slots: vec![(3, launch_uri as usize), (4, acquire_display_deferral as usize), (5, close_display_deferral as usize)] },
        Iface { clsid: INVITE, iid: INVITE, name: "XGameInvite(0651aae2)", slots: vec![(3, super::social::register_invite as usize), (4, super::social::unregister_invite as usize)] },
        Iface { clsid: EVENT, iid: EVENT, name: "XGameEvent(bbfbdcc7)", slots: vec![(3, event_write as usize)] },
        Iface {
            clsid: PLS,
            iid: PLS_IID,
            name: "XPersistentLocalStorage(d29411df)",
            slots: vec![(3, pls_path_size as usize), (4, pls_get_path as usize), (5, pls_space_info as usize), (6, pls_prompt_async as usize), (7, pls_prompt_result_slot as usize)],
        },
    ]
}
