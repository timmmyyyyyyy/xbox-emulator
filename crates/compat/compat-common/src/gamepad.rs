use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod host;
mod history;
pub mod mouse;

extern "system" {
    fn GetModuleHandleW(n: *const u16) -> *mut u8;
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
    fn GetCurrentProcessId() -> u32;
    fn QueryPerformanceCounter(v: *mut i64) -> i32;
    fn QueryPerformanceFrequency(v: *mut i64) -> i32;
}

pub const KIND_GAMEPAD: u32 = 0x0004_0000;
const KIND_KEYBOARD: u32 = 0x0000_0010;
const STATUS_CONNECTED: u32 = 0x1;
const STATUS_INPUT_ENABLED: u32 = 0x2;
const S_OK: i32 = 0;
const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
const READING_NOT_FOUND: i32 = 0x838A_0003u32 as i32;
const DEVICE_DISCONNECTED: i32 = 0x838A_0001u32 as i32;
const REFERENCE_READING_TOO_OLD: i32 = 0x838A_0004u32 as i32;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
const E_NOTFOUND: i32 = 0x8007_0490u32 as i32;

pub const BTN_MENU: u32 = 0x1;
pub const BTN_VIEW: u32 = 0x2;
pub const BTN_A: u32 = 0x4;
pub const BTN_B: u32 = 0x8;
pub const BTN_X: u32 = 0x10;
pub const BTN_Y: u32 = 0x20;
pub const BTN_UP: u32 = 0x40;
pub const BTN_DOWN: u32 = 0x80;
pub const BTN_LEFT: u32 = 0x100;
pub const BTN_RIGHT: u32 = 0x200;
pub const BTN_LB: u32 = 0x400;
pub const BTN_RB: u32 = 0x800;
pub const BTN_LS: u32 = 0x1000;
pub const BTN_RS: u32 = 0x2000;

pub const AX_LT: usize = 0;
pub const AX_RT: usize = 1;
pub const AX_LX: usize = 2;
pub const AX_LY: usize = 3;
pub const AX_RX: usize = 4;
pub const AX_RY: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PadState {
    pub buttons: u32,
    pub axes: [f32; 6],
}

