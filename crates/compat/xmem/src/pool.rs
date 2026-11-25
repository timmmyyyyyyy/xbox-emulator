use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::sys::*;

pub const SHIFT_SMALL: u32 = 0x13;
pub const SHIFT_LARGE: u32 = 0x15;
const POOL_FLAGS: [u32; 16] = [
    0, 0x1000_0000, 0x1000_0000, 0x1000_0000, 0x1000_0000, 0x1000_0000, 0x1000_0000, 0x40_0000,
    0x40_0000, 0x40_0000, 0x1000_0000, 0x1000_0000, 0x1000_0000, 0, 0, 0,
];
const BIG_POOL: u32 = 0x1000_0000;
const BIG_HYSTERESIS: u64 = 0x400_0000;
const SMALL_HYSTERESIS: u64 = 0x80_0000;

struct Pool {
    hysteresis: u32,
    free: Vec<usize>,
    low: u32,
    released: u32,
}

static POOLS: Mutex<Vec<Option<Pool>>> = Mutex::new(Vec::new());
static LARGE_GRANULE: AtomicU32 = AtomicU32::new(0);
static SHIFT: AtomicU32 = AtomicU32::new(0);

pub fn set_large_granule(large: bool) -> bool {
    if SHIFT.load(Ordering::Acquire) != 0 {
        return false;
    }
    LARGE_GRANULE.store(large as u32, Ordering::Release);
    true
}

pub fn shift() -> u32 {
    let s = SHIFT.load(Ordering::Acquire);
    if s != 0 {
        return s;
    }
    let want = shift_for(compat_common::os_host::routes::current_build(), LARGE_GRANULE.load(Ordering::Acquire) != 0);
    match SHIFT.compare_exchange(0, want, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => want,
        Err(v) => v,
    }
}

pub const SHIFT_19041: u32 = 0x16;
const FIRST_POOL_OPTION_BUILD: u32 = 22000;

pub fn shift_for(os_build: u32, large: bool) -> u32 {
    if os_build < FIRST_POOL_OPTION_BUILD {
        SHIFT_19041
    } else if large {
        SHIFT_LARGE
    } else {
        SHIFT_SMALL
    }
}

pub fn granule() -> usize {
    1usize << shift()
}

fn with<R>(index: usize, f: impl FnOnce(&mut Pool, u32) -> R) -> R {
    let mut g = POOLS.lock().unwrap_or_else(|p| p.into_inner());
    if g.len() < 32 {
        g.resize_with(32, || None);
    }
    let sh = shift();
    let p = g[index & 0x1f].get_or_insert_with(|| {
        let bytes = if POOL_FLAGS[index & 0xf] == BIG_POOL { BIG_HYSTERESIS } else { SMALL_HYSTERESIS };
        Pool { hysteresis: (bytes >> sh) as u32, free: Vec::new(), low: 0, released: 0 }
    });
    f(p, sh)
}

fn reported_hysteresis(pages: u32, shift: u32) -> u64 {
    if shift == SHIFT_19041 {
        u64::from(pages << shift)
    } else {
        u64::from(pages) << shift
    }
}

pub fn hysteresis_bytes(index: usize) -> u64 {
    with(index, |p, sh| reported_hysteresis(p.hysteresis, sh))
}

pub fn set_hysteresis(index: usize, bytes: usize) {
    with(index, |p, sh| {
        p.hysteresis = ((bytes as u64).wrapping_add((1u64 << sh) - 1) >> sh) as u32;
        while p.free.len() as u64 > p.hysteresis as u64 {
            let c = p.free.pop().unwrap();
            crate::epoch::bump_range(c, c + (1usize << sh));
            crate::write_watch::forget(c);
            unsafe { VirtualFree(c as *mut c_void, 0, MEM_RELEASE) };
            p.released = p.released.wrapping_add(1);
        }
        p.low = p.free.len() as u32;
    })
}

