use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use compat_common::{record, E_FAIL, E_NOTIMPL, S_OK};
use crate::layout::{Layout2D, E_INVALIDARG, LAYOUT_BYTES};

const M: &str = "xg_xs";

#[repr(C)]
pub struct Computer {
    vtbl: *const Vtbl,
    refs: AtomicU32,
    pub layout: Layout2D,
}

pub(crate) type GetResourceTiling<T> = unsafe extern "system" fn(
    *mut T, *mut u32, *mut [u32; 3], *mut [u32; 3], *mut u32, u32, *mut SubresourceTiling,
) -> i32;

#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct SubresourceTiling {
    width: u32,
    height: u16,
    depth: u16,
    start: u32,
}

#[repr(C)]
struct Vtbl {
    add_ref: unsafe extern "system" fn(*mut Computer) -> u32,
    release: unsafe extern "system" fn(*mut Computer) -> u32,
    get_resource_layout: unsafe extern "system" fn(*mut Computer, *mut u8) -> i32,
    get_size: unsafe extern "system" fn(*mut Computer) -> u64,
    get_align: unsafe extern "system" fn(*mut Computer) -> u64,
    get_mip_offset: unsafe extern "system" fn(*mut Computer, u32, u32) -> u64,
    get_element_offset: unsafe extern "system" fn(*mut Computer, u32, u32, u64, u32, u32, u32, *mut u32) -> u64,
    get_texel_coordinate: unsafe extern "system" fn(*mut Computer, usize, usize, usize, usize, usize, usize) -> i32,
    copy_into: unsafe extern "system" fn(*mut Computer, *mut u8, u32, u32, *const u8, u32, u32) -> i32,
    copy_from: unsafe extern "system" fn(*mut Computer, *mut u8, u32, u32, *const u8, u32, u32) -> i32,
    tiled: GetResourceTiling<Computer>,
    view_descriptor: unsafe extern "system" fn(*mut Computer, u32, *mut u8) -> i32,
}

static VTBL: Vtbl = Vtbl {
    add_ref,
    release,
    get_resource_layout,
    get_size,
    get_align,
    get_mip_offset,
    get_element_offset,
    get_texel_coordinate,
    copy_into,
    copy_from,
    tiled,
    view_descriptor,
};

pub fn create(layout: Layout2D) -> *mut c_void {
    Box::into_raw(Box::new(Computer { vtbl: &VTBL, refs: AtomicU32::new(1), layout })) as *mut c_void
}

unsafe extern "system" fn add_ref(this: *mut Computer) -> u32 {
    (*this).refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn release(this: *mut Computer) -> u32 {
    let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        drop(Box::from_raw(this));
    }
    n
}

unsafe extern "system" fn get_resource_layout(this: *mut Computer, out: *mut u8) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    let b = core::slice::from_raw_parts_mut(out, LAYOUT_BYTES);
    (*this).layout.write(b);
    S_OK
}

unsafe extern "system" fn get_size(this: *mut Computer) -> u64 {
    (*this).layout.resource_total()
}

unsafe extern "system" fn get_align(this: *mut Computer) -> u64 {
    (*this).layout.resource_align()
}

unsafe extern "system" fn get_mip_offset(this: *mut Computer, plane: u32, mip: u32) -> u64 {
    if plane != 0 {
        return 0;
    }
    (*this).layout.mip_offset(mip)
}

unsafe extern "system" fn get_element_offset(this: *mut Computer, plane: u32, mip: u32, x: u64, y: u32, z: u32, sample: u32, bit: *mut u32) -> u64 {
    if !bit.is_null() {
        *bit = 0;
    }
    if plane != 0 {
        return u64::MAX;
    }
    (*this).layout.sample_offset(mip, x, y as u64, z as u64, sample as u64).unwrap_or(u64::MAX)
}