impl PadState {
    pub fn apply(&mut self, input: Input, down: bool) {
        match input {
            Input::Button(b) => {
                if down { self.buttons |= b } else { self.buttons &= !b }
            }
            Input::Axis(i, v) => self.axes[i] = if down { v } else { 0.0 },
        }
    }
    pub fn to_bytes(&self) -> [u8; 28] {
        let mut out = [0u8; 28];
        out[0..4].copy_from_slice(&self.buttons.to_le_bytes());
        for i in 0..6 {
            out[4 + i * 4..8 + i * 4].copy_from_slice(&self.axes[i].to_le_bytes());
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    Button(u32),
    Axis(usize, f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Press(Vec<Input>, u64),
    Hold(Vec<Input>, u64),
    Down(Vec<Input>),
    Up(Vec<Input>),
    Stick(bool, f32, f32, Option<u64>),
    Trigger(usize, f32, Option<u64>),
    Wait(u64),
    Release,
    Connect,
    Disconnect,
}

pub const DEFAULT_PRESS_MS: u64 = 120;
pub const GAP_MS: u64 = 120;

pub fn parse_input(name: &str) -> Option<Input> {
    let n = name.trim().to_ascii_lowercase().replace('-', "_");
    let b = |v| Some(Input::Button(v));
    match n.as_str() {
        "a" => b(BTN_A),
        "b" => b(BTN_B),
        "x" => b(BTN_X),
        "y" => b(BTN_Y),
        "menu" | "start" => b(BTN_MENU),
        "view" | "back" | "select" => b(BTN_VIEW),
        "up" | "dpad_up" | "du" => b(BTN_UP),
        "down" | "dpad_down" | "dd" => b(BTN_DOWN),
        "left" | "dpad_left" | "dl" => b(BTN_LEFT),
        "right" | "dpad_right" | "dr" => b(BTN_RIGHT),
        "lb" | "l1" | "left_shoulder" => b(BTN_LB),
        "rb" | "r1" | "right_shoulder" => b(BTN_RB),
        "ls" | "l3" | "left_thumb" => b(BTN_LS),
        "rs" | "r3" | "right_thumb" => b(BTN_RS),
        "lt" | "l2" => Some(Input::Axis(AX_LT, 1.0)),
        "rt" | "r2" => Some(Input::Axis(AX_RT, 1.0)),
        "ls_up" => Some(Input::Axis(AX_LY, 1.0)),
        "ls_down" => Some(Input::Axis(AX_LY, -1.0)),
        "ls_left" => Some(Input::Axis(AX_LX, -1.0)),
        "ls_right" => Some(Input::Axis(AX_LX, 1.0)),
        "rs_up" => Some(Input::Axis(AX_RY, 1.0)),
        "rs_down" => Some(Input::Axis(AX_RY, -1.0)),
        "rs_left" => Some(Input::Axis(AX_RX, -1.0)),
        "rs_right" => Some(Input::Axis(AX_RX, 1.0)),
        _ => None,
    }
}

fn parse_inputs(s: &str) -> Option<Vec<Input>> {
    let v: Option<Vec<Input>> = s.split('+').map(parse_input).collect();
    v.filter(|v| !v.is_empty())
}

fn parse_ms(s: Option<&&str>) -> Option<u64> {
    s.and_then(|t| t.trim_end_matches("ms").parse::<u64>().ok())
}

fn parse_axis_value(s: &str) -> Option<f32> {
    s.parse::<f32>().ok().filter(|v| v.is_finite()).map(|v| v.clamp(-1.0, 1.0))
}

pub fn parse_command(line: &str) -> Result<Option<Cmd>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return Ok(None);
    }
    let t: Vec<&str> = line.split_whitespace().collect();
    let verb = t[0].to_ascii_lowercase();
    let bad = || format!("bad command: {line}");
    let inputs = |i: usize| t.get(i).and_then(|s| parse_inputs(s)).ok_or_else(bad);
    let cmd = match verb.as_str() {
        "press" | "tap" => Cmd::Press(inputs(1)?, parse_ms(t.get(2)).unwrap_or(DEFAULT_PRESS_MS)),
        "hold" => Cmd::Hold(inputs(1)?, parse_ms(t.get(2)).ok_or_else(bad)?),
        "down" => Cmd::Down(inputs(1)?),
        "up" => Cmd::Up(inputs(1)?),
        "stick" => {
            let side = match t.get(1).map(|s| s.to_ascii_lowercase()) {
                Some(s) if s == "l" || s == "left" || s == "ls" => true,
                Some(s) if s == "r" || s == "right" || s == "rs" => false,
                _ => return Err(bad()),
            };
            let x = t.get(2).and_then(|s| parse_axis_value(s)).ok_or_else(bad)?;
            let y = t.get(3).and_then(|s| parse_axis_value(s)).ok_or_else(bad)?;
            Cmd::Stick(side, x, y, parse_ms(t.get(4)))
        }
        "trigger" => {
            let idx = match t.get(1).map(|s| s.to_ascii_lowercase()) {
                Some(s) if s == "lt" || s == "l" || s == "left" => AX_LT,
                Some(s) if s == "rt" || s == "r" || s == "right" => AX_RT,
                _ => return Err(bad()),
            };
            let v = t.get(2).and_then(|s| parse_axis_value(s)).ok_or_else(bad)?.max(0.0);
            Cmd::Trigger(idx, v, parse_ms(t.get(3)))
        }
        "wait" | "sleep" => Cmd::Wait(parse_ms(t.get(1)).ok_or_else(bad)?),
        "release" | "neutral" | "reset" => Cmd::Release,
        "connect" | "plug" => Cmd::Connect,
        "disconnect" | "unplug" => Cmd::Disconnect,
        _ => return Err(bad()),
    };
    Ok(Some(cmd))
}

static STATE: Mutex<PadState> = Mutex::new(PadState { buttons: 0, axes: [0.0; 6] });
static HOST: Mutex<PadState> = Mutex::new(PadState { buttons: 0, axes: [0.0; 6] });
static SCRIPT_ACTIVE: AtomicBool = AtomicBool::new(false);
static TITLE_READS_KEYBOARD: AtomicBool = AtomicBool::new(false);
static SEQ: AtomicU64 = AtomicU64::new(1);
static HIST: Mutex<history::History> = Mutex::new(history::History::new());
static POLLS: AtomicU64 = AtomicU64::new(0);
static REAL_THIS: AtomicUsize = AtomicUsize::new(0);
static ORIG: [AtomicUsize; 24] = [const { AtomicUsize::new(0) }; 24];
static REAL_VTBL: AtomicUsize = AtomicUsize::new(0);
static ORIG_CREATE: AtomicUsize = AtomicUsize::new(0);
static LOG_N: AtomicU32 = AtomicU32::new(0);
static HOST_LOG_N: AtomicU32 = AtomicU32::new(0);
static CONNECTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Registration {
    pub token: u64,
    pub ctx: usize,
    pub cb: usize,
}

static REGS: Mutex<Vec<Registration>> = Mutex::new(Vec::new());

pub fn regs_add(list: &mut Vec<Registration>, r: Registration) {
    if r.token != 0 {
        list.retain(|x| x.token != r.token);
    }
    list.push(r);
    if list.len() > 64 {
        list.remove(0);
    }
}

pub fn regs_remove(list: &mut Vec<Registration>, token: u64) -> bool {
    let n = list.len();
    list.retain(|x| x.token != token);
    list.len() != n
}

fn notify(r: &Registration, current: u32, previous: u32) {
    let f: DeviceCb = unsafe { core::mem::transmute(r.cb) };
    unsafe { f(r.token, r.ctx as *mut c_void, vdev(), now_us(), current, previous) };
}

fn set_connected(on: bool) {
    let regs = REGS.lock().unwrap_or_else(|e| e.into_inner());
    if CONNECTED.swap(on, Ordering::SeqCst) == on {
        return;
    }
    if !on {
        update("unplug", |s| *s = PadState::default());
    }
    log(if on { "connect" } else { "disconnect" }, &format!("virtual pad {} -> notifying {} device callback registration(s)",
        if on { "plugged in" } else { "unplugged" }, regs.len()));
    let (cur, prev) = if on { (STATUS_CONNECTED | STATUS_INPUT_ENABLED, 0) } else { (0, STATUS_CONNECTED | STATUS_INPUT_ENABLED) };
    for r in regs.iter() {
        notify(r, cur, prev);
    }
}

fn log(func: &str, detail: &str) {
    crate::record_raw("gamepad", func, detail);
}

fn log_limited(func: &str, detail: &str) {
    let n = LOG_N.fetch_add(1, Ordering::Relaxed);
    if n < 20 || n % 5000 == 0 {
        log(func, &format!("{detail} (#{n})"));
    }
}

pub fn published() -> PadState {
    let script = *STATE.lock().unwrap_or_else(|e| e.into_inner());
    let host = *HOST.lock().unwrap_or_else(|e| e.into_inner());
    crate::pad_host::merge(script, SCRIPT_ACTIVE.load(Ordering::SeqCst), host)
}

fn published_with(script: &PadState) -> PadState {
    let host = *HOST.lock().unwrap_or_else(|e| e.into_inner());
    crate::pad_host::merge(*script, SCRIPT_ACTIVE.load(Ordering::SeqCst), host)
}

fn record_published() {
    let g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    let state = published_with(&g).to_bytes();
    HIST.lock().unwrap_or_else(|e| e.into_inner()).push(history::Entry { seq, ts: now_us(), state });
}

pub(crate) fn set_host(s: PadState) {
    let changed = {
        let mut g = HOST.lock().unwrap_or_else(|e| e.into_inner());
        let c = *g != s;
        *g = s;
        c
    };
    if changed {
        record_published();
        let n = HOST_LOG_N.fetch_add(1, Ordering::Relaxed);
        if n < 20 || n % 5000 == 0 {
            log("host", &format!("host -> buttons={:#06x} lt={:.2} rt={:.2} lx={:.2} ly={:.2} rx={:.2} ry={:.2}{} (#{n})",
                s.buttons, s.axes[0], s.axes[1], s.axes[2], s.axes[3], s.axes[4], s.axes[5],
                if published() == s { "" } else { ", masked by the scripted pad" }));
        }
    }
}

pub(crate) fn host_connect(on: bool, why: &str) {
    if SCRIPT_ACTIVE.load(Ordering::SeqCst) || CONNECTED.load(Ordering::SeqCst) == on {
        return;
    }
    log("host", &format!("{why} -> virtual pad {}", if on { "plugged in" } else { "unplugged" }));
    set_connected(on);
}

fn set_script_active(on: bool) {
    if SCRIPT_ACTIVE.swap(on, Ordering::SeqCst) != on {
        record_published();
    }
}

pub(crate) fn title_reads_keyboard() -> bool {
    TITLE_READS_KEYBOARD.load(Ordering::SeqCst)
}

fn note_reading_kind(api: &str, kind: u32) {
    if kind & KIND_KEYBOARD != 0 && !TITLE_READS_KEYBOARD.swap(true, Ordering::SeqCst) {
        log(api, &format!("title asks for keyboard readings (kind={kind:#x}); the host keyboard reaches it through GameInput"));
    }
}

pub(crate) fn host_has_gamepad() -> bool {
    let (real, f) = (REAL_THIS.load(Ordering::SeqCst), orig(4));
    if real == 0 || f == 0 {
        return false;
    }
    unsafe {
        let f: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(f);
        let mut r: *mut c_void = core::ptr::null_mut();
        if f(real as *mut c_void, KIND_GAMEPAD, core::ptr::null_mut(), &mut r) < 0 || r.is_null() {
            return false;
        }
        let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*(*(r as *const *const usize)).add(2));
        release(r);
        true
    }
}

pub fn virtual_device() -> *mut c_void {
    vdev()
}

fn update(what: &str, f: impl FnOnce(&mut PadState)) {
    let s = {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
        HIST.lock().unwrap_or_else(|e| e.into_inner()).push(history::Entry { seq, ts: now_us(), state: published_with(&g).to_bytes() });
        *g
    };
    log("state", &format!("{what} -> buttons={:#06x} lt={:.2} rt={:.2} lx={:.2} ly={:.2} rx={:.2} ry={:.2}",
        s.buttons, s.axes[0], s.axes[1], s.axes[2], s.axes[3], s.axes[4], s.axes[5]));
}

#[repr(C)]
struct Device {
    vtbl: *const usize,
}
unsafe impl Sync for Device {}

#[repr(C, align(8))]
struct Info([u8; 0x200]);

