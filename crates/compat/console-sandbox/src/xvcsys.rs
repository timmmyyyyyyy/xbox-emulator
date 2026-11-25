use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

pub const IOCTL_POWER_STATE: u32 = 0x15004c;
pub const IOCTL_SFR_COUNTER_GROUP: u32 = 0x150064;
pub const IOCTL_MODE_SFR_COUNTER: u32 = 0x150068;
pub const IOCTL_PIX_STATE: u32 = 0x15006c;
pub const IOCTL_SLOT_DEQUEUE: u32 = 0x150084;
pub const IOCTL_SLOT_UNMAP: u32 = 0x150088;
pub const IOCTL_SLOT_REGISTER: u32 = 0x15008c;
pub const IOCTL_SLOT_UNREGISTER: u32 = 0x150090;
pub const IOCTL_AUDIO_FLAG: u32 = 0x150094;
pub const IOCTL_SFR_UPDATE: u32 = 0x1500b4;
pub const IOCTL_GAME_READY: u32 = 0x1500cc;
pub const IOCTL_PARTITION_INFO: u32 = 0x1500dc;
pub const IOCTL_TRANSFER_MEMORY: u32 = 0x1500e0;
pub const IOCTL_SFR_GET: u32 = 0x1500e8;

pub const SFR_TOOLS_FLAGS: u32 = 0x27;
pub const SFR_COUNTERS: u32 = 0x1ff;
pub const SLOTS: usize = 32;

const STATUS_NO_MORE_ENTRIES: i32 = 0x8000_001Au32 as i32;
const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;
const STATUS_INVALID_DEVICE_STATE: i32 = 0xC000_00BBu32 as i32;
const EVENT_MODIFY_STATE: u32 = 2;

use crate::xvcmgmt::{STATUS_BUFFER_TOO_SMALL, STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_HANDLE, STATUS_INVALID_PARAMETER, STATUS_NOT_FOUND, STATUS_SUCCESS};

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> usize;
    fn DuplicateHandle(sp: usize, sh: usize, tp: usize, th: *mut usize, access: u32, inherit: i32, opts: u32) -> i32;
    fn CloseHandle(h: usize) -> i32;
}

#[derive(Default)]
struct Slot {
    event: usize,
}

#[derive(Default)]
struct Sys {
    sfr: HashMap<u32, u64>,
    power: u32,
    pix: u32,
    audio: u32,
    slots: HashMap<u16, Slot>,
}

fn sys() -> MutexGuard<'static, Sys> {
    static S: OnceLock<Mutex<Sys>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Sys::default())).lock().unwrap_or_else(|e| e.into_inner())
}

pub fn sfr(counter: u32) -> u64 {
    sys().sfr.get(&counter).copied().unwrap_or(0)
}

pub fn slot_count() -> usize {
    sys().slots.len()
}

pub fn power_state() -> u32 {
    sys().power
}

pub fn pix_state() -> u32 {
    sys().pix
}

unsafe fn rd32(p: *const u8, off: usize) -> u32 {
    (p.add(off) as *const u32).read_unaligned()
}

unsafe fn rd64(p: *const u8, off: usize) -> u64 {
    (p.add(off) as *const u64).read_unaligned()
}

pub fn known(code: u32) -> bool {
    matches!(code, IOCTL_SFR_COUNTER_GROUP | IOCTL_POWER_STATE | IOCTL_MODE_SFR_COUNTER | IOCTL_PIX_STATE | IOCTL_SLOT_DEQUEUE | IOCTL_SLOT_UNMAP | IOCTL_SLOT_REGISTER | IOCTL_SLOT_UNREGISTER | IOCTL_AUDIO_FLAG | IOCTL_SFR_UPDATE | IOCTL_GAME_READY | IOCTL_PARTITION_INFO | IOCTL_TRANSFER_MEMORY | IOCTL_SFR_GET)
}