unsafe extern "system" fn get_texel_coordinate(_this: *mut Computer, _a: usize, _b: usize, _c: usize, _d: usize, _e: usize, _f: usize) -> i32 {
    record(M, "XGTextureAddressComputer::GetTexelCoordinate", "[native E_NOTIMPL]");
    E_NOTIMPL
}

unsafe fn sub_rect(l: &Layout2D, plane: u32, sub: u32) -> Option<(u32, u32, u32, u32)> {
    if plane != 0 || l.mips == 0 {
        return None;
    }
    let (mip, slice) = (sub % l.mips, sub / l.mips);
    if slice >= l.desc.array_size {
        return None;
    }
    let m = &l.mip[mip as usize];
    Some((mip, slice, m.w_e, m.h_e))
}

unsafe extern "system" fn copy_into(this: *mut Computer, res: *mut u8, plane: u32, sub: u32, data: *const u8, row_pitch: u32, _slice_pitch: u32) -> i32 {
    let l = &(*this).layout;
    let (mip, slice, w, h) = match sub_rect(l, plane, sub) {
        Some(v) => v,
        None => return E_INVALIDARG,
    };
    if res.is_null() || data.is_null() {
        return E_INVALIDARG;
    }
    let bpe = l.bpe as usize;
    for y in 0..h {
        let row = data.add(y as usize * row_pitch as usize);
        for x in 0..w {
            if let Some(o) = l.element_offset(mip, x as u64, y as u64, slice as u64) {
                core::ptr::copy_nonoverlapping(row.add(x as usize * bpe), res.add(o as usize), bpe);
            }
        }
    }
    S_OK
}

unsafe extern "system" fn copy_from(this: *mut Computer, data: *mut u8, plane: u32, sub: u32, res: *const u8, row_pitch: u32, _slice_pitch: u32) -> i32 {
    let l = &(*this).layout;
    let (mip, slice, w, h) = match sub_rect(l, plane, sub) {
        Some(v) => v,
        None => return E_INVALIDARG,
    };
    if res.is_null() || data.is_null() {
        return E_INVALIDARG;
    }
    let bpe = l.bpe as usize;
    for y in 0..h {
        let row = data.add(y as usize * row_pitch as usize);
        for x in 0..w {
            if let Some(o) = l.element_offset(mip, x as u64, y as u64, slice as u64) {
                core::ptr::copy_nonoverlapping(res.add(o as usize), row.add(x as usize * bpe), bpe);
            }
        }
    }
    S_OK
}

unsafe extern "system" fn tiled(
    this: *mut Computer,
    total: *mut u32,
    packed: *mut [u32; 3],
    shape: *mut [u32; 3],
    count: *mut u32,
    first: u32,
    tilings: *mut SubresourceTiling,
) -> i32 {
    const TILE: u64 = 64 * 1024;
    let layout = &(*this).layout;
    let first_tail = layout.first_tail;
    let mode = layout.desc.swizzle_mode & 31;
    if layout.block_bits != 16 || !matches!(mode, 8..=11 | 16..=19 | 24..=27) {
        return E_FAIL;
    }
    let Some(num_subresources) = layout.mips.checked_mul(layout.desc.array_size) else {
        return E_FAIL;
    };
    let Ok(num_tiles) = u32::try_from(layout.total / TILE) else {
        return E_FAIL;
    };
    let entries = if count.is_null() { 0 } else {
        count.read_unaligned().min(num_subresources.saturating_sub(first))
    };
    if entries != 0 && tilings.is_null() {
        return E_INVALIDARG;
    }
    if !total.is_null() {
        total.write_unaligned(num_tiles);
    }
    let tail_mips = layout.mips - first_tail;
    if !packed.is_null() {
        let tail_start = if tail_mips == 0 { 0 } else { (layout.mip_offset(first_tail) / TILE) as u32 };
        packed.write_unaligned([first_tail | tail_mips << 8, u32::from(tail_mips != 0), tail_start]);
    }
    if !shape.is_null() {
        let [width, height, depth] = if first_tail == 0 { [0; 3] } else {
            [layout.bw * layout.block, layout.bh * layout.block, 1]
        };
        shape.write_unaligned([width, height, depth]);
    }
    for n in 0..entries {
        let sub = first + n;
        let mip = sub % layout.mips;
        let slice = sub / layout.mips;
        let record = if mip < first_tail {
            let level = &layout.mip[mip as usize];
            SubresourceTiling {
                width: level.pad_w / layout.bw,
                height: (level.pad_h / layout.bh) as u16,
                depth: 1,
                start: ((u64::from(slice) * layout.slice_size + level.offset) / TILE) as u32,
            }
        } else {
            SubresourceTiling { width: 0, height: 0, depth: 0, start: u32::MAX }
        };
        tilings.add(n as usize).write_unaligned(record);
    }
    if !count.is_null() {
        count.write_unaligned(entries);
    }
    static LOGGED: AtomicU32 = AtomicU32::new(0);
    if LOGGED.fetch_add(1, Ordering::Relaxed) < 8 {
        record(M, "XGTextureAddressComputer::GetResourceTiling",
            &format!("mode={mode} total={num_tiles} first={first} returned={entries} packed={tail_mips}"));
    }
    S_OK
}