static mut DEVICE_VTBL: [usize; 32] = [0; 32];
static mut DEVICE: Device = Device { vtbl: core::ptr::null() };
static mut INFO: Info = Info([0; 0x200]);
pub const DEVICE_ID: [u8; 32] = *b"TomBoxVirtualXboxGamepad00000001";

fn vdev() -> *mut c_void {
    core::ptr::addr_of_mut!(DEVICE) as *mut c_void
}

unsafe fn init_device() {
    let info = &mut *core::ptr::addr_of_mut!(INFO);
    let b = &mut info.0;
    b[0..4].copy_from_slice(&0x140u32.to_le_bytes());
    b[4..6].copy_from_slice(&0x045Eu16.to_le_bytes());
    b[6..8].copy_from_slice(&0x0B12u16.to_le_bytes());
    b[8..10].copy_from_slice(&1u16.to_le_bytes());
    b[12..14].copy_from_slice(&1u16.to_le_bytes());
    b[14..16].copy_from_slice(&5u16.to_le_bytes());
    b[0x20..0x40].copy_from_slice(&DEVICE_ID);
    b[0x40..0x60].copy_from_slice(&DEVICE_ID);
    b[0x60..0x64].copy_from_slice(&1u32.to_le_bytes());
    b[0x68..0x6C].copy_from_slice(&KIND_GAMEPAD.to_le_bytes());
    b[0x6C..0x70].copy_from_slice(&0x3u32.to_le_bytes());
    let vt = &mut *core::ptr::addr_of_mut!(DEVICE_VTBL);
    for s in vt.iter_mut() {
        *s = ret_notimpl as usize;
    }
    vt[0] = dev_qi as usize;
    vt[1] = static_ref as usize;
    vt[2] = static_ref as usize;
    vt[3] = dev_info as usize;
    vt[4] = dev_status as usize;
    vt[5] = dev_battery as usize;
    crate::gameinput_ext::install_pad_feedback(vt.as_mut_ptr());
    (*core::ptr::addr_of_mut!(DEVICE)).vtbl = vt.as_ptr();
}

unsafe extern "system" fn ret_notimpl(_this: *mut c_void) -> i32 {
    E_NOTIMPL
}
unsafe extern "system" fn ret_zero(_this: *mut c_void) -> u64 {
    0
}
unsafe extern "system" fn static_ref(_this: *mut c_void) -> u32 {
    1
}
unsafe extern "system" fn dev_qi(this: *mut c_void, _iid: *const u8, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    *out = this;
    S_OK
}
unsafe extern "system" fn dev_info(_this: *mut c_void) -> *const u8 {
    (*core::ptr::addr_of!(INFO)).0.as_ptr()
}
unsafe extern "system" fn dev_status(_this: *mut c_void) -> u32 {
    STATUS_CONNECTED | STATUS_INPUT_ENABLED
}
unsafe extern "system" fn dev_battery(_this: *mut c_void, out: *mut u8) {
    if !out.is_null() {
        core::ptr::write_bytes(out, 0, 20);
    }
}

#[repr(C)]
struct Reading {
    vtbl: *const usize,
    refs: AtomicU32,
    ts: u64,
    seq: u64,
    state: [u8; 28],
}

static mut READING_VTBL: [usize; 32] = [0; 32];

unsafe fn init_reading_vtbl() {
    let vt = &mut *core::ptr::addr_of_mut!(READING_VTBL);
    for s in vt.iter_mut() {
        *s = ret_zero as usize;
    }
    vt[0] = rd_qi as usize;
    vt[1] = rd_addref as usize;
    vt[2] = rd_release as usize;
    vt[3] = rd_kind as usize;
    vt[4] = rd_seq as usize;
    vt[5] = rd_ts as usize;
    vt[6] = rd_device as usize;
    vt[7] = rd_raw as usize;
    for (i, f) in RD_UNSERVED.iter().enumerate() {
        if i >= 8 {
            vt[i] = *f as usize;
        }
    }
    vt[22] = rd_gamepad as usize;
}

const RD_SLOT_NAMES: [&str; 32] = [
    "QueryInterface", "AddRef", "Release", "GetInputKind", "GetSequenceNumber", "GetTimestamp", "GetDevice", "GetRawReport",
    "GetControllerAxisCount", "GetControllerAxisState", "GetControllerButtonCount", "GetControllerButtonState",
    "GetControllerSwitchCount", "GetControllerSwitchState", "GetKeyCount", "GetKeyState", "GetMouseState",
    "GetTouchCount", "GetTouchState", "GetMotionState", "GetArcadeStickState", "GetFlightStickState",
    "GetGamepadState", "GetRacingWheelState", "GetUiNavigationState", "slot25", "slot26", "slot27", "slot28", "slot29", "slot30", "slot31",
];
static RD_SLOT_SEEN: AtomicU32 = AtomicU32::new(0);

fn rd_unserved(slot: usize) -> u64 {
    let bit = 1u32 << slot;
    if RD_SLOT_SEEN.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
        log("virtual reading", &format!("title called slot {slot} {} on a virtual pad reading for the first time; the virtual pad only reports gamepad input, so it returns 0 (no data of that kind)", RD_SLOT_NAMES[slot]));
    }
    0
}

macro_rules! rd_slots {
    ($($n:literal),*) => { [$({ unsafe extern "system" fn f(_this: *mut c_void) -> u64 { rd_unserved($n) } f as unsafe extern "system" fn(*mut c_void) -> u64 }),*] };
}

static RD_UNSERVED: [unsafe extern "system" fn(*mut c_void) -> u64; 32] = rd_slots!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);

fn is_reading(p: *mut c_void) -> bool {
    !p.is_null() && unsafe { *(p as *const *const usize) == core::ptr::addr_of!(READING_VTBL) as *const usize }
}

fn new_reading() -> *mut c_void {
    POLLS.fetch_add(1, Ordering::SeqCst);
    let (seq, state) = {
        let g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.load(Ordering::SeqCst);
        let state = published_with(&g).to_bytes();
        let mut h = HIST.lock().unwrap_or_else(|e| e.into_inner());
        if h.len() == 0 {
            h.push(history::Entry { seq, ts: now_us(), state });
        }
        (seq, state)
    };
    reading_from(history::Entry { seq, ts: now_us(), state })
}

fn reading_from(e: history::Entry) -> *mut c_void {
    let r = Box::new(Reading {
        vtbl: core::ptr::addr_of!(READING_VTBL) as *const usize,
        refs: AtomicU32::new(1),
        ts: e.ts,
        seq: e.seq,
        state: e.state,
    });
    Box::into_raw(r) as *mut c_void
}

unsafe fn virtual_neighbour(newer: bool, reference: *mut c_void, kind: u32, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    *out = core::ptr::null_mut();
    if reference.is_null() {
        return E_INVALIDARG;
    }
    if !is_reading(reference) {
        return READING_NOT_FOUND;
    }
    if !CONNECTED.load(Ordering::SeqCst) {
        return DEVICE_DISCONNECTED;
    }
    if kind & KIND_GAMEPAD == 0 {
        return READING_NOT_FOUND;
    }
    let seq = (*(reference as *mut Reading)).seq;
    let now = now_us();
    let step = {
        let mut h = HIST.lock().unwrap_or_else(|e| e.into_inner());
        if newer { h.next(seq, now) } else { h.prev(seq, now) }
    };
    match step {
        history::Step::Found(e) => {
            *out = reading_from(e);
            S_OK
        }
        history::Step::NotFound => READING_NOT_FOUND,
        history::Step::TooOld => REFERENCE_READING_TOO_OLD,
    }
}

