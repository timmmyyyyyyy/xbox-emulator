#![allow(dead_code)]

use core::ffi::c_void;
use std::sync::OnceLock;

#[repr(C)]
pub struct Guid(u32, u16, u16, [u8; 8]);

const fn num(v: &[u64; 32], from: usize, len: usize) -> u64 {
    let mut x = 0u64;
    let mut k = 0;
    while k < len {
        x = (x << 4) | v[from + k];
        k += 1;
    }
    x
}

pub const fn guid(s: &str) -> Guid {
    let b = s.as_bytes();
    const fn hex(c: u8) -> u64 {
        match c {
            b'0'..=b'9' => (c - b'0') as u64,
            b'a'..=b'f' => (c - b'a' + 10) as u64,
            _ => (c - b'A' + 10) as u64,
        }
    }
    let mut v = [0u64; 32];
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        if b[i] != b'-' {
            v[n] = hex(b[i]);
            n += 1;
        }
        i += 1;
    }
    let mut tail = [0u8; 8];
    let mut k = 0;
    while k < 8 {
        tail[k] = num(&v, 16 + 2 * k, 2) as u8;
        k += 1;
    }
    Guid(num(&v, 0, 8) as u32, num(&v, 8, 4) as u16, num(&v, 12, 4) as u16, tail)
}

pub const XSYSTEM: Guid = guid("e349bd1a-fc20-4e40-b99c-4178cc6b409f");
pub const XSYSTEM_IIDS: [Guid; 3] = [guid("e349bd1a-fc20-4e40-b99c-4178cc6b409f"), guid("67ce4bfc-b1d1-4ac7-bc3a-cb9219a97a85"), guid("6fd71f09-7513-49f0-89bc-bfaf5df6f852")];
pub const ANALYTICS: Guid = guid("b884675d-b738-4a9c-815d-9a9a1e0c6c9b");
pub const XGAME: Guid = guid("973a344e-24bf-4d0f-8457-56c534892b29");
pub const XGAME_IIDS: [Guid; 2] = [guid("50849859-0ad8-4f81-80e4-5bc78626f852"), guid("2549f142-6419-4a06-97b5-931aab7c2f34")];

pub const S_OK: i32 = 0;
pub const E_NOT_SUFFICIENT_BUFFER: i32 = 0x8007_007Au32 as i32;
pub const E_NOT_FOUND: i32 = 0x8007_0490u32 as i32;

pub const CONSOLE_ID_BYTES: usize = 39;
pub const SANDBOX_ID_BYTES: usize = 16;
pub const APP_DEVICE_ID_BYTES: usize = 45;
pub const ANALYTICS_BYTES: usize = 144;

const CONSOLE_ID: usize = 3;
const SANDBOX_ID: usize = 4;
const APP_DEVICE_ID: usize = 5;

const FILL: u8 = 0xcc;
const UNTOUCHED: usize = 0xdead;

pub type Query = unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> i32;

static QUERY: OnceLock<usize> = OnceLock::new();

pub fn install(q: Query) {
    let _ = QUERY.set(q as usize);
}

pub fn object(provider: &Guid, iid: &Guid) -> usize {
    let q: Query = unsafe { core::mem::transmute(*QUERY.get().expect("QueryApiImpl not installed")) };
    let mut obj = core::ptr::null_mut();
    let hr = unsafe { q(provider, iid, &mut obj) };
    assert_eq!(hr, S_OK, "QueryApiImpl hr={hr:#010x}");
    assert!(!obj.is_null());
    obj as usize
}

fn slot(obj: usize, n: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(n) }
}

pub struct Out {
    pub hr: i32,
    pub used: usize,
    pub buf: Vec<u8>,
}

impl Out {
    fn text(&self) -> &str {
        let n = self.buf.iter().position(|&c| c == 0).expect("no terminator");
        std::str::from_utf8(&self.buf[..n]).expect("not UTF-8")
    }
}

fn string_call(obj: usize, n: usize, size: usize, with_buffer: bool, with_used: bool) -> Out {
    type F = unsafe extern "system" fn(usize, usize, *mut u8, *mut usize) -> i32;
    let f: F = unsafe { core::mem::transmute(slot(obj, n)) };
    let mut buf = vec![FILL; 128];
    let mut used = UNTOUCHED;
    let hr = unsafe { f(obj, size, if with_buffer { buf.as_mut_ptr() } else { core::ptr::null_mut() }, if with_used { &mut used } else { core::ptr::null_mut() }) };
    Out { hr, used, buf }
}

fn system() -> usize {
    object(&XSYSTEM, &XSYSTEM_IIDS[2])
}

fn assert_untouched(o: &Out) {
    assert_eq!(o.used, UNTOUCHED);
    assert!(o.buf.iter().all(|&c| c == FILL), "{:02x?}", &o.buf[..64]);
}

