use core::ffi::c_void;

pub const S_OK: i32 = 0;
pub const E_FAIL: i32 = 0x8000_4005u32 as i32;
pub const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;

pub const SLOT_SET_NAME: usize = 6;
pub const SLOT_GET_NODE_COUNT: usize = 7;
pub const SLOT_GET_CUSTOM_HEAP_PROPERTIES: usize = 26;
pub const SLOTS_NOTIMPL: [usize; 5] = [31, 32, 33, 34, 35];
pub const SLOT_GET_DEVICE_REMOVED_REASON: usize = 37;
pub const SLOT_SET_STABLE_POWER_STATE: usize = 40;
pub const SLOT_GET_ADAPTER_LUID: usize = 43;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapProperties {
    pub heap_type: u32,
    pub cpu_page_property: u32,
    pub memory_pool_preference: u32,
    pub creation_node_mask: u32,
    pub visible_node_mask: u32,
}

pub fn custom_heap_properties(heap_type: u32) -> HeapProperties {
    let cpu_page_property = match heap_type {
        1 | 2 => 2,
        3 => 3,
        _ => 0,
    };
    HeapProperties { heap_type, cpu_page_property, memory_pool_preference: 1, creation_node_mask: 1, visible_node_mask: 1 }
}

unsafe extern "system" fn set_name_x(_this: *mut c_void, _name: *const u16) -> i32 {
    S_OK
}

unsafe extern "system" fn get_node_count_x(_this: *mut c_void) -> u32 {
    1
}

pub fn pc_heap_properties(p: HeapProperties) -> HeapProperties {
    match p.heap_type {
        1..=3 => HeapProperties { cpu_page_property: 0, memory_pool_preference: 0, ..p },
        _ => p,
    }
}

pub fn custom_heap_slot() -> (usize, usize) {
    (SLOT_GET_CUSTOM_HEAP_PROPERTIES, get_custom_heap_properties_x as usize)
}

unsafe extern "system" fn get_custom_heap_properties_x(out: *mut HeapProperties, _this: *mut c_void, _node_mask: u32, heap_type: u32) -> *mut HeapProperties {
    if !out.is_null() {
        unsafe { core::ptr::write_unaligned(out, custom_heap_properties(heap_type)) };
    }
    out
}

unsafe extern "system" fn notimpl_x(_this: *mut c_void) -> i32 {
    E_NOTIMPL
}

unsafe extern "system" fn get_device_removed_reason_x(_this: *mut c_void) -> i32 {
    S_OK
}

unsafe extern "system" fn set_stable_power_state_x(_this: *mut c_void, _enable: i32) -> i32 {
    E_FAIL
}

unsafe extern "system" fn get_adapter_luid_x(_this: *mut c_void) -> u64 {
    0
}

pub fn plan() -> Vec<(usize, usize)> {
    let mut p = vec![
        (SLOT_SET_NAME, set_name_x as usize),
        (SLOT_GET_NODE_COUNT, get_node_count_x as usize),
        (SLOT_GET_DEVICE_REMOVED_REASON, get_device_removed_reason_x as usize),
        (SLOT_SET_STABLE_POWER_STATE, set_stable_power_state_x as usize),
        (SLOT_GET_ADAPTER_LUID, get_adapter_luid_x as usize),
    ];
    p.extend(SLOTS_NOTIMPL.iter().map(|&s| (s, notimpl_x as usize)));
    p.push(custom_heap_slot());
    p
}

pub fn pc_heap_words(w: [u32; 5]) -> [u32; 5] {
    let p = pc_heap_properties(HeapProperties { heap_type: w[0], cpu_page_property: w[1], memory_pool_preference: w[2], creation_node_mask: w[3], visible_node_mask: w[4] });
    [p.heap_type, p.cpu_page_property, p.memory_pool_preference, p.creation_node_mask, p.visible_node_mask]
}

pub fn pc_heap_words_at(d: &mut [u8], off: usize) {
    let mut w = [0u32; 5];
    for (k, v) in w.iter_mut().enumerate() {
        *v = xs_u32(d, off + 4 * k);
    }
    for (k, v) in pc_heap_words(w).iter().enumerate() {
        d[off + 4 * k..off + 4 * k + 4].copy_from_slice(&v.to_le_bytes());
    }
}

pub const RES_DESC_SIZE: usize = 0x38;
const XG_LAYOUT_BYTES: usize = 0x2000;
const ALLOC_FAILED: (u64, u64) = (0, 0);

extern "system" {
    fn GetModuleHandleA(n: *const u8) -> *mut c_void;
    fn LoadLibraryA(n: *const u8) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, n: *const u8) -> *mut c_void;
}

pub fn alloc_step(acc: (u64, u64), part: (u64, u64)) -> (u64, u64) {
    let (size, a) = part;
    let align = acc.1.max(a);
    let off = acc.0.wrapping_sub(1).wrapping_add(a) & !a.wrapping_sub(1);
    (off.wrapping_add(size), align)
}

pub fn buffer_alloc(desc: &[u8]) -> (u64, u64) {
    let width = u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap());
    let align = u64::from_le_bytes(desc[0x08..0x10].try_into().unwrap());
    (width, align.max(4))
}

pub fn alloc_info_with(descs: &[u8], count: usize, mut texture: impl FnMut(&[u8]) -> Option<(u64, u64)>) -> (u64, u64) {
    let mut acc = (0u64, 1u64);
    for i in 0..count {
        let d = &descs[i * RES_DESC_SIZE..(i + 1) * RES_DESC_SIZE];
        let part = if xs_u32(d, 0) == 1 {
            buffer_alloc(d)
        } else {
            match texture(d) {
                Some(p) => p,
                None => return ALLOC_FAILED,
            }
        };
        acc = alloc_step(acc, part);
    }
    acc
}

