use compat_common::record;

const PC_PRIORITY_NORMAL: u32 = 0;
const PC_PRIORITY_HIGH: u32 = 100;
const PC_PRIORITY_GLOBAL_REALTIME: u32 = 10000;
const PC_TYPE_COPY: u32 = 3;
const XBOX_TYPE_DMA: u32 = 0x10;
const PC_QUEUE_FLAGS: u32 = 0x1;

fn get(d: &[u8; 16], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn put(d: &mut [u8; 16], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn translate(d: &mut [u8; 16]) {
    let (ty, prio, flags, node) = (get(d, 0), get(d, 4), get(d, 8), get(d, 12));
    let pc_ty = if ty == XBOX_TYPE_DMA { PC_TYPE_COPY } else { ty };
    let pc_prio = match prio {
        PC_PRIORITY_NORMAL | PC_PRIORITY_HIGH | PC_PRIORITY_GLOBAL_REALTIME => prio,
        1 => PC_PRIORITY_HIGH,
        _ => PC_PRIORITY_HIGH,
    };
    let pc_node = if node == 0 { 0 } else { 1 };
    let pc_flags = flags & PC_QUEUE_FLAGS;
    put(d, 0, pc_ty);
    put(d, 4, pc_prio);
    put(d, 8, pc_flags);
    put(d, 12, pc_node);
    if (pc_ty, pc_prio, pc_flags, pc_node) != (ty, prio, flags, node) {
        record("d3d12_xs", "xone.queue", &format!(
            "Xbox queue desc type={ty:#x} priority={prio} flags={flags:#x} node={node:#x} -> PC type={pc_ty} priority={pc_prio} flags={pc_flags:#x} node={pc_node}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(ty: u32, prio: u32, flags: u32, node: u32) -> [u8; 16] {
        let mut d = [0u8; 16];
        put(&mut d, 0, ty);
        put(&mut d, 4, prio);
        put(&mut d, 8, flags);
        put(&mut d, 12, node);
        d
    }

    fn fields(d: &[u8; 16]) -> (u32, u32, u32, u32) {
        (get(d, 0), get(d, 4), get(d, 8), get(d, 12))
    }

    #[test]
    fn console_only_queue_flags_are_dropped_and_the_pc_flag_kept() {
        for (flags, pc) in [(0u32, 0u32), (1, 1), (2, 0), (3, 1), (0x8000_0002, 0)] {
            let mut d = desc(2, 0, flags, 0);
            translate(&mut d);
            assert_eq!(fields(&d), (2, 0, pc, 0), "flags {flags:#x}");
        }
    }

    #[test]
    fn dma_type_priority_and_node_still_map_to_pc_values() {
        let mut d = desc(XBOX_TYPE_DMA, 1, 3, 4);
        translate(&mut d);
        assert_eq!(fields(&d), (PC_TYPE_COPY, PC_PRIORITY_HIGH, 1, 1));
        let mut same = desc(0, PC_PRIORITY_GLOBAL_REALTIME, 1, 0);
        translate(&mut same);
        assert_eq!(fields(&same), (0, PC_PRIORITY_GLOBAL_REALTIME, 1, 0));
    }
}

pub fn list_type(t: u32) -> u32 {
    if t == XBOX_TYPE_DMA { PC_TYPE_COPY } else { t }
}

pub unsafe fn create_allocator(real: *mut core::ffi::c_void, t: u32, riid: *const core::ffi::c_void, ppv: *mut *mut core::ffi::c_void) -> i32 {
    let pc = list_type(t);
    let f: unsafe extern "system" fn(*mut core::ffi::c_void, u32, *const core::ffi::c_void, *mut *mut core::ffi::c_void) -> i32 =
        unsafe { core::mem::transmute(*(*(real as *const *const usize)).add(9)) };
    let hr = unsafe { f(real, pc, riid, ppv) };
    if pc != t || hr != 0 {
        record("d3d12_xs", "xone.CreateCommandAllocator", &format!("Xbox type={t:#x} -> PC type={pc} hr={hr:#010x}"));
    }
    hr
}
