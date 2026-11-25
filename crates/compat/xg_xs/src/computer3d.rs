use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use compat_common::{record, E_NOTIMPL, S_OK};
use crate::layout::{E_INVALIDARG, LAYOUT_BYTES};
use crate::layout3d::Surface;

const M: &str = "xg_xs";
const E_FAIL: i32 = 0x8000_4005u32 as i32;

#[repr(C)]
pub struct Computer3D {
    vtbl: *const Vtbl,
    refs: AtomicU32,
    pub surface: Surface,
}

#[repr(C)]
struct Vtbl {
    add_ref: unsafe extern "system" fn(*mut Computer3D) -> u32,
    release: unsafe extern "system" fn(*mut Computer3D) -> u32,
    get_resource_layout: unsafe extern "system" fn(*mut Computer3D, *mut u8) -> i32,
    get_size: unsafe extern "system" fn(*mut Computer3D) -> u64,
    get_align: unsafe extern "system" fn(*mut Computer3D) -> u64,
    get_mip_offset: unsafe extern "system" fn(*mut Computer3D, u32, u32) -> u64,
    get_element_offset: unsafe extern "system" fn(*mut Computer3D, u32, u32, u64, u32, u32, u32, *mut u32) -> u64,
    get_texel_coordinate: unsafe extern "system" fn(*mut Computer3D, usize, usize, usize, usize, usize, usize) -> i32,
    copy_into: unsafe extern "system" fn(*mut Computer3D, *mut u8, u32, u32, *const u8, u32, u32) -> i32,
    copy_from: unsafe extern "system" fn(*mut Computer3D, *mut u8, u32, u32, *const u8, u32, u32) -> i32,
    tiled: crate::computer::GetResourceTiling<Computer3D>,
    view_descriptor: unsafe extern "system" fn(*mut Computer3D, u32, *mut u8) -> i32,
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

pub fn create(surface: Surface) -> *mut c_void {
    Box::into_raw(Box::new(Computer3D { vtbl: &VTBL, refs: AtomicU32::new(1), surface })) as *mut c_void
}

unsafe extern "system" fn add_ref(this: *mut Computer3D) -> u32 {
    (*this).refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn release(this: *mut Computer3D) -> u32 {
    let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if n == 0 {
        drop(Box::from_raw(this));
    }
    n
}

unsafe extern "system" fn get_resource_layout(this: *mut Computer3D, out: *mut u8) -> i32 {
    if out.is_null() {
        return E_INVALIDARG;
    }
    (*this).surface.write(core::slice::from_raw_parts_mut(out, LAYOUT_BYTES));
    S_OK
}

unsafe extern "system" fn get_size(this: *mut Computer3D) -> u64 {
    (*this).surface.total()
}

unsafe extern "system" fn get_align(this: *mut Computer3D) -> u64 {
    (*this).surface.align()
}

unsafe extern "system" fn get_mip_offset(this: *mut Computer3D, plane: u32, mip: u32) -> u64 {
    if plane != 0 {
        return 0;
    }
    (*this).surface.mip_offset(mip)
}

unsafe extern "system" fn get_element_offset(this: *mut Computer3D, plane: u32, mip: u32, x: u64, y: u32, z: u32, _sample: u32, bit: *mut u32) -> u64 {
    if !bit.is_null() {
        *bit = 0;
    }
    if plane != 0 {
        return u64::MAX;
    }
    (*this).surface.element_offset(mip, x, y as u64, z as u64).unwrap_or(u64::MAX)
}

unsafe extern "system" fn get_texel_coordinate(_this: *mut Computer3D, _a: usize, _b: usize, _c: usize, _d: usize, _e: usize, _f: usize) -> i32 {
    record(M, "XGTextureAddressComputer::GetTexelCoordinate", "[native E_NOTIMPL] 1D/3D");
    E_NOTIMPL
}

unsafe fn copy(s: &Surface, res: *mut u8, plane: u32, sub: u32, data: *mut u8, row_pitch: u32, slice_pitch: u32, into: bool) -> i32 {
    if plane != 0 || res.is_null() || data.is_null() {
        return E_INVALIDARG;
    }
    let r = match s.sub_rect(sub) {
        Some(r) => r,
        None => return E_INVALIDARG,
    };
    if s.element_offset(r.mip, 0, 0, r.slice as u64).is_none() {
        return E_FAIL;
    }
    let bpe = s.bpe() as usize;
    for z in 0..r.d {
        for y in 0..r.h {
            let row = data.add(z as usize * slice_pitch as usize + y as usize * row_pitch as usize);
            for x in 0..r.w {
                if let Some(o) = s.element_offset(r.mip, x as u64, y as u64, (r.slice + z) as u64) {
                    if into {
                        core::ptr::copy_nonoverlapping(row.add(x as usize * bpe), res.add(o as usize), bpe);
                    } else {
                        core::ptr::copy_nonoverlapping(res.add(o as usize), row.add(x as usize * bpe), bpe);
                    }
                }
            }
        }
    }
    S_OK
}

unsafe extern "system" fn copy_into(this: *mut Computer3D, res: *mut u8, plane: u32, sub: u32, data: *const u8, row_pitch: u32, slice_pitch: u32) -> i32 {
    copy(&(*this).surface, res, plane, sub, data as *mut u8, row_pitch, slice_pitch, true)
}

unsafe extern "system" fn copy_from(_this: *mut Computer3D, _data: *mut u8, _plane: u32, _sub: u32, _res: *const u8, _row_pitch: u32, _slice_pitch: u32) -> i32 {
    record(M, "XGTextureAddressComputer::slot9", "E_INVALIDARG: the console slot needs a descriptor-backed plane and a registered runtime hook, which 1D/3D computers never have");
    E_INVALIDARG
}

unsafe extern "system" fn tiled(
    _this: *mut Computer3D,
    _total: *mut u32,
    _packed: *mut [u32; 3],
    _shape: *mut [u32; 3],
    _count: *mut u32,
    _first: u32,
    _tilings: *mut crate::computer::SubresourceTiling,
) -> i32 {
    record(M, "XGTextureAddressComputer::slot10", "[native E_NOTIMPL] tiled resources (1D/3D)");
    E_NOTIMPL
}

unsafe extern "system" fn view_descriptor(_this: *mut Computer3D, _plane: u32, _out: *mut u8) -> i32 {
    record(M, "XGTextureAddressComputer::GetTextureViewDescriptor", "[native E_NOTIMPL] 1D/3D");
    E_NOTIMPL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout3d::{compute_3d, Tex3DDesc};

    #[test]
    fn copy_roundtrip_3d() {
        crate::layout::set_legacy_19041(false);
        let d = Tex3DDesc { width: 20, height: 12, depth: 9, format: 28, bind_flags: 8, swizzle_mode: 9, ..Default::default() };
        let p = create(Surface::Three(compute_3d(&d).unwrap())) as *mut Computer3D;
        unsafe {
            let total = get_size(p) as usize;
            let mut res = vec![0u8; total];
            let src: Vec<u8> = (0..20 * 12 * 9 * 4).map(|i| (i * 7 + 3) as u8).collect();
            assert_eq!(copy_into(p, res.as_mut_ptr(), 0, 40, src.as_ptr(), 80, 960), S_OK);
            assert_eq!(copy_into(p, res.as_mut_ptr(), 0, 45, src.as_ptr(), 80, 960), E_INVALIDARG);
            let mut back = vec![0u8; src.len()];
            assert_eq!(copy_from(p, back.as_mut_ptr(), 0, 0, res.as_ptr(), 80, 960), E_INVALIDARG);
            for z in 0..9u64 {
                for y in 0..12u64 {
                    for x in 0..20u64 {
                        let o = (*p).surface.element_offset(0, x, y, z).unwrap() as usize;
                        let i = (z * 960 + y * 80 + x * 4) as usize;
                        back[i..i + 4].copy_from_slice(&res[o..o + 4]);
                    }
                }
            }
            assert_eq!(src, back);
            assert_eq!(release(p), 0);
        }
    }
}