fn xg_create(fam: crate::console_slots_dev2::Family) -> usize {
    static ONE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    static SC: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let (cell, name): (&std::sync::OnceLock<usize>, &[u8]) = match fam {
        crate::console_slots_dev2::Family::XboxOne => (&ONE, b"xg_x.dll\0"),
        _ => (&SC, b"xg_xs.dll\0"),
    };
    *cell.get_or_init(|| unsafe {
        let mut h = GetModuleHandleA(name.as_ptr());
        if h.is_null() {
            h = LoadLibraryA(name.as_ptr());
        }
        if h.is_null() { 0 } else { GetProcAddress(h, b"XGCreateTextureComputer\0".as_ptr()) as usize }
    })
}

pub fn xg_available(fam: crate::console_slots_dev2::Family) -> bool {
    xg_create(fam) != 0
}

pub const XG_E_NOTIMPL: i32 = 0x8000_4001u32 as i32;

pub fn title_family_from(xbox_one_title: bool) -> crate::console_slots_dev2::Family {
    if xbox_one_title { crate::console_slots_dev2::Family::XboxOne } else { crate::console_slots_dev2::Family::Scarlett }
}

pub fn title_family() -> crate::console_slots_dev2::Family {
    title_family_from(crate::xone_tex::is_xbox_one())
}

pub fn texture_part(xg: Result<(u64, u64), i32>, estimate: impl FnOnce() -> (u64, u64)) -> Option<(u64, u64)> {
    match xg {
        Ok(p) => Some(p),
        Err(XG_E_NOTIMPL) => Some(estimate()),
        Err(_) => None,
    }
}

pub fn xg_texture_alloc(desc: &[u8], fam: crate::console_slots_dev2::Family) -> Result<(u64, u64), i32> {
    let f = xg_create(fam);
    if f == 0 {
        return Err(XG_E_NOTIMPL);
    }
    unsafe {
        let create: unsafe extern "system" fn(*const u8, *mut *mut c_void) -> i32 = core::mem::transmute(f);
        let mut comp: *mut c_void = core::ptr::null_mut();
        let hr = create(desc.as_ptr(), &mut comp);
        if hr < 0 {
            return Err(hr);
        }
        if comp.is_null() {
            return Err(E_INVALIDARG);
        }
        let vt = *(comp as *const *const usize);
        let layout: unsafe extern "system" fn(*mut c_void, *mut u8) -> i32 = core::mem::transmute(*vt.add(2));
        let mut out = vec![0u8; XG_LAYOUT_BYTES];
        layout(comp, out.as_mut_ptr());
        let release: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*vt.add(1));
        release(comp);
        Ok((u64::from_le_bytes(out[0..8].try_into().unwrap()), u64::from_le_bytes(out[8..16].try_into().unwrap())))
    }
}

pub const HEAP_DESC_SIZE: usize = 0x30;
pub const HEAP_FLAG_ALLOW_DISPLAY: u32 = 0x8;
pub const HEAP_FLAG_XBOX_CPU_ATTR: u32 = 0x2;
const E_INVALIDARG: i32 = 0x8007_0057u32 as i32;
const PC_HEAP_MIN: u64 = 0x10000;
const PC_HEAP_ALIGN_MSAA: u64 = 0x40_0000;
pub const HEAP_DENY_CATEGORIES: u32 = 0x4 | 0x40 | 0x80;

pub fn pc_heap_deny_flags(flags: u32) -> u32 {
    let deny = flags & HEAP_DENY_CATEGORIES;
    if deny.count_ones() == 1 {
        flags & !HEAP_DENY_CATEGORIES
    } else {
        flags
    }
}

pub fn heap_desc_for_pc(x: &[u8]) -> Result<[u8; HEAP_DESC_SIZE], i32> {
    let mut d: [u8; HEAP_DESC_SIZE] = x[..HEAP_DESC_SIZE].try_into().unwrap();
    let flags = xs_u32(&d, 0x28);
    if flags & HEAP_FLAG_ALLOW_DISPLAY != 0 {
        return Err(E_INVALIDARG);
    }
    d[0x28..0x2c].copy_from_slice(&pc_heap_deny_flags(flags & !HEAP_FLAG_XBOX_CPU_ATTR).to_le_bytes());
    pc_heap_words_at(&mut d, 8);
    let size = u64::from_le_bytes(d[0..8].try_into().unwrap());
    let size = ((size + 0xff) & !0xff).max(PC_HEAP_MIN);
    d[0..8].copy_from_slice(&size.to_le_bytes());
    let align = u64::from_le_bytes(d[0x20..0x28].try_into().unwrap());
    let pc_align = if align <= PC_HEAP_MIN { 0 } else if align <= PC_HEAP_ALIGN_MSAA { PC_HEAP_ALIGN_MSAA } else { align };
    d[0x20..0x28].copy_from_slice(&pc_align.to_le_bytes());
    Ok(d)
}

pub const XSAMP_SIZE: usize = 0x6c;
pub const PC_SAMPLER_DESC_SIZE: usize = 0x34;
const FILTER_ANISOTROPIC: u32 = 0x55;

pub const DEFAULT_XSAMP_WORDS: [u32; 17] = [0, 1, 1, 2, 2, 0, 3, 3, 3, 0xff7f_ffff, 0x7f7f_ffff, 0, 0, 16, 0, 0, 1];

