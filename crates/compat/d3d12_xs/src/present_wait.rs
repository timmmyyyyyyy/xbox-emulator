use core::ffi::c_void;

extern "system" {
    fn CreateEventW(sa: *mut c_void, manual: i32, initial: i32, name: *const u16) -> isize;
    fn WaitForSingleObject(h: isize, ms: u32) -> u32;
}

thread_local! {
    static EV: isize = unsafe { CreateEventW(core::ptr::null_mut(), 0, 0, core::ptr::null()) };
}

pub unsafe fn wait_fence(fence: *mut c_void, value: u64, timeout_ms: u32) -> bool {
    let vt = *(fence as *const *const usize);
    let completed: unsafe extern "system" fn(*mut c_void) -> u64 = core::mem::transmute(*vt.add(8));
    let current = completed(fence);
    if current >= value {
        return current != u64::MAX;
    }
    let set_event: unsafe extern "system" fn(*mut c_void, u64, isize) -> i32 = core::mem::transmute(*vt.add(9));
    let ev = EV.with(|e| *e);
    if ev == 0 || set_event(fence, value, ev) < 0 {
        let t0 = std::time::Instant::now();
        loop {
            let current = completed(fence);
            if current >= value {
                return current != u64::MAX;
            }
            if t0.elapsed().as_millis() as u32 > timeout_ms {
                return false;
            }
            std::thread::yield_now();
        }
    }
    WaitForSingleObject(ev, timeout_ms);
    let current = completed(fence);
    current != u64::MAX && current >= value
}


#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU64, Ordering};

    #[repr(C)]
    struct Fence {
        vtable: *const usize,
        value: AtomicU64,
    }

    unsafe extern "system" fn completed(this: *mut c_void) -> u64 {
        (*(this.cast::<Fence>())).value.load(Ordering::Relaxed)
    }

    unsafe extern "system" fn remove_while_waiting(this: *mut c_void, _: u64, _: isize) -> i32 {
        (*(this.cast::<Fence>())).value.store(u64::MAX, Ordering::Relaxed);
        0x8000_4005u32 as i32
    }

    #[test]
    fn device_removal_does_not_complete_a_fence() {
        let mut vtable = [0usize; 10];
        vtable[8] = completed as *const () as usize;
        vtable[9] = remove_while_waiting as *const () as usize;
        let mut fence = Fence { vtable: vtable.as_ptr(), value: AtomicU64::new(9) };
        let raw = (&mut fence as *mut Fence).cast();
        unsafe {
            assert!(wait_fence(raw, 9, 1));
            assert!(!wait_fence(raw, 10, 1));
            assert!(!wait_fence(raw, 9, 1));
        }
    }
}