unsafe extern "system" fn rd_qi(this: *mut c_void, _iid: *const u8, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    rd_addref(this);
    *out = this;
    S_OK
}
unsafe extern "system" fn rd_addref(this: *mut c_void) -> u32 {
    (*(this as *mut Reading)).refs.fetch_add(1, Ordering::SeqCst) + 1
}
unsafe extern "system" fn rd_release(this: *mut c_void) -> u32 {
    let r = this as *mut Reading;
    let n = (*r).refs.fetch_sub(1, Ordering::SeqCst) - 1;
    if n == 0 {
        drop(Box::from_raw(r));
    }
    n
}
unsafe extern "system" fn rd_kind(_this: *mut c_void) -> u32 {
    KIND_GAMEPAD
}
unsafe extern "system" fn rd_seq(this: *mut c_void, _kind: u32) -> u64 {
    (*(this as *mut Reading)).seq
}
unsafe extern "system" fn rd_ts(this: *mut c_void) -> u64 {
    (*(this as *mut Reading)).ts
}
unsafe extern "system" fn rd_device(_this: *mut c_void, out: *mut *mut c_void) {
    if !out.is_null() {
        *out = vdev();
    }
}
unsafe extern "system" fn rd_raw(_this: *mut c_void, out: *mut *mut c_void) -> u32 {
    if !out.is_null() {
        *out = core::ptr::null_mut();
    }
    0
}
unsafe extern "system" fn rd_gamepad(this: *mut c_void, out: *mut u8) -> u32 {
    if out.is_null() {
        return 0;
    }
    core::ptr::copy_nonoverlapping((*(this as *mut Reading)).state.as_ptr(), out, 28);
    1
}

fn now_us() -> u64 {
    let real = REAL_THIS.load(Ordering::SeqCst);
    let f = ORIG[3].load(Ordering::SeqCst);
    if real != 0 && f != 0 {
        let g: unsafe extern "system" fn(usize) -> u64 = unsafe { core::mem::transmute(f) };
        return unsafe { g(real) };
    }
    qpc_us()
}

fn qpc_us() -> u64 {
    let (mut c, mut q) = (0i64, 1i64);
    unsafe {
        QueryPerformanceCounter(&mut c);
        QueryPerformanceFrequency(&mut q);
    }
    ((c as i128 * 1_000_000) / q.max(1) as i128) as u64
}

fn orig(i: usize) -> usize {
    ORIG[i].load(Ordering::SeqCst)
}

type DeviceCb = unsafe extern "system" fn(u64, *mut c_void, *mut c_void, u64, u32, u32);