pub fn default_xsamp() -> [u8; XSAMP_SIZE] {
    let mut x = [0u8; XSAMP_SIZE];
    for (k, w) in DEFAULT_XSAMP_WORDS.iter().enumerate() {
        x[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
    }
    x
}

fn xs_u32(x: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(x[o..o + 4].try_into().unwrap())
}

pub fn sampler_filter_from_xbox(mag: u32, min: u32, mip: u32, mode: u32) -> u32 {
    let reduction = match mode & 3 {
        1 => 2,
        2 => 3,
        _ => 0,
    };
    let base = if mag >= 2 || min >= 2 {
        FILTER_ANISOTROPIC
    } else {
        ((mag == 1) as u32) << 2 | ((min == 1) as u32) << 4 | (mip == 2) as u32
    };
    base | reduction << 7
}

pub fn sampler_desc_from_xbox(x: &[u8]) -> [u8; PC_SAMPLER_DESC_SIZE] {
    let mut d = [0u8; PC_SAMPLER_DESC_SIZE];
    let mut put = |o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(0x00, sampler_filter_from_xbox(xs_u32(x, 0x04), xs_u32(x, 0x08), xs_u32(x, 0x0c), xs_u32(x, 0x14)));
    put(0x04, xs_u32(x, 0x18));
    put(0x08, xs_u32(x, 0x1c));
    put(0x0c, xs_u32(x, 0x20));
    put(0x10, xs_u32(x, 0x2c));
    put(0x14, xs_u32(x, 0x34).clamp(1, 16));
    put(0x18, xs_u32(x, 0x40));
    for k in 0..4 {
        put(0x1c + 4 * k, xs_u32(x, 0x44 + 4 * k));
    }
    put(0x2c, xs_u32(x, 0x24));
    put(0x30, xs_u32(x, 0x28));
    d
}

pub fn sampler_unmapped_fields(x: &[u8]) -> Vec<(usize, u32)> {
    let mut v = Vec::new();
    let anisotropic = xs_u32(x, 0x04) >= 2 || xs_u32(x, 0x08) >= 2;
    if anisotropic && xs_u32(x, 0x0c) != 2 {
        v.push((0x0c, xs_u32(x, 0x0c)));
    }
    let min_linear = anisotropic || xs_u32(x, 0x08) == 1;
    let z = xs_u32(x, 0x10);
    if z != 0 && (z == 2) != min_linear {
        v.push((0x10, z));
    }
    for o in [0x00usize, 0x30, 0x38, 0x3c, 0x54, 0x58] {
        let val = xs_u32(x, o);
        if val != 0 {
            v.push((o, val));
        }
    }
    if xs_u32(x, 0) & 0x100 != 0 {
        for o in [0x5cusize, 0x60, 0x64, 0x68] {
            v.push((o, xs_u32(x, o)));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xsamp(fields: &[(usize, u32)]) -> Vec<u8> {
        let mut x = vec![0u8; XSAMP_SIZE];
        for &(o, v) in fields {
            x[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        x
    }

    #[test]
    fn sampler_filter_follows_sq_img_samp_word2_fields() {
        assert_eq!(sampler_filter_from_xbox(3, 3, 2, 0), 0x55);
        assert_eq!(sampler_filter_from_xbox(0, 0, 0, 0), 0x00);
        assert_eq!(sampler_filter_from_xbox(0, 0, 1, 0), 0x00);
        assert_eq!(sampler_filter_from_xbox(1, 1, 2, 0), 0x15);
        assert_eq!(sampler_filter_from_xbox(1, 0, 1, 0), 0x04);
        assert_eq!(sampler_filter_from_xbox(0, 1, 2, 0), 0x11);
        assert_eq!(sampler_filter_from_xbox(1, 1, 2, 1), 0x115);
        assert_eq!(sampler_filter_from_xbox(1, 1, 2, 2), 0x195);
        assert_eq!(sampler_filter_from_xbox(3, 3, 2, 2), 0x1d5);
    }

    #[test]
    fn sampler_desc_maps_every_pc_field_from_the_xbox_desc() {
        let x = xsamp(&[(0x04, 1), (0x08, 1), (0x0c, 1), (0x18, 3), (0x1c, 1), (0x20, 4), (0x24, 0.5f32.to_bits()), (0x28, 9f32.to_bits()), (0x2c, (-1f32).to_bits()), (0x34, 32), (0x40, 4), (0x44, 1f32.to_bits()), (0x50, 1f32.to_bits())]);
        let d = sampler_desc_from_xbox(&x);
        let g = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        assert_eq!(g(0x00), 0x14);
        assert_eq!((g(0x04), g(0x08), g(0x0c)), (3, 1, 4));
        assert_eq!(f32::from_bits(g(0x10)), -1.0);
        assert_eq!(g(0x14), 16);
        assert_eq!(g(0x18), 4);
        assert_eq!((f32::from_bits(g(0x1c)), g(0x20), g(0x24), f32::from_bits(g(0x28))), (1.0, 0, 0, 1.0));
        assert_eq!((f32::from_bits(g(0x2c)), f32::from_bits(g(0x30))), (0.5, 9.0));
        assert!(sampler_unmapped_fields(&x).is_empty());
        let a = xsamp(&[(0x04, 3), (0x08, 3), (0x0c, 1), (0x38, 2f32.to_bits()), (0x00, 0x100), (0x5c, 5)]);
        let u = sampler_unmapped_fields(&a);
        assert!(u.contains(&(0x0c, 1)) && u.contains(&(0x38, 2f32.to_bits())) && u.contains(&(0x5c, 5)) && u.contains(&(0x00, 0x100)));
    }

    use crate::device_wrap::Guid;
    const IID_DEV: Guid = Guid { d1: 0x1898_19f1, d2: 0x1db6, d3: 0x4b57, d4: [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7] };
    const IID_RES: Guid = Guid { d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad] };
    const IID_HEAP: Guid = Guid { d1: 0x6b3b_2502, d2: 0x6e51, d3: 0x45b3, d4: [0x90, 0xee, 0x98, 0x84, 0x26, 0x5e, 0x8d, 0xf3] };

    unsafe fn slot(o: *mut c_void, i: usize) -> usize {
        unsafe { *(*(o as *const *const usize)).add(i) }
    }

    unsafe fn rel(o: *mut c_void) {
        if !o.is_null() {
            let f: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(slot(o, 2)) };
            unsafe { f(o) };
        }
    }

    #[test]
    fn console_heap_properties_create_on_pc_through_every_wrapped_path() {
        unsafe {
            let p = crate::host_proc(b"D3D12CreateDevice\0");
            assert!(p != 0);
            let mk: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(mk(core::ptr::null_mut(), 0xb000, &IID_DEV, &mut dev) >= 0 && !dev.is_null());
            let w = crate::device_wrap::wrap_device(dev);
            let get: unsafe extern "system" fn(*mut HeapProperties, *mut c_void, u32, u32) -> *mut HeapProperties = core::mem::transmute(slot(w, 26));
            let committed: unsafe extern "system" fn(*mut c_void, *const HeapProperties, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 27));
            let heap: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 28));
            let placed: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 29));
            let mut buf = vec![0u8; RES_DESC_SIZE];
            buf[0..4].copy_from_slice(&1u32.to_le_bytes());
            buf[0x10..0x18].copy_from_slice(&0x10000u64.to_le_bytes());
            buf[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
            buf[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
            buf[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
            buf[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
            buf[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
            for (ty, state) in [(1u32, 0u32), (2, 0xac3), (3, 0x400)] {
                let mut hp = HeapProperties { heap_type: 0, cpu_page_property: 9, memory_pool_preference: 9, creation_node_mask: 9, visible_node_mask: 9 };
                assert_eq!(get(&mut hp, w, 0, ty), &mut hp as *mut HeapProperties);
                assert_eq!(hp, custom_heap_properties(ty));
                let raw: unsafe extern "system" fn(*mut c_void, *const HeapProperties, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(dev, 27));
                let mut r: *mut c_void = core::ptr::null_mut();
                let raw_hr = raw(dev, &hp, 0, buf.as_ptr(), state, core::ptr::null(), &IID_RES, &mut r);
                rel(r);
                assert!(raw_hr < 0, "PC accepted console heap properties for type {ty}; normalisation no longer needed");
                let mut r: *mut c_void = core::ptr::null_mut();
                let hr = committed(w, &hp, 0, buf.as_ptr(), state, core::ptr::null(), &IID_RES, &mut r);
                assert!(hr >= 0 && !r.is_null(), "committed type {ty} hr={hr:#010x}");
                rel(r);
                for xflags in [0xc0u32, 0xc0 | HEAP_FLAG_XBOX_CPU_ATTR] {
                    let mut hd = vec![0u8; HEAP_DESC_SIZE];
                    hd[0..8].copy_from_slice(&0x10000u64.to_le_bytes());
                    let words = [hp.heap_type, hp.cpu_page_property, hp.memory_pool_preference, hp.creation_node_mask, hp.visible_node_mask];
                    for (k, v) in words.iter().enumerate() {
                        hd[8 + 4 * k..12 + 4 * k].copy_from_slice(&v.to_le_bytes());
                    }
                    hd[0x28..0x2c].copy_from_slice(&xflags.to_le_bytes());
                    let mut h: *mut c_void = core::ptr::null_mut();
                    let hr = heap(w, hd.as_ptr(), &IID_HEAP, &mut h);
                    assert!(hr >= 0 && !h.is_null(), "heap type {ty} flags {xflags:#x} hr={hr:#010x}");
                    let mut pr: *mut c_void = core::ptr::null_mut();
                    let hr = placed(w, h, 0, buf.as_ptr(), state, core::ptr::null(), &IID_RES, &mut pr);
                    assert!(hr >= 0 && !pr.is_null(), "placed type {ty} hr={hr:#010x}");
                    rel(pr);
                    rel(h);
                }
                let mut hd = vec![0u8; HEAP_DESC_SIZE];
                hd[0..8].copy_from_slice(&0x10000u64.to_le_bytes());
                hd[8..12].copy_from_slice(&ty.to_le_bytes());
                hd[0x28..0x2c].copy_from_slice(&(0xc0 | HEAP_FLAG_ALLOW_DISPLAY).to_le_bytes());
                let mut h: *mut c_void = 8 as *mut c_void;
                assert_eq!(heap(w, hd.as_ptr(), &IID_HEAP, &mut h), E_INVALIDARG);
                assert!(h.is_null());
            }
            rel(w);
        }
    }

    #[test]
    fn console_sized_buffers_place_into_a_pc_heap_through_slot_29() {
        unsafe {
            let p = crate::host_proc(b"D3D12CreateDevice\0");
            let mk: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(mk(core::ptr::null_mut(), 0xb000, &IID_DEV, &mut dev) >= 0 && !dev.is_null());
            let w = crate::device_wrap::wrap_device(dev);
            let heap: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 28));
            let placed: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 29));
            let mut b = rdesc(1, 0, 100);
            b[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
            b[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
            b[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
            b[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
            b[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
            let mut two = b.clone();
            two.extend_from_slice(&b);
            let (size, align) = alloc_info_with(&two, 2, |_| None);
            assert_eq!((size, align), (200, 4));
            let mut hd = vec![0u8; HEAP_DESC_SIZE];
            hd[0..8].copy_from_slice(&size.to_le_bytes());
            hd[8..12].copy_from_slice(&1u32.to_le_bytes());
            hd[0x28..0x2c].copy_from_slice(&0xc0u32.to_le_bytes());
            let mut h: *mut c_void = core::ptr::null_mut();
            assert!(heap(w, hd.as_ptr(), &IID_HEAP, &mut h) >= 0 && !h.is_null());
            let mut r0: *mut c_void = core::ptr::null_mut();
            let hr0 = placed(w, h, 0, b.as_ptr(), 0, core::ptr::null(), &IID_RES, &mut r0);
            assert!(hr0 >= 0 && !r0.is_null(), "first console-sized buffer hr={hr0:#010x}");
            let mut r1: *mut c_void = core::ptr::null_mut();
            let hr1 = placed(w, h, 100, b.as_ptr(), 0, core::ptr::null(), &IID_RES, &mut r1);
            assert!(hr1 >= 0 && !r1.is_null(), "second console-packed buffer at offset 100 hr={hr1:#010x}");
            let mut got = [0u8; 0x38];
            let gd: unsafe extern "system" fn(*mut c_void, *mut u8) -> *mut u8 = core::mem::transmute(slot(r1, 10));
            gd(r1, got.as_mut_ptr());
            assert_eq!(u64::from_le_bytes(got[0x10..0x18].try_into().unwrap()), 100, "GetDesc reports the console width");
            let gva: unsafe extern "system" fn(*mut c_void) -> u64 = core::mem::transmute(slot(r0, 11));
            let (v0, v1) = (gva(r0), gva(r1));
            assert!(v0 != 0 && v1 != 0);
            assert_eq!(crate::placed_offset::bytes(r1 as usize), 100);
            assert_eq!(v1, v0 + 100, "the GPU address is the heap address plus the console offset");
            rel(r1);
            rel(r0);
            rel(h);

            let mut hd = vec![0u8; HEAP_DESC_SIZE];
            hd[0..8].copy_from_slice(&400u64.to_le_bytes());
            hd[8..12].copy_from_slice(&2u32.to_le_bytes());
            hd[0x28..0x2c].copy_from_slice(&0xc0u32.to_le_bytes());
            let mut h: *mut c_void = core::ptr::null_mut();
            assert!(heap(w, hd.as_ptr(), &IID_HEAP, &mut h) >= 0 && !h.is_null());
            let mut rs = [core::ptr::null_mut::<c_void>(); 3];
            let mut ps = [core::ptr::null_mut::<u8>(); 3];
            for (i, off) in [0u64, 100, 150].into_iter().enumerate() {
                let hr = placed(w, h, off, b.as_ptr(), 0xac3, core::ptr::null(), &IID_RES, &mut rs[i]);
                assert!(hr >= 0 && !rs[i].is_null(), "upload buffer at {off} hr={hr:#010x}");
                let m: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut u8) -> i32 = core::mem::transmute(slot(rs[i], 8));
                assert!(m(rs[i], 0, core::ptr::null(), &mut ps[i]) >= 0 && !ps[i].is_null());
            }
            assert_eq!((ps[1] as usize - ps[0] as usize, ps[2] as usize - ps[0] as usize), (100, 150), "Map pointers keep the console packing");
            *ps[1].add(60) = 0x5a;
            assert_eq!(*ps[0].add(160), 0x5a, "the second buffer aliases the first buffer's bytes at offset 100");
            assert_eq!(*ps[2].add(10), 0x5a, "an overlapping third buffer sees the same byte");
            for r in rs {
                rel(r);
            }
            rel(h);

            let mut hd = vec![0u8; HEAP_DESC_SIZE];
            hd[0..8].copy_from_slice(&0x2_0000u64.to_le_bytes());
            hd[8..12].copy_from_slice(&1u32.to_le_bytes());
            hd[0x28..0x2c].copy_from_slice(&0x44u32.to_le_bytes());
            let mut h: *mut c_void = core::ptr::null_mut();
            assert!(heap(w, hd.as_ptr(), &IID_HEAP, &mut h) >= 0 && !h.is_null());
            let mut t = rdesc(3, 0, 64);
            t[0x18..0x1c].copy_from_slice(&64u32.to_le_bytes());
            t[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
            t[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
            t[0x20..0x24].copy_from_slice(&28u32.to_le_bytes());
            t[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
            let (mut t0, mut t1) = (core::ptr::null_mut(), core::ptr::null_mut());
            let hr = placed(w, h, 0, t.as_ptr(), 0, core::ptr::null(), &IID_RES, &mut t0);
            assert!(hr >= 0 && !t0.is_null(), "texture at 0 hr={hr:#010x}");
            let hr = placed(w, h, 0x4000, t.as_ptr(), 0, core::ptr::null(), &IID_RES, &mut t1);
            assert!(hr >= 0 && !t1.is_null(), "a 16 KB texture packed at the console's 4 KB offset 0x4000 hr={hr:#010x}");
            let mut t2: *mut c_void = core::ptr::null_mut();
            let hr = placed(w, h, 0x4010, t.as_ptr(), 0, core::ptr::null(), &IID_RES, &mut t2);
            assert!(hr < 0 && t2.is_null(), "a texture off any 4 KB boundary is refused, never faked");
            rel(t1);
            rel(t0);
            rel(h);
            rel(w);
        }
    }

    fn rdesc(dim: u32, align: u64, width: u64) -> Vec<u8> {
        let mut d = vec![0u8; RES_DESC_SIZE];
        d[0..4].copy_from_slice(&dim.to_le_bytes());
        d[8..16].copy_from_slice(&align.to_le_bytes());
        d[16..24].copy_from_slice(&width.to_le_bytes());
        d
    }

    #[test]
    fn alloc_info_follows_release_body_0x4ced0() {
        assert_eq!(alloc_info_with(&[], 0, |_| None), (0, 1));
        assert_eq!(alloc_info_with(&rdesc(1, 0, 100), 1, |_| None), (100, 4));
        assert_eq!(alloc_info_with(&rdesc(1, 0x10000, 100), 1, |_| None), (100, 0x10000));
        let mut two = rdesc(1, 0, 6);
        two.extend(rdesc(3, 0, 64));
        let mut seen = Vec::new();
        assert_eq!(alloc_info_with(&two, 2, |d| { seen.push(u32::from_le_bytes(d[0..4].try_into().unwrap())); Some((0x20000, 0x10000)) }), (0x30000, 0x10000));
        assert_eq!(seen, vec![3]);
        assert_eq!(alloc_info_with(&two, 2, |_| None), ALLOC_FAILED);
        assert_eq!(alloc_step((0x10, 4), (8, 0)), (8, 4));
        assert_eq!(alloc_step((5, 1), (3, 4)), (11, 4));
    }

    fn heap(size: u64, ty: u32, align: u64, flags: u32) -> Vec<u8> {
        let mut d = vec![0u8; HEAP_DESC_SIZE];
        d[0..8].copy_from_slice(&size.to_le_bytes());
        d[8..12].copy_from_slice(&ty.to_le_bytes());
        d[0x20..0x28].copy_from_slice(&align.to_le_bytes());
        d[0x28..0x2c].copy_from_slice(&flags.to_le_bytes());
        d
    }

    #[test]
    fn heap_desc_follows_release_create_heap_0x4d3e0() {
        assert_eq!(heap_desc_for_pc(&heap(0x10000, 1, 0, 0x8)), Err(E_INVALIDARG));
        let d = heap_desc_for_pc(&heap(0x12345, 1, 0x100, 0x2 | 0x4)).unwrap();
        assert_eq!(u64::from_le_bytes(d[0..8].try_into().unwrap()), 0x12400);
        assert_eq!(u64::from_le_bytes(d[0x20..0x28].try_into().unwrap()), 0);
        assert_eq!(xs_u32(&d, 0x28), 0);
        assert_eq!(xs_u32(&d, 8), 1);
        let z = heap_desc_for_pc(&heap(0, 2, 0x20_0000, 0)).unwrap();
        assert_eq!(u64::from_le_bytes(z[0..8].try_into().unwrap()), PC_HEAP_MIN);
        assert_eq!(u64::from_le_bytes(z[0x20..0x28].try_into().unwrap()), PC_HEAP_ALIGN_MSAA);
        let big = heap_desc_for_pc(&heap(1 << 24, 1, 1 << 23, 0)).unwrap();
        assert_eq!(u64::from_le_bytes(big[0x20..0x28].try_into().unwrap()), 1 << 23);
    }

    #[test]
    fn single_category_deny_heaps_become_valid_pc_heaps() {
        for (x, pc) in [(0u32, 0u32), (0x4, 0), (0x40, 0), (0x80, 0), (0x44, 0x44), (0x84, 0x84), (0xc0, 0xc0), (0x4 | 0x1, 0x1), (0x40 | 0x100, 0x100), (0x44 | 0x1, 0x45)] {
            assert_eq!(pc_heap_deny_flags(x), pc, "flags {x:#x}");
            assert_eq!(xs_u32(&heap_desc_for_pc(&heap(0x10000, 1, 0, x)).unwrap(), 0x28), pc, "flags {x:#x}");
        }
    }

    #[test]
    fn null_sampler_desc_uses_the_release_driver_default_0x12d600() {
        let x = default_xsamp();
        let d = sampler_desc_from_xbox(&x);
        let g = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        assert_eq!(g(0x00), 0x15);
        assert_eq!((g(0x04), g(0x08), g(0x0c)), (3, 3, 3));
        assert_eq!((g(0x14), g(0x18)), (16, 1));
        assert_eq!((f32::from_bits(g(0x2c)), f32::from_bits(g(0x30))), (f32::MIN, f32::MAX));
        assert!(sampler_unmapped_fields(&x).is_empty());
    }

    #[test]
    fn custom_heap_properties_match_release_body_0x46cf0() {
        assert_eq!(custom_heap_properties(1), HeapProperties { heap_type: 1, cpu_page_property: 2, memory_pool_preference: 1, creation_node_mask: 1, visible_node_mask: 1 });
        assert_eq!(custom_heap_properties(2).cpu_page_property, 2);
        assert_eq!(custom_heap_properties(3).cpu_page_property, 3);
        assert_eq!(custom_heap_properties(4).cpu_page_property, 0);
        assert_eq!(custom_heap_properties(9), HeapProperties { heap_type: 9, cpu_page_property: 0, memory_pool_preference: 1, creation_node_mask: 1, visible_node_mask: 1 });
        assert_eq!(core::mem::size_of::<HeapProperties>(), 20);
        let mut out = HeapProperties::default();
        let r = unsafe { get_custom_heap_properties_x(&mut out, 0x10 as *mut c_void, 0x3, 3) };
        assert_eq!(r, &mut out as *mut HeapProperties);
        assert_eq!(out, custom_heap_properties(3));
        assert_eq!(pc_heap_properties(custom_heap_properties(1)), HeapProperties { heap_type: 1, cpu_page_property: 0, memory_pool_preference: 0, creation_node_mask: 1, visible_node_mask: 1 });
        let custom = HeapProperties { heap_type: 4, cpu_page_property: 3, memory_pool_preference: 1, creation_node_mask: 1, visible_node_mask: 1 };
        assert_eq!(pc_heap_properties(custom), custom);
        assert_eq!(custom_heap_slot(), (26, get_custom_heap_properties_x as usize));
    }

    #[test]
    fn fixed_bodies_return_the_release_driver_values() {
        let t = 0x20 as *mut c_void;
        unsafe {
            assert_eq!(set_name_x(t, core::ptr::null()), S_OK);
            assert_eq!(get_node_count_x(t), 1);
            assert_eq!(notimpl_x(t), E_NOTIMPL);
            assert_eq!(get_device_removed_reason_x(t), S_OK);
            assert_eq!(set_stable_power_state_x(t, 1), E_FAIL);
            assert_eq!(get_adapter_luid_x(t), 0);
        }
    }

    #[test]
    fn plan_covers_the_trivial_pc_mirrored_slots() {
        let slots: Vec<usize> = plan().iter().map(|p| p.0).collect();
        assert_eq!(slots, vec![6, 7, 37, 40, 43, 31, 32, 33, 34, 35, 26]);
        let mut v: Vec<usize> = (0..90).map(|i| 1000 + i).collect();
        let defaults = v.clone();
        crate::console_slots::install_device(&mut v, &defaults);
        for (s, f) in plan() {
            assert_eq!(v[s], f);
        }
    }

    #[test]
    fn slot29_widened_buffer_leaves_the_previous_buffer_in_the_heap_intact() {
        const IID_RES1: Guid = Guid { d1: 0x9d5e_227a, d2: 0x4430, d3: 0x4161, d4: [0x88, 0xb3, 0x3e, 0xca, 0x6b, 0xb1, 0x6e, 0x19] };
        const IID_BOGUS: Guid = Guid { d1: 0x1234_5678, d2: 0x9abc, d3: 0xdef0, d4: [1, 2, 3, 4, 5, 6, 7, 8] };
        unsafe {
            let p = crate::host_proc(b"D3D12CreateDevice ");
            let mk: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(mk(core::ptr::null_mut(), 0xb000, &IID_DEV, &mut dev) >= 0 && !dev.is_null());
            let w = crate::device_wrap::wrap_device(dev);
            let heap: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 28));
            let placed: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(w, 29));
            let buf = |width: u64| {
                let mut b = rdesc(1, 0, width);
                b[0x18..0x1c].copy_from_slice(&1u32.to_le_bytes());
                b[0x1c..0x1e].copy_from_slice(&1u16.to_le_bytes());
                b[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
                b[0x24..0x28].copy_from_slice(&1u32.to_le_bytes());
                b[0x2c..0x30].copy_from_slice(&1u32.to_le_bytes());
                b
            };
            let mut hd = vec![0u8; HEAP_DESC_SIZE];
            hd[0..8].copy_from_slice(&0x3_0000u64.to_le_bytes());
            hd[8..12].copy_from_slice(&2u32.to_le_bytes());
            hd[0x28..0x2c].copy_from_slice(&0xc0u32.to_le_bytes());
            let mut h: *mut c_void = core::ptr::null_mut();
            assert!(heap(w, hd.as_ptr(), &IID_HEAP, &mut h) >= 0 && !h.is_null());
            let map = |r: *mut c_void| {
                let m: unsafe extern "system" fn(*mut c_void, u32, *const c_void, *mut *mut u8) -> i32 = core::mem::transmute(slot(r, 8));
                let mut q: *mut u8 = core::ptr::null_mut();
                assert!(m(r, 0, core::ptr::null(), &mut q) >= 0 && !q.is_null());
                q
            };
            let a_len = 0x1_2000usize;
            let mut a: *mut c_void = core::ptr::null_mut();
            assert!(placed(w, h, 0, buf(a_len as u64).as_ptr(), 0xac3, core::ptr::null(), &IID_RES, &mut a) >= 0 && !a.is_null());
            let pa = map(a);
            for i in 0..a_len {
                *pa.add(i) = (i * 7 + 3) as u8;
            }
            let mut b: *mut c_void = core::ptr::null_mut();
            assert_eq!(placed(w, h, a_len as u64, buf(0x1000).as_ptr(), 0xac3, core::ptr::null(), &IID_RES1, &mut b), 0x8000_4002u32 as i32, "release xgs12 0x4765f compares riid with IID_ID3D12Resource only; ID3D12Resource1 gets E_NOINTERFACE");
            assert!(b.is_null());
            assert!(placed(w, h, a_len as u64, buf(0x1000).as_ptr(), 0xac3, core::ptr::null(), &IID_RES, &mut b) >= 0 && !b.is_null());
            assert_eq!(crate::placed_offset::bytes(b as usize), 0x2000, "B is widened down to the 64 KB base and overlaps A's last 0x2000 bytes");
            let intact = |what: &str| {
                for i in 0..a_len {
                    assert_eq!(*pa.add(i), (i * 7 + 3) as u8, "A byte {i:#x} changed {what}");
                }
            };
            intact("by creating the widened B");
            let pb = map(b);
            assert_eq!(pb as usize - pa as usize, a_len, "B's mapped bytes start right after A");
            for i in 0..0x1000 {
                *pb.add(i) = 0xee;
            }
            intact("by writing all of B");
            assert_eq!(*pa.add(a_len), 0xee);
            let qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(slot(b, 0));
            let mut b1: *mut c_void = core::ptr::null_mut();
            assert!(qi(b, &IID_RES1, &mut b1) >= 0 && b1 == b, "the ID3D12Resource returned for an ID3D12Resource1 request is the same object");
            rel(b1);
            let mut c: *mut c_void = core::ptr::null_mut();
            let hr = placed(w, h, 0x2_0000, buf(0x100).as_ptr(), 0xac3, core::ptr::null(), &IID_BOGUS, &mut c);
            assert_eq!((hr, c), (0x8000_4002u32 as i32, core::ptr::null_mut()), "slot 29 with an unrelated riid returns E_NOINTERFACE and creates nothing");
            rel(b);
            rel(a);
            rel(h);
            rel(w);
        }
    }

    fn tdesc(w: u64, h: u32, da: u16, fmt: u32, samples: u32, flags: u32) -> Vec<u8> {
        let mut d = vec![0u8; RES_DESC_SIZE];
        d[0..4].copy_from_slice(&3u32.to_le_bytes());
        d[0x10..0x18].copy_from_slice(&w.to_le_bytes());
        d[0x18..0x1c].copy_from_slice(&h.to_le_bytes());
        d[0x1c..0x1e].copy_from_slice(&da.to_le_bytes());
        d[0x1e..0x20].copy_from_slice(&1u16.to_le_bytes());
        d[0x20..0x24].copy_from_slice(&fmt.to_le_bytes());
        d[0x24..0x28].copy_from_slice(&samples.to_le_bytes());
        d[0x30..0x34].copy_from_slice(&flags.to_le_bytes());
        d
    }

    fn load_emulated_xg(name: &str) -> bool {
        let exe = std::env::current_exe().unwrap();
        let Some(p) = exe.ancestors().map(|a| a.join(name)).find(|p| p.is_file()) else {
            eprintln!("{name} not built next to {}; run cargo build -p xg_x -p xg_xs for the family XG tests", exe.display());
            return false;
        };
        let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        !unsafe { LoadLibraryA(c.as_ptr() as *const u8) }.is_null()
    }

    #[test]
    fn layout_family_follows_the_title_driver() {
        use crate::console_slots_dev2::Family;
        assert_eq!(title_family_from(true), Family::XboxOne);
        assert_eq!(title_family_from(false), Family::Scarlett);
        assert_eq!(texture_part(Ok((0x100, 0x40)), || unreachable!()), Some((0x100, 0x40)));
        assert_eq!(texture_part(Err(XG_E_NOTIMPL), || (0x20000, 0x10000)), Some((0x20000, 0x10000)));
        assert_eq!(texture_part(Err(E_INVALIDARG), || unreachable!()), None);
    }

    #[test]
    fn xbox_one_alloc_info_uses_xg_x_like_xgs12_0x46b90() {
        use crate::console_slots_dev2::Family;
        if !load_emulated_xg("xg_x.dll") {
            return;
        }
        let one = |d: &[u8]| alloc_info_with(d, 1, |d| texture_part(xg_texture_alloc(d, Family::XboxOne), || (1, 1)));
        assert_eq!(one(&tdesc(2048, 2048, 2, 56, 1, 0)), (0x100_0000, 0x100));
        assert_eq!(one(&tdesc(1920, 1080, 1, 19, 2, 2)), (0xff_0000 + 0x3f_c000, 0x2000));
        assert_eq!(one(&tdesc(1920, 1080, 1, 61, 2, 1)), (0x3f_c000, 0x2000));
        let mut two = tdesc(1920, 1080, 1, 61, 2, 1);
        two.extend(rdesc(1, 0, 100));
        assert_eq!(alloc_info_with(&two, 2, |d| xg_texture_alloc(d, Family::XboxOne).ok()), (0x3f_c000 + 100, 0x2000));
    }

    #[test]
    fn scarlett_alloc_info_uses_xg_xs_and_never_returns_zero_for_an_emulator_gap() {
        use crate::console_slots_dev2::Family;
        if !load_emulated_xg("xg_xs.dll") {
            return;
        }
        let sc = |d: &[u8]| alloc_info_with(d, 1, |d| texture_part(xg_texture_alloc(d, Family::Scarlett), || (0x7777_0000, 0x10000)));
        let rt = sc(&tdesc(1920, 1080, 1, 27, 1, 1));
        assert!(rt.0 >= 1920 * 1080 * 4 && rt.1 != 0, "{rt:x?}");
        let gap = tdesc(1920, 1080, 1, 19, 2, 2);
        match xg_texture_alloc(&gap, Family::Scarlett) {
            Err(XG_E_NOTIMPL) => assert_eq!(sc(&gap), (0x7777_0000, 0x10000)),
            Ok(p) => assert!(p.0 != 0 && p.1 != 0, "{p:x?}"),
            Err(hr) => panic!("unexpected {hr:#x}"),
        }
        let zero = tdesc(0, 0, 1, 26, 1, 1);
        assert_eq!(xg_texture_alloc(&zero, Family::Scarlett), Err(E_INVALIDARG));
        assert_eq!(sc(&zero), ALLOC_FAILED);
    }
}