fn assert_fixed_field(o: &Out, n: usize) {
    assert_eq!(o.hr, S_OK);
    assert!(o.buf[n..].iter().all(|&c| c == FILL), "bytes past the field were written: {:02x?}", &o.buf[..n + 8]);
    assert!(o.buf[..n].iter().all(|&c| c != FILL));
}

pub fn console_id_fills_its_fixed_size() {
    let o = string_call(system(), CONSOLE_ID, CONSOLE_ID_BYTES, true, true);
    assert_fixed_field(&o, CONSOLE_ID_BYTES);
    assert_eq!(o.used, CONSOLE_ID_BYTES);
    let t = o.text();
    assert_eq!(t.len(), CONSOLE_ID_BYTES - 1, "{t}");
    let groups: Vec<&str> = t.split('.').collect();
    assert_eq!(groups.iter().map(|g| g.len()).collect::<Vec<_>>(), [8, 8, 8, 8, 2], "{t}");
    assert!(groups.iter().all(|g| g.bytes().all(|c| c.is_ascii_hexdigit())), "{t}");
}

pub fn console_id_writes_only_its_size_into_a_larger_buffer() {
    let o = string_call(system(), CONSOLE_ID, 128, true, true);
    assert_fixed_field(&o, CONSOLE_ID_BYTES);
    assert_eq!(o.used, CONSOLE_ID_BYTES);
}

pub fn console_id_one_byte_short_is_refused_untouched() {
    let o = string_call(system(), CONSOLE_ID, CONSOLE_ID_BYTES - 1, true, true);
    assert_eq!(o.hr, E_NOT_SUFFICIENT_BUFFER);
    assert_untouched(&o);
}

pub fn console_id_size_query_with_no_buffer_is_refused() {
    let o = string_call(system(), CONSOLE_ID, 0, false, true);
    assert_eq!(o.hr, E_NOT_SUFFICIENT_BUFFER);
    assert_untouched(&o);
}

pub fn console_id_accepts_no_used_pointer() {
    let o = string_call(system(), CONSOLE_ID, CONSOLE_ID_BYTES, true, false);
    assert_fixed_field(&o, CONSOLE_ID_BYTES);
    assert_eq!(o.used, UNTOUCHED);
}

pub fn sandbox_id_is_zero_padded_to_its_maximum() {
    let o = string_call(system(), SANDBOX_ID, SANDBOX_ID_BYTES, true, true);
    assert_fixed_field(&o, SANDBOX_ID_BYTES);
    let t = o.text().to_string();
    assert!(!t.is_empty());
    assert_eq!(o.used, t.len() + 1, "used counts the string and its terminator, not the padded field");
    assert!(o.buf[t.len()..SANDBOX_ID_BYTES].iter().all(|&c| c == 0));
}

pub fn sandbox_id_needs_the_maximum_even_when_it_is_shorter() {
    let o = string_call(system(), SANDBOX_ID, SANDBOX_ID_BYTES - 1, true, true);
    assert_eq!(o.hr, E_NOT_SUFFICIENT_BUFFER);
    assert_untouched(&o);
    let o = string_call(system(), SANDBOX_ID, 8, true, true);
    assert_eq!(o.hr, E_NOT_SUFFICIENT_BUFFER);
    assert_untouched(&o);
}

pub fn sandbox_id_size_query_with_no_buffer_is_refused() {
    let o = string_call(system(), SANDBOX_ID, 0, false, false);
    assert_eq!(o.hr, E_NOT_SUFFICIENT_BUFFER);
    assert_untouched(&o);
}

pub fn sandbox_id_accepts_no_used_pointer() {
    let o = string_call(system(), SANDBOX_ID, SANDBOX_ID_BYTES, true, false);
    assert_fixed_field(&o, SANDBOX_ID_BYTES);
    assert_eq!(o.used, UNTOUCHED);
}

pub fn app_device_id_fills_its_fixed_size() {
    let o = string_call(system(), APP_DEVICE_ID, APP_DEVICE_ID_BYTES, true, true);
    assert_fixed_field(&o, APP_DEVICE_ID_BYTES);
    assert_eq!(o.used, APP_DEVICE_ID_BYTES);
    assert_eq!(o.text().len(), APP_DEVICE_ID_BYTES - 1);
}

pub fn app_device_id_one_byte_short_is_refused_untouched() {
    let o = string_call(system(), APP_DEVICE_ID, APP_DEVICE_ID_BYTES - 1, true, true);
    assert_eq!(o.hr, E_NOT_SUFFICIENT_BUFFER);
    assert_untouched(&o);
}

pub fn app_device_id_accepts_no_used_pointer() {
    let o = string_call(system(), APP_DEVICE_ID, APP_DEVICE_ID_BYTES, true, false);
    assert_fixed_field(&o, APP_DEVICE_ID_BYTES);
    assert_eq!(o.used, UNTOUCHED);
}