unsafe extern "system" fn gi_current(this: *mut c_void, kind: u32, device: *mut c_void, out: *mut *mut c_void) -> i32 {
    host::count(4);
    note_reading_kind("GetCurrentReading", kind);
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    if device == vdev() {
        if !CONNECTED.load(Ordering::SeqCst) {
            *out = core::ptr::null_mut();
            return DEVICE_DISCONNECTED;
        }
        if kind & KIND_GAMEPAD == 0 {
            *out = core::ptr::null_mut();
            return READING_NOT_FOUND;
        }
        *out = new_reading();
        log_limited("GetCurrentReading", &format!("virtual pad reading (kind={kind:#x})"));
        return S_OK;
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(orig(4));
    let hr = f(this, kind, device, out);
    host::note_reading(hr, out);
    let hr = host::drop_host_pad(hr, out);
    if hr < 0 && device.is_null() && kind & KIND_GAMEPAD != 0 && kind & 0x130 == 0 && CONNECTED.load(Ordering::SeqCst) {
        *out = new_reading();
        log_limited("GetCurrentReading", &format!("virtual pad reading for any-device query (kind={kind:#x})"));
        return S_OK;
    }
    hr
}

unsafe extern "system" fn gi_next(this: *mut c_void, reference: *mut c_void, kind: u32, device: *mut c_void, out: *mut *mut c_void) -> i32 {
    host::count(5);
    note_reading_kind("GetNextReading", kind);
    if device == vdev() || is_reading(reference) {
        return virtual_neighbour(true, reference, kind, out);
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(orig(5));
    host::drop_host_pad(f(this, reference, kind, device, out), out)
}

unsafe extern "system" fn gi_prev(this: *mut c_void, reference: *mut c_void, kind: u32, device: *mut c_void, out: *mut *mut c_void) -> i32 {
    host::count(6);
    note_reading_kind("GetPreviousReading", kind);
    if device == vdev() || is_reading(reference) {
        return virtual_neighbour(false, reference, kind, out);
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(orig(6));
    host::drop_host_pad(f(this, reference, kind, device, out), out)
}

unsafe extern "system" fn gi_temporal(this: *mut c_void, ts: u64, device: *mut c_void, out: *mut *mut c_void) -> i32 {
    host::count(7);
    if device == vdev() {
        if out.is_null() {
            return 0x8000_4003u32 as i32;
        }
        *out = new_reading();
        return S_OK;
    }
    let f: unsafe extern "system" fn(*mut c_void, u64, *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(orig(7));
    host::drop_host_pad(f(this, ts, device, out), out)
}

unsafe extern "system" fn gi_reading_cb(this: *mut c_void, device: *mut c_void, kind: u32, threshold: f32, ctx: *mut c_void, cb: usize, token: *mut u64) -> i32 {
    host::count(8);
    if device == vdev() {
        log("RegisterReadingCallback", "virtual pad: not supported, title must poll");
        return E_NOTIMPL;
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, f32, *mut c_void, usize, *mut u64) -> i32 = core::mem::transmute(orig(8));
    let (hctx, hcb) = host::wrap_reading_callback(ctx, cb);
    f(this, device, kind, threshold, hctx, hcb, token)
}

unsafe extern "system" fn gi_device_cb(this: *mut c_void, device: *mut c_void, kind: u32, filter: u32, enumk: u32, ctx: *mut c_void, cb: usize, token: *mut u64) -> i32 {
    host::count(9);
    let hr = if device == vdev() {
        if !token.is_null() {
            *token = 0;
        }
        S_OK
    } else {
        let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32, *mut c_void, usize, *mut u64) -> i32 = core::mem::transmute(orig(9));
        let (hctx, hcb) = host::wrap_device_callback(ctx, cb);
        f(this, device, kind, filter, enumk, hctx, hcb, token)
    };
    let wants = (device.is_null() || device == vdev()) && kind & KIND_GAMEPAD != 0 && filter & STATUS_CONNECTED != 0 && cb != 0;
    if hr < 0 || !wants {
        log("RegisterDeviceCallback", &format!("kind={kind:#x} filter={filter:#x} enum={enumk} hr={hr:#x} -> not a gamepad listener"));
        return hr;
    }
    let r = Registration { token: if token.is_null() { 0 } else { *token }, ctx: ctx as usize, cb };
    let mut regs = REGS.lock().unwrap_or_else(|e| e.into_inner());
    regs_add(&mut regs, r);
    let now = CONNECTED.load(Ordering::SeqCst) && enumk != 0;
    log("RegisterDeviceCallback", &format!("kind={kind:#x} filter={filter:#x} enum={enumk} token={:#x} -> gamepad listener recorded ({} total){}",
        r.token, regs.len(), if now { ", virtual pad announced now" } else { ", virtual pad stays unplugged until the first pad command" }));
    if now {
        notify(&r, STATUS_CONNECTED | STATUS_INPUT_ENABLED, 0);
    }
    hr
}

unsafe extern "system" fn gi_stop(this: *mut c_void, token: u64) {
    host::count(12);
    if regs_remove(&mut REGS.lock().unwrap_or_else(|e| e.into_inner()), token) {
        log("StopCallback", &format!("token={token:#x} gamepad listener removed"));
    }
    let f: unsafe extern "system" fn(*mut c_void, u64) = core::mem::transmute(orig(12));
    f(this, token)
}

unsafe extern "system" fn gi_unregister(this: *mut c_void, token: u64, timeout: u64) -> u32 {
    host::count(13);
    if regs_remove(&mut REGS.lock().unwrap_or_else(|e| e.into_inner()), token) {
        log("UnregisterCallback", &format!("token={token:#x} gamepad listener removed"));
    }
    let f: unsafe extern "system" fn(*mut c_void, u64, u64) -> u32 = core::mem::transmute(orig(13));
    f(this, token, timeout)
}

unsafe extern "system" fn gi_find_id(this: *mut c_void, id: *const [u8; 32], out: *mut *mut c_void) -> i32 {
    host::count(16);
    if !id.is_null() && !out.is_null() && *id == DEVICE_ID {
        *out = vdev();
        return S_OK;
    }
    let f: unsafe extern "system" fn(*mut c_void, *const [u8; 32], *mut *mut c_void) -> i32 = core::mem::transmute(orig(16));
    f(this, id, out)
}

unsafe extern "system" fn gi_find_obj(this: *mut c_void, obj: *mut c_void, out: *mut *mut c_void) -> i32 {
    host::count(17);
    if obj == vdev() && !out.is_null() {
        *out = vdev();
        return S_OK;
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(orig(17));
    f(this, obj, out)
}

const IGAMEINPUT_SLOTS: usize = 22;
static mut GI_VTBL: [usize; 32] = [0; 32];

#[repr(C)]
struct Standin {
    vtbl: *const usize,
}

static mut STANDIN_VTBL: [usize; IGAMEINPUT_SLOTS] = [0; IGAMEINPUT_SLOTS];
static mut STANDIN: Standin = Standin { vtbl: core::ptr::null() };
static STANDIN_INIT: std::sync::Once = std::sync::Once::new();
static STANDIN_REFS: AtomicU32 = AtomicU32::new(1);
static STANDIN_TOKENS: AtomicU64 = AtomicU64::new(0x5442_0000_0000_0001);
static STANDIN_LIVE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn standin_token(token: *mut u64) -> i32 {
    if token.is_null() {
        return E_INVALIDARG;
    }
    let t = STANDIN_TOKENS.fetch_add(1, Ordering::SeqCst);
    STANDIN_LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(t);
    unsafe { *token = t };
    S_OK
}

fn standin_forget(token: u64) -> bool {
    let mut live = STANDIN_LIVE.lock().unwrap_or_else(|e| e.into_inner());
    match live.iter().position(|t| *t == token) {
        Some(i) => {
            live.swap_remove(i);
            true
        }
        None => false,
    }
}

unsafe extern "system" fn sd_qi(this: *mut c_void, _iid: *const u8, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    sd_addref(this);
    *out = this;
    S_OK
}
unsafe extern "system" fn sd_addref(_this: *mut c_void) -> u32 {
    STANDIN_REFS.fetch_add(1, Ordering::SeqCst) + 1
}
unsafe extern "system" fn sd_release(_this: *mut c_void) -> u32 {
    let prev = STANDIN_REFS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1).max(1))).unwrap_or(1);
    prev.saturating_sub(1).max(1)
}
unsafe extern "system" fn sd_timestamp(_this: *mut c_void) -> u64 {
    qpc_us()
}
unsafe extern "system" fn sd_reading4(_this: *mut c_void, _a: usize, _b: *mut c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    *out = core::ptr::null_mut();
    READING_NOT_FOUND
}
unsafe extern "system" fn sd_reading5(_this: *mut c_void, _r: *mut c_void, _kind: u32, _d: *mut c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    *out = core::ptr::null_mut();
    READING_NOT_FOUND
}
unsafe extern "system" fn sd_reading_cb(_this: *mut c_void, _d: *mut c_void, _kind: u32, _t: f32, _ctx: *mut c_void, _cb: usize, token: *mut u64) -> i32 {
    if !token.is_null() {
        *token = 0;
    }
    E_NOTIMPL
}
unsafe extern "system" fn sd_device_cb(_this: *mut c_void, _d: *mut c_void, _kind: u32, _f: u32, _e: u32, _ctx: *mut c_void, _cb: usize, token: *mut u64) -> i32 {
    standin_token(token)
}
unsafe extern "system" fn sd_button_cb(_this: *mut c_void, _d: *mut c_void, _f: u32, _ctx: *mut c_void, _cb: usize, token: *mut u64) -> i32 {
    standin_token(token)
}
unsafe extern "system" fn sd_layout_cb(_this: *mut c_void, _d: *mut c_void, _ctx: *mut c_void, _cb: usize, token: *mut u64) -> i32 {
    standin_token(token)
}
unsafe extern "system" fn sd_stop(_this: *mut c_void, token: u64) {
    standin_forget(token);
}
unsafe extern "system" fn sd_unregister(_this: *mut c_void, token: u64, _timeout: u64) -> u32 {
    standin_forget(token) as u32
}
unsafe extern "system" fn sd_create2(_this: *mut c_void, out: *mut *mut c_void) -> i32 {
    if !out.is_null() {
        *out = core::ptr::null_mut();
    }
    E_NOTIMPL
}
unsafe extern "system" fn sd_create3(_this: *mut c_void, _kind: u32, out: *mut *mut c_void) -> i32 {
    if !out.is_null() {
        *out = core::ptr::null_mut();
    }
    E_NOTIMPL
}
unsafe extern "system" fn sd_find(_this: *mut c_void, _key: *const c_void, out: *mut *mut c_void) -> i32 {
    if out.is_null() {
        return 0x8000_4003u32 as i32;
    }
    *out = core::ptr::null_mut();
    E_NOTFOUND
}
unsafe extern "system" fn sd_oem(_this: *mut c_void, _vid: u16, _pid: u16, _iface: u8, _cat: u8) -> i32 {
    S_OK
}
unsafe extern "system" fn sd_focus(_this: *mut c_void, _policy: u32) {}

fn standin() -> *mut c_void {
    STANDIN_INIT.call_once(|| unsafe {
        let vt = &mut *core::ptr::addr_of_mut!(STANDIN_VTBL);
        let slots: [usize; IGAMEINPUT_SLOTS] = [
            sd_qi as usize,
            sd_addref as usize,
            sd_release as usize,
            sd_timestamp as usize,
            sd_reading4 as usize,
            sd_reading5 as usize,
            sd_reading5 as usize,
            sd_reading4 as usize,
            sd_reading_cb as usize,
            sd_device_cb as usize,
            sd_button_cb as usize,
            sd_layout_cb as usize,
            sd_stop as usize,
            sd_unregister as usize,
            sd_create2 as usize,
            sd_create3 as usize,
            sd_find as usize,
            sd_find as usize,
            sd_find as usize,
            sd_find as usize,
            sd_oem as usize,
            sd_focus as usize,
        ];
        *vt = slots;
        (*core::ptr::addr_of_mut!(STANDIN)).vtbl = vt.as_ptr();
    });
    core::ptr::addr_of_mut!(STANDIN) as *mut c_void
}

unsafe fn adopt_standin(out: *mut *mut c_void) -> bool {
    if out.is_null() {
        return false;
    }
    let obj = standin();
    patch_object(obj);
    if REAL_THIS.load(Ordering::SeqCst) != obj as usize {
        return false;
    }
    *out = obj;
    true
}

unsafe fn patch_object(obj: *mut c_void) {
    let vptr = obj as *mut *const usize;
    let cur = *vptr as usize;
    let ours = core::ptr::addr_of!(GI_VTBL) as usize;
    if cur == ours {
        return;
    }
    let known = REAL_VTBL.load(Ordering::SeqCst);
    if known == 0 {
        let real = cur as *const usize;
        let vt = &mut *core::ptr::addr_of_mut!(GI_VTBL);
        for s in vt.iter_mut() {
            *s = ret_notimpl as usize;
        }
        for i in 0..IGAMEINPUT_SLOTS {
            let p = *real.add(i);
            vt[i] = p;
            ORIG[i].store(p, Ordering::SeqCst);
        }
        vt[4] = gi_current as usize;
        vt[5] = gi_next as usize;
        vt[6] = gi_prev as usize;
        vt[7] = gi_temporal as usize;
        vt[8] = gi_reading_cb as usize;
        vt[9] = gi_device_cb as usize;
        vt[12] = gi_stop as usize;
        vt[13] = gi_unregister as usize;
        vt[16] = gi_find_id as usize;
        vt[17] = gi_find_obj as usize;
        REAL_VTBL.store(cur, Ordering::SeqCst);
    } else if known != cur {
        log("GameInputCreate", &format!("object {obj:p} has an unexpected vtable {cur:#x}; virtual pad not attached to it"));
        return;
    }
    REAL_THIS.store(obj as usize, Ordering::SeqCst);
    *vptr = ours as *const usize;
    log("GameInputCreate", &format!("IGameInput {obj:p} wrapped (virtual Xbox gamepad attached)"));
}

unsafe extern "system" fn create_hook(out: *mut *mut c_void) -> i32 {
    let f: unsafe extern "system" fn(*mut *mut c_void) -> i32 = core::mem::transmute(ORIG_CREATE.load(Ordering::SeqCst));
    let hr = f(out);
    log("GameInputCreate", &format!("real GameInputCreate in {} hr={hr:#x}", host::module_of(ORIG_CREATE.load(Ordering::SeqCst))));
    if hr >= 0 && !out.is_null() && !(*out).is_null() {
        patch_object(*out);
        start_channel();
        crate::pad_host::start();
    } else {
        log("GameInputCreate", &format!("real GameInputCreate failed hr={hr:#x}"));
        if adopt_standin(out) {
            start_channel();
            crate::pad_host::start();
            log("GameInputCreate", &format!("no host GameInput (hr={hr:#x}): returned a stand-in IGameInput with no host devices and the virtual Xbox gamepad, hr=0 as on console"));
            return S_OK;
        }
    }
    hr
}

fn eq_ascii_nocase(p: *const u8, s: &[u8]) -> bool {
    unsafe {
        for (i, c) in s.iter().enumerate() {
            if (*p.add(i)).to_ascii_lowercase() != *c {
                return false;
            }
        }
        *p.add(s.len()) == 0
    }
}

pub unsafe fn find_import_slot(base: *const u8, dll: &[u8], ordinal: u16, name: &[u8]) -> Option<*mut usize> {
    if core::ptr::read_unaligned(base as *const u16) != 0x5A4D {
        return None;
    }
    let nt = base.add(core::ptr::read_unaligned(base.add(0x3c) as *const u32) as usize);
    if core::ptr::read_unaligned(nt as *const u32) != 0x4550 {
        return None;
    }
    let opt = nt.add(24);
    if core::ptr::read_unaligned(opt as *const u16) != 0x20b {
        return None;
    }
    let imp_rva = core::ptr::read_unaligned(opt.add(112 + 8) as *const u32) as usize;
    if imp_rva == 0 {
        return None;
    }
    let mut d = base.add(imp_rva);
    loop {
        let oft = core::ptr::read_unaligned(d as *const u32) as usize;
        let name_rva = core::ptr::read_unaligned(d.add(12) as *const u32) as usize;
        let ft = core::ptr::read_unaligned(d.add(16) as *const u32) as usize;
        if name_rva == 0 && ft == 0 {
            return None;
        }
        if eq_ascii_nocase(base.add(name_rva), dll) {
            let lookup = base.add(if oft != 0 { oft } else { ft }) as *const u64;
            let mut i = 0;
            loop {
                let e = core::ptr::read_unaligned(lookup.add(i));
                if e == 0 {
                    break;
                }
                let hit = if e & (1 << 63) != 0 {
                    (e & 0xffff) as u16 == ordinal
                } else {
                    let p = base.add((e & 0x7fff_ffff) as usize + 2);
                    eq_ascii_nocase_exact(p, name)
                };
                if hit {
                    return Some((base.add(ft) as *mut usize).add(i));
                }
                i += 1;
            }
        }
        d = d.add(20);
    }
}

fn eq_ascii_nocase_exact(p: *const u8, s: &[u8]) -> bool {
    unsafe {
        for (i, c) in s.iter().enumerate() {
            if *p.add(i) != *c {
                return false;
            }
        }
        *p.add(s.len()) == 0
    }
}

pub fn ensure() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe { install() });
}