pub unsafe fn ioctl(code: u32, inb: *const u8, inl: u32, outb: *mut u8, outl: u32) -> (i32, usize) {
    let inb_ok = !inb.is_null();
    let outb_ok = !outb.is_null();
    match code {
        IOCTL_POWER_STATE => {
            if inl < 4 || !inb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            let v = rd32(inb, 0);
            let mut s = sys();
            s.power = v;
            if v == 1 {
                s.audio = 0;
            }
            (STATUS_SUCCESS, 0)
        }
        IOCTL_SFR_COUNTER_GROUP if compat_common::os_host::routes::current_build() == 19041 => {
            if outl < 33 || !outb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            for (i, counter) in (0x28..=0x2b).enumerate() {
                outb.add(8 * i).cast::<u64>().write_unaligned(sfr(counter));
            }
            outb.add(32).write(0);
            (STATUS_SUCCESS, 33)
        }
        IOCTL_MODE_SFR_COUNTER => {
            if outl < 8 || !outb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            (outb as *mut u64).write_unaligned(sfr(SFR_TOOLS_FLAGS));
            (STATUS_SUCCESS, 8)
        }
        IOCTL_PIX_STATE => {
            if inl < 4 || !inb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            sys().pix = rd32(inb, 0);
            (STATUS_SUCCESS, 0)
        }
        IOCTL_SLOT_REGISTER => {
            if inl != 8 || outl != 2 || !inb_ok || !outb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            let h = rd64(inb, 0) as usize;
            let mut dup = 0usize;
            if h == 0 || DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &mut dup, EVENT_MODIFY_STATE, 0, 0) == 0 {
                return (STATUS_INVALID_HANDLE, 0);
            }
            let mut s = sys();
            let Some(n) = (1..SLOTS as u16).find(|n| !s.slots.contains_key(n)) else {
                drop(s);
                CloseHandle(dup);
                return (STATUS_INSUFFICIENT_RESOURCES, 0);
            };
            s.slots.insert(n, Slot { event: dup });
            (outb as *mut u16).write_unaligned(n);
            (STATUS_SUCCESS, 2)
        }
        IOCTL_SLOT_DEQUEUE => {
            if outl < 0x10 || inl != 2 || !inb_ok || !outb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            core::ptr::write_bytes(outb, 0, 0x10);
            let n = (inb as *const u16).read_unaligned();
            if !sys().slots.contains_key(&n) {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            (STATUS_NO_MORE_ENTRIES, 0)
        }
        IOCTL_SLOT_UNMAP => {
            if inl < 0x10 || !inb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            (STATUS_NOT_FOUND, 0)
        }
        IOCTL_SLOT_UNREGISTER => {
            if inl != 2 || !inb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            let n = (inb as *const u16).read_unaligned();
            if let Some(s) = sys().slots.remove(&n) {
                CloseHandle(s.event);
            }
            (STATUS_SUCCESS, 0)
        }
        IOCTL_AUDIO_FLAG => {
            if outl < 4 || !outb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            (outb as *mut u32).write_unaligned(sys().audio);
            (STATUS_SUCCESS, 4)
        }
        IOCTL_SFR_UPDATE => {
            if inl % 16 != 0 || (inl != 0 && !inb_ok) {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            let mut s = sys();
            for i in 0..(inl / 16) as usize {
                let counter = rd32(inb, i * 16);
                let op = rd32(inb, i * 16 + 4) as i32;
                let value = rd64(inb, i * 16 + 8);
                if counter >= SFR_COUNTERS || op >= 3 {
                    return (STATUS_INVALID_PARAMETER, 0);
                }
                let e = s.sfr.entry(counter).or_insert(0);
                *e = match op {
                    0 => value,
                    1 => *e & value,
                    2 => *e | value,
                    _ => return (STATUS_INVALID_PARAMETER, 0),
                };
            }
            (STATUS_SUCCESS, 0)
        }
        IOCTL_GAME_READY => (STATUS_SUCCESS, 0),
        IOCTL_PARTITION_INFO => {
            if inl < 0xc || outl < 0xc || !inb_ok || !outb_ok {
                return (STATUS_BUFFER_TOO_SMALL, 0);
            }
            let class = rd32(inb, 4);
            let index = rd32(inb, 8);
            if !(1..=7).contains(&class) || index >= 3 {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            (STATUS_NOT_FOUND, 0)
        }
        IOCTL_TRANSFER_MEMORY => {
            if inl < 0xc || outl < 0xc || !inb_ok || !outb_ok {
                return (STATUS_BUFFER_TOO_SMALL, 0);
            }
            let class = rd32(inb, 4);
            let index = rd32(inb, 8);
            if !(1..=7).contains(&class) || index >= 3 {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            if class != 7 {
                return (STATUS_INVALID_DEVICE_STATE, 0);
            }
            if outl < 0x28 {
                return (STATUS_BUFFER_OVERFLOW, 4);
            }
            (STATUS_NOT_FOUND, 0)
        }
        IOCTL_SFR_GET => {
            if inl < 4 || outl < 8 || !inb_ok || !outb_ok {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            let c = rd32(inb, 0);
            if c >= SFR_COUNTERS {
                return (STATUS_INVALID_PARAMETER, 0);
            }
            (outb as *mut u64).write_unaligned(sfr(c));
            (STATUS_SUCCESS, 8)
        }
        _ => (STATUS_INVALID_DEVICE_REQUEST, 0),
    }
}