pub fn preallocate(index: usize) -> bool {
    with(index, |p, sh| {
        let gran = 1usize << sh;
        let mut ok = true;
        while (p.free.len() as u64) < p.hysteresis as u64 {
            let c = crate::write_watch::reserve(gran, gran, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
            if c.is_null() {
                ok = false;
                break;
            }
            p.free.push(c as usize);
        }
        p.low = p.free.len() as u32;
        ok
    })
}

pub fn take(index: usize, len: usize, align: usize) -> Option<*mut c_void> {
    if len != granule() || granule() % align.max(1) != 0 {
        return None;
    }
    with(index, |p, _| {
        let c = p.free.pop()?;
        p.low = p.low.min(p.free.len() as u32);
        Some(c as *mut c_void)
    })
}

pub fn give(index: usize, addr: usize, len: usize) -> bool {
    if len != granule() || addr & (len - 1) != 0 {
        return false;
    }
    with(index, |p, _| {
        if (p.free.len() as u64) < p.hysteresis as u64 {
            p.free.push(addr);
            true
        } else {
            p.released = p.released.wrapping_add(1);
            false
        }
    })
}

pub fn cached_low_bytes(index: usize) -> u64 {
    with(index, |p, sh| (p.low as u64) << sh)
}

pub fn released(index: usize) -> u32 {
    with(index, |p, _| p.released)
}

pub fn cached_count(index: usize) -> usize {
    with(index, |p, _| p.free.len())
}

pub fn total_cached_bytes() -> u64 {
    let sh = SHIFT.load(Ordering::Acquire);
    if sh == 0 {
        return 0;
    }
    let g = POOLS.lock().unwrap_or_else(|p| p.into_inner());
    g.iter().flatten().map(|p| (p.free.len() as u64) << sh).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed(addr: usize) -> bool {
        let mut m = MemoryBasicInformation::default();
        unsafe { VirtualQueryEx(GetCurrentProcess(), addr as *const c_void, &mut m, core::mem::size_of::<MemoryBasicInformation>()) };
        m.state == MEM_COMMIT
    }

    #[test]
    fn granule_follows_console_os_build() {
        assert_eq!(shift_for(19041, false), 0x16);
        assert_eq!(shift_for(19041, true), 0x16);
        assert_eq!(shift_for(22000, false), 0x13);
        assert_eq!(shift_for(22621, true), 0x15);
        assert_eq!((BIG_HYSTERESIS >> 0x16, SMALL_HYSTERESIS >> 0x16), (16, 2));
    }

    #[test]
    fn hysteresis_report_width_at_four_gib() {
        assert_eq!(reported_hysteresis(1023, SHIFT_19041), 0xffc0_0000);
        assert_eq!(reported_hysteresis(1024, SHIFT_19041), 0);
        assert_eq!(reported_hysteresis(1025, SHIFT_19041), 0x40_0000);
        assert_eq!(reported_hysteresis(8192, SHIFT_SMALL), 1u64 << 32);
        assert_eq!(reported_hysteresis(2048, SHIFT_LARGE), 1u64 << 32);
    }

    #[test]
    fn defaults_follow_console_pool_table() {
        assert_eq!(hysteresis_bytes(1), 0x400_0000);
        assert_eq!(shift(), SHIFT_SMALL);
        assert!(!set_large_granule(true));
        assert_eq!(shift(), SHIFT_SMALL);
        assert_eq!(hysteresis_bytes(0x18), 0x80_0000);
        assert_eq!(hysteresis_bytes(0), 0x80_0000);
        assert_eq!(hysteresis_bytes(0x1c), 0x400_0000);
        assert_eq!(hysteresis_bytes(13), 0x80_0000);
    }

    #[test]
    fn preallocate_fills_cache_and_trim_releases() {
        let i = 0x14;
        set_hysteresis(i, 3 * granule() - 1);
        assert_eq!(hysteresis_bytes(i), 3 * granule() as u64);
        assert!(preallocate(i));
        assert_eq!(cached_count(i), 3);
        assert_eq!(cached_low_bytes(i), 3 * granule() as u64);
        let a = take(i, granule(), 0x1_0000).unwrap() as usize;
        assert_eq!(a % granule(), 0);
        assert!(committed(a) && committed(a + granule() - 1));
        assert_eq!(cached_low_bytes(i), 2 * granule() as u64);
        assert!(take(i, granule() / 2, 0x1000).is_none());
        assert!(give(i, a, granule()));
        assert_eq!(cached_count(i), 3);
        assert_eq!(cached_low_bytes(i), 2 * granule() as u64);
        let before = released(i);
        let extra = reserve_aligned(granule(), granule(), MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) as usize;
        assert!(!give(i, extra, granule()));
        assert_eq!(released(i), before + 1);
        assert_eq!(unsafe { VirtualFree(extra as *mut c_void, 0, MEM_RELEASE) }, 1);
        set_hysteresis(i, granule());
        assert_eq!(cached_count(i), 1);
        assert_eq!(released(i), before + 3);
        assert_eq!(cached_low_bytes(i), granule() as u64);
        set_hysteresis(i, 0);
        assert_eq!(cached_count(i), 0);
        assert!(preallocate(i));
        assert_eq!(cached_count(i), 0);
    }
}