unsafe fn install() {
    if std::env::var("TOMBOX_PAD").ok().as_deref() == Some("0") {
        return;
    }
    let marker = format!("{}", GetCurrentProcessId());
    if std::env::var("TOMBOX_PAD_OWNER").ok().as_deref() == Some(marker.as_str()) {
        return;
    }
    std::env::set_var("TOMBOX_PAD_OWNER", &marker);
    let base = GetModuleHandleW(core::ptr::null());
    if base.is_null() {
        return;
    }
    let Some(slot) = find_import_slot(base, b"gameinput.dll", 1, b"GameInputCreate") else {
        log("install", "host EXE does not import GameInputCreate - virtual gamepad not installed");
        return;
    };
    init_device();
    init_reading_vtbl();
    if std::env::var("TOMBOX_PAD_BOOT").ok().as_deref() == Some("1") {
        CONNECTED.store(true, Ordering::SeqCst);
    }
    let mut old = 0u32;
    if VirtualProtect(slot as *mut c_void, 8, 0x04, &mut old) == 0 {
        log("install", "VirtualProtect on the GameInput import slot failed - virtual gamepad not installed");
        return;
    }
    let prev = core::ptr::read_volatile(slot);
    ORIG_CREATE.store(prev, Ordering::SeqCst);
    core::ptr::write_volatile(slot, create_hook as usize);
    let mut tmp = 0u32;
    VirtualProtect(slot as *mut c_void, 8, old, &mut tmp);
    log("install", &format!("GameInputCreate import (slot EXE+{:#x}) hooked; virtual Xbox gamepad will attach to IGameInput ({}); control dir {}",
        slot as usize - base as usize,
        if CONNECTED.load(Ordering::SeqCst) { "plugged in at boot (TOMBOX_PAD_BOOT=1)" } else { "unplugged until the first pad command" },
        channel_dir().display()));
}