pub fn every_xsystem_iid_returns_the_same_ids() {
    let read = |iid: &Guid| {
        let obj = object(&XSYSTEM, iid);
        let c = string_call(obj, CONSOLE_ID, CONSOLE_ID_BYTES, true, true);
        let s = string_call(obj, SANDBOX_ID, SANDBOX_ID_BYTES, true, true);
        assert_eq!((c.hr, s.hr), (S_OK, S_OK));
        (c.buf, c.used, s.buf, s.used)
    };
    let first = read(&XSYSTEM_IIDS[0]);
    for iid in &XSYSTEM_IIDS[1..] {
        assert!(read(iid) == first);
    }
}

pub fn every_xsystem_iid_serves_the_app_specific_device_id() {
    let read = |iid: &Guid| {
        let o = string_call(object(&XSYSTEM, iid), APP_DEVICE_ID, APP_DEVICE_ID_BYTES, true, true);
        assert_eq!(o.hr, S_OK, "hr={:#010x}", o.hr);
        assert_eq!(o.used, APP_DEVICE_ID_BYTES);
        o.buf
    };
    let first = read(&XSYSTEM_IIDS[0]);
    for iid in &XSYSTEM_IIDS[1..] {
        assert_eq!(read(iid), first);
    }
}

fn nul_terminated_and_zero_padded(field: &[u8]) -> String {
    let n = field.iter().position(|&c| c == 0).expect("no terminator");
    assert!(n > 0);
    assert!(field[n..].iter().all(|&c| c == 0), "{field:02x?}");
    String::from_utf8(field[..n].to_vec()).expect("not UTF-8")
}

pub fn analytics_info_is_returned_through_the_hidden_pointer() {
    let obj = object(&ANALYTICS, &ANALYTICS);
    type F = unsafe extern "system" fn(usize, *mut u8) -> *mut u8;
    let f: F = unsafe { core::mem::transmute(slot(obj, 3)) };
    let mut b = [FILL; 160];
    let r = unsafe { f(obj, b.as_mut_ptr()) };
    assert_eq!(r, b.as_mut_ptr());
    assert!(b[ANALYTICS_BYTES..].iter().all(|&c| c == FILL), "bytes past XSystemAnalyticsInfo were written");
    let os = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let hosting = u64::from_le_bytes(b[8..16].try_into().unwrap());
    assert_eq!(os, hosting);
    assert_eq!(os & 0xffff, 10, "major is in the low word: {os:#018x}");
    assert_eq!((os >> 16) & 0xffff, 0, "{os:#018x}");
    assert!((os >> 32) & 0xffff >= 10240, "{os:#018x}");
    let family = nul_terminated_and_zero_padded(&b[16..80]);
    assert!(family.starts_with("Windows."), "{family}");
    nul_terminated_and_zero_padded(&b[80..144]);
}

type TitleId = unsafe extern "system" fn(usize, *mut u32) -> i32;

pub fn title_id_without_a_title_is_not_found() {
    for iid in &XGAME_IIDS {
        let obj = object(&XGAME, iid);
        let f: TitleId = unsafe { core::mem::transmute(slot(obj, 3)) };
        let mut v = 0xdead_beefu32;
        assert_eq!(unsafe { f(obj, &mut v) }, E_NOT_FOUND);
        assert_eq!(v, 0xdead_beef);
    }
}

pub fn title_id_without_a_title_reports_not_found_before_a_null_output() {
    for iid in &XGAME_IIDS {
        let obj = object(&XGAME, iid);
        let f: TitleId = unsafe { core::mem::transmute(slot(obj, 3)) };
        assert_eq!(unsafe { f(obj, core::ptr::null_mut()) }, E_NOT_FOUND);
    }
}

#[macro_export]
macro_rules! xsys_tests {
    ($setup:path) => {
        xsys_tests!(@ $setup;
            console_id_fills_its_fixed_size, console_id_writes_only_its_size_into_a_larger_buffer,
            console_id_one_byte_short_is_refused_untouched, console_id_size_query_with_no_buffer_is_refused,
            console_id_accepts_no_used_pointer,
            sandbox_id_is_zero_padded_to_its_maximum, sandbox_id_needs_the_maximum_even_when_it_is_shorter,
            sandbox_id_size_query_with_no_buffer_is_refused, sandbox_id_accepts_no_used_pointer,
            app_device_id_fills_its_fixed_size, app_device_id_one_byte_short_is_refused_untouched,
            app_device_id_accepts_no_used_pointer,
            every_xsystem_iid_returns_the_same_ids, every_xsystem_iid_serves_the_app_specific_device_id,
            analytics_info_is_returned_through_the_hidden_pointer,
            title_id_without_a_title_is_not_found, title_id_without_a_title_reports_not_found_before_a_null_output);
    };
    (@ $setup:path; $($name:ident),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                if $setup(stringify!($name)) {
                    xsys::$name();
                }
            }
        )*
    };
}