unsafe extern "system" fn view_descriptor(_this: *mut Computer, _plane: u32, _out: *mut u8) -> i32 {
    record(M, "XGTextureAddressComputer::GetTextureViewDescriptor", "[native E_NOTIMPL]");
    E_NOTIMPL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{compute, Tex2DDesc};

    #[test]
    fn reports_tiling_through_slot_ten_and_bounds_the_output() {
        let desc = Tex2DDesc {
            width: 512, height: 256, mip_levels: 1, array_size: 2,
            format: 28, sample_count: 1, swizzle_mode: 25, ..Default::default()
        };
        let p = create(compute(&desc).unwrap()) as *mut Computer;
        let mut total = 0;
        let mut packed = [0; 3];
        let mut shape = [0; 3];
        let mut count = 2;
        let sentinel = SubresourceTiling { width: 99, height: 0, depth: 0, start: 0 };
        let mut records = [sentinel; 2];
        let result = unsafe {
            let f = (*(*p).vtbl).tiled;
            let hr = f(p, &mut total, &mut packed, &mut shape, &mut count, 1, records.as_mut_ptr());
            release(p);
            hr
        };
        assert_eq!(result, S_OK);
        assert_eq!((total, packed, shape, count), (16, [1, 0, 0], [128, 128, 1], 1));
        let t = records[0];
        assert_eq!((t.width, t.height, t.depth, t.start), (4, 2, 1, 8));
        assert_eq!(records[1].width, 99);
    }

    #[test]
    fn copy_roundtrip() {
        let d = Tex2DDesc { width: 64, height: 48, mip_levels: 0, array_size: 2, format: 28, sample_count: 1, swizzle_mode: 25, ..Default::default() };
        let l = compute(&d).unwrap();
        let p = create(l) as *mut Computer;
        unsafe {
            let total = get_size(p) as usize;
            let mut res = vec![0u8; total];
            let src: Vec<u8> = (0..64 * 48 * 4).map(|i| (i * 7 + 3) as u8).collect();
            let sub = (*p).layout.mips;
            assert_eq!(copy_into(p, res.as_mut_ptr(), 0, sub, src.as_ptr(), 256, 0), S_OK);
            let mut back = vec![0u8; src.len()];
            assert_eq!(copy_from(p, back.as_mut_ptr(), 0, sub, res.as_ptr(), 256, 0), S_OK);
            assert_eq!(src, back);
            assert_eq!(add_ref(p), 2);
            assert_eq!(release(p), 1);
            assert_eq!(release(p), 0);
        }
    }
}