pub fn channel_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("TOMBOX_PAD_DIR") {
        return std::path::PathBuf::from(p);
    }
    let mut d = std::env::temp_dir();
    d.push("tombox");
    d.push("pad");
    d
}

fn start_channel() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let _ = std::thread::Builder::new().name("tombox-pad".into()).spawn(channel_loop);
    });
}

fn wait_polls(since: u64, n: u64, limit: Duration) {
    let t0 = Instant::now();
    while POLLS.load(Ordering::SeqCst) < since + n && t0.elapsed() < limit {
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn hold_for(ms: u64) {
    let p = POLLS.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(ms));
    wait_polls(p, 2, Duration::from_secs(3));
}

fn run(cmd: &Cmd) {
    match cmd {
        Cmd::Press(inputs, ms) => {
            update("press down", |s| inputs.iter().for_each(|i| s.apply(*i, true)));
            hold_for(*ms);
            update("press up", |s| inputs.iter().for_each(|i| s.apply(*i, false)));
            hold_for(GAP_MS);
        }
        Cmd::Hold(inputs, ms) => {
            update("hold down", |s| inputs.iter().for_each(|i| s.apply(*i, true)));
            hold_for(*ms);
            update("hold up", |s| inputs.iter().for_each(|i| s.apply(*i, false)));
            hold_for(GAP_MS);
        }
        Cmd::Down(inputs) => update("down", |s| inputs.iter().for_each(|i| s.apply(*i, true))),
        Cmd::Up(inputs) => update("up", |s| inputs.iter().for_each(|i| s.apply(*i, false))),
        Cmd::Stick(left, x, y, ms) => {
            let (ix, iy) = if *left { (AX_LX, AX_LY) } else { (AX_RX, AX_RY) };
            update("stick", |s| {
                s.axes[ix] = *x;
                s.axes[iy] = *y;
            });
            if let Some(ms) = ms {
                hold_for(*ms);
                update("stick center", |s| {
                    s.axes[ix] = 0.0;
                    s.axes[iy] = 0.0;
                });
                hold_for(GAP_MS);
            }
        }
        Cmd::Trigger(i, v, ms) => {
            update("trigger", |s| s.axes[*i] = *v);
            if let Some(ms) = ms {
                hold_for(*ms);
                update("trigger release", |s| s.axes[*i] = 0.0);
                hold_for(GAP_MS);
            }
        }
        Cmd::Wait(ms) => std::thread::sleep(Duration::from_millis(*ms)),
        Cmd::Release => update("release", |s| *s = PadState::default()),
        Cmd::Connect => plug_in(),
        Cmd::Disconnect => set_connected(false),
    }
}

fn plug_in() {
    if CONNECTED.load(Ordering::SeqCst) {
        return;
    }
    set_connected(true);
    let p = POLLS.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(500));
    wait_polls(p, 2, Duration::from_secs(3));
    log("connect", &format!("title has read the virtual pad {} time(s) since plug-in", POLLS.load(Ordering::SeqCst) - p));
}

fn channel_loop() {
    let dir = channel_dir();
    let _ = std::fs::create_dir_all(&dir);
    log("channel", &format!("polling {} for *.cmd files", dir.display()));
    loop {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("cmd")).unwrap_or(false))
                .collect())
            .unwrap_or_default();
        files.sort();
        set_script_active(!files.is_empty());
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let fname = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            for line in text.lines() {
                match parse_command(line) {
                    Ok(Some(cmd)) => {
                        log("cmd", &format!("{fname}: {}", line.trim()));
                        if cmd != Cmd::Disconnect {
                            plug_in();
                        }
                        run(&cmd);
                    }
                    Ok(None) => {}
                    Err(e) => log("cmd", &format!("{fname}: {e}")),
                }
            }
            let _ = std::fs::remove_file(&f);
            log("cmd", &format!("{fname}: done (readings served so far: {})", POLLS.load(Ordering::SeqCst)));
        }
        set_script_active(false);
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_state_reaches_readings_and_their_history() {
        let mut s = PadState::default();
        s.buttons = 0x4;
        s.axes[2] = 0.5;
        let before = SEQ.load(Ordering::SeqCst);
        set_host(s);
        let r = new_reading() as *mut Reading;
        let (seq, state) = unsafe { ((*r).seq, (*r).state) };
        unsafe { drop(Box::from_raw(r)) };
        let step = HIST.lock().unwrap_or_else(|e| e.into_inner()).next(before, now_us());
        set_host(PadState::default());
        assert_eq!(state, s.to_bytes());
        match step {
            history::Step::Found(e) => {
                assert_eq!(e.state, s.to_bytes());
                assert!(e.seq > before && e.seq <= seq);
            }
            other => panic!("host change not in the reading history: {other:?}"),
        }
    }

    unsafe fn slot<T>(obj: *mut c_void, i: usize) -> T {
        let vt = *(obj as *const *const usize);
        core::mem::transmute_copy(&*vt.add(i))
    }

    #[test]
    fn failed_create_hands_out_a_standin_gameinput_with_the_virtual_pad() {
        unsafe {
            let mut obj: *mut c_void = core::ptr::null_mut();
            assert!(adopt_standin(&mut obj));
            assert_eq!(obj, standin());
            assert_eq!(*(obj as *const usize), core::ptr::addr_of!(GI_VTBL) as usize);
            let mut again: *mut c_void = core::ptr::null_mut();
            assert!(adopt_standin(&mut again));
            assert_eq!(again, obj);
            assert!(!adopt_standin(core::ptr::null_mut()));

            let qi: unsafe extern "system" fn(*mut c_void, *const u8, *mut *mut c_void) -> i32 = slot(obj, 0);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = slot(obj, 2);
            let mut same: *mut c_void = core::ptr::null_mut();
            assert_eq!(qi(obj, [0u8; 16].as_ptr(), &mut same), S_OK);
            assert_eq!(same, obj);
            assert!(release(obj) >= 1);

            let ts: unsafe extern "system" fn(*mut c_void) -> u64 = slot(obj, 3);
            let t0 = ts(obj);
            let t1 = ts(obj);
            assert!(t0 > 0 && t1 >= t0);

            let current: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut *mut c_void) -> i32 = slot(obj, 4);
            let mut r = 1usize as *mut c_void;
            assert_eq!(current(obj, KIND_KEYBOARD, core::ptr::null_mut(), &mut r), READING_NOT_FOUND);
            assert!(r.is_null());

            let device_cb: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, u32, u32, *mut c_void, usize, *mut u64) -> i32 = slot(obj, 9);
            let mut token = 0u64;
            assert_eq!(device_cb(obj, core::ptr::null_mut(), KIND_KEYBOARD, STATUS_CONNECTED, 1, core::ptr::null_mut(), 0, &mut token), S_OK);
            assert_ne!(token, 0);
            let button_cb: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, usize, *mut u64) -> i32 = slot(obj, 10);
            let mut token2 = 0u64;
            assert_eq!(button_cb(obj, core::ptr::null_mut(), 1, core::ptr::null_mut(), 0, &mut token2), S_OK);
            assert!(token2 != 0 && token2 != token);
            let reading_cb: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, f32, *mut c_void, usize, *mut u64) -> i32 = slot(obj, 8);
            let mut token3 = 9u64;
            assert_eq!(reading_cb(obj, core::ptr::null_mut(), KIND_GAMEPAD, 0.0, core::ptr::null_mut(), 0, &mut token3), E_NOTIMPL);
            assert_eq!(token3, 0);
            let stop: unsafe extern "system" fn(*mut c_void, u64) = slot(obj, 12);
            stop(obj, token);
            let unregister: unsafe extern "system" fn(*mut c_void, u64, u64) -> u32 = slot(obj, 13);
            assert_eq!(unregister(obj, token, 0), 0);
            assert_eq!(unregister(obj, token2, 0), 1);
            assert_eq!(unregister(obj, token2, 0), 0);
            assert_eq!(unregister(obj, 0x1234, 0), 0);
            assert_eq!(device_cb(obj, core::ptr::null_mut(), KIND_KEYBOARD, STATUS_CONNECTED, 1, core::ptr::null_mut(), 0, core::ptr::null_mut()), E_INVALIDARG);

            let dispatcher: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32 = slot(obj, 14);
            let mut d = 1usize as *mut c_void;
            assert_eq!(dispatcher(obj, &mut d), E_NOTIMPL);
            assert!(d.is_null());

            let find_id: unsafe extern "system" fn(*mut c_void, *const [u8; 32], *mut *mut c_void) -> i32 = slot(obj, 16);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert_eq!(find_id(obj, &DEVICE_ID, &mut dev), S_OK);
            assert_eq!(dev, vdev());
            let other = [7u8; 32];
            assert_eq!(find_id(obj, &other, &mut dev), E_NOTFOUND);
            assert!(dev.is_null());
            let find_obj: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> i32 = slot(obj, 17);
            assert_eq!(find_obj(obj, vdev(), &mut dev), S_OK);
            assert_eq!(dev, vdev());

            let oem: unsafe extern "system" fn(*mut c_void, u16, u16, u8, u8) -> i32 = slot(obj, 20);
            assert_eq!(oem(obj, 0x045e, 0x0b12, 0, 1), S_OK);
            let focus: unsafe extern "system" fn(*mut c_void, u32) = slot(obj, 21);
            focus(obj, 0);
            assert!(now_us() > 0);
        }
    }

    #[test]
    fn parses_press_with_default_and_explicit_ms() {
        assert_eq!(parse_command("press A").unwrap(), Some(Cmd::Press(vec![Input::Button(BTN_A)], DEFAULT_PRESS_MS)));
        assert_eq!(parse_command("press down 300").unwrap(), Some(Cmd::Press(vec![Input::Button(BTN_DOWN)], 300)));
        assert_eq!(parse_command("  tap dpad_left 50ms ").unwrap(), Some(Cmd::Press(vec![Input::Button(BTN_LEFT)], 50)));
    }

    #[test]
    fn parses_hold_combos_and_sticks() {
        assert_eq!(parse_command("hold LS_down 300").unwrap(), Some(Cmd::Hold(vec![Input::Axis(AX_LY, -1.0)], 300)));
        assert_eq!(parse_command("press lb+rb").unwrap(), Some(Cmd::Press(vec![Input::Button(BTN_LB), Input::Button(BTN_RB)], DEFAULT_PRESS_MS)));
        assert_eq!(parse_command("stick L 0.5 -2").unwrap(), Some(Cmd::Stick(true, 0.5, -1.0, None)));
        assert_eq!(parse_command("stick r 1 0 200").unwrap(), Some(Cmd::Stick(false, 1.0, 0.0, Some(200))));
        assert_eq!(parse_command("trigger rt 0.7 100").unwrap(), Some(Cmd::Trigger(AX_RT, 0.7, Some(100))));
        assert_eq!(parse_command("wait 250").unwrap(), Some(Cmd::Wait(250)));
        assert_eq!(parse_command("release").unwrap(), Some(Cmd::Release));
    }

    #[test]
    fn parses_plug_commands_and_tracks_registrations() {
        assert_eq!(parse_command("connect").unwrap(), Some(Cmd::Connect));
        assert_eq!(parse_command("unplug").unwrap(), Some(Cmd::Disconnect));
        let mut l = Vec::new();
        regs_add(&mut l, Registration { token: 7, ctx: 1, cb: 2 });
        regs_add(&mut l, Registration { token: 9, ctx: 3, cb: 4 });
        regs_add(&mut l, Registration { token: 7, ctx: 5, cb: 6 });
        assert_eq!(l.len(), 2);
        assert_eq!(l[1], Registration { token: 7, ctx: 5, cb: 6 });
        assert!(regs_remove(&mut l, 9));
        assert!(!regs_remove(&mut l, 9));
        assert_eq!(l.len(), 1);
        for t in 100..200 {
            regs_add(&mut l, Registration { token: t, ctx: 0, cb: 0 });
        }
        assert_eq!(l.len(), 64);
    }

    #[test]
    fn ignores_blank_and_rejects_garbage() {
        assert_eq!(parse_command("").unwrap(), None);
        assert_eq!(parse_command("# note").unwrap(), None);
        assert!(parse_command("press Q").is_err());
        assert!(parse_command("hold A").is_err());
        assert!(parse_command("jump").is_err());
        assert!(parse_command("stick L x 0").is_err());
    }

    #[test]
    fn state_applies_and_serializes_like_gameinput_gamepad_state() {
        let mut s = PadState::default();
        s.apply(Input::Button(BTN_A), true);
        s.apply(Input::Button(BTN_MENU), true);
        s.apply(Input::Axis(AX_LY, -1.0), true);
        s.apply(Input::Axis(AX_RT, 1.0), true);
        s.apply(Input::Button(BTN_MENU), false);
        assert_eq!(s.buttons, BTN_A);
        let b = s.to_bytes();
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), BTN_A);
        assert_eq!(f32::from_le_bytes(b[8..12].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(b[16..20].try_into().unwrap()), -1.0);
        s.apply(Input::Axis(AX_LY, -1.0), false);
        assert_eq!(s.axes[AX_LY], 0.0);
    }

    #[test]
    fn finds_import_slot_in_synthetic_pe() {
        let mut img = vec![0u8; 0x1000];
        img[0] = b'M';
        img[1] = b'Z';
        img[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        img[0x80..0x84].copy_from_slice(&0x4550u32.to_le_bytes());
        let opt = 0x80 + 24;
        img[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        img[opt + 120..opt + 124].copy_from_slice(&0x200u32.to_le_bytes());
        let put32 = |img: &mut Vec<u8>, at: usize, v: u32| img[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put32(&mut img, 0x200, 0x300);
        put32(&mut img, 0x20c, 0x400);
        put32(&mut img, 0x210, 0x500);
        img[0x300..0x308].copy_from_slice(&0x600u64.to_le_bytes());
        img[0x308..0x310].copy_from_slice(&((1u64 << 63) | 1).to_le_bytes());
        img[0x400..0x40d].copy_from_slice(b"GameInput.dll");
        img[0x602..0x60b].copy_from_slice(b"SomethingElse"[..9].as_ref());
        let slot = unsafe { find_import_slot(img.as_ptr(), b"gameinput.dll", 1, b"GameInputCreate") }.unwrap();
        assert_eq!(slot as usize - img.as_ptr() as usize, 0x508);
        let none = unsafe { find_import_slot(img.as_ptr(), b"xinput1_4.dll", 1, b"XInputGetState") };
        assert!(none.is_none());
    }
}
