mod bvh;
mod computer;
mod equations;
mod equations_v4;
mod equations_ms;
mod formats;
mod layout;
mod forward;
mod tile;
mod descriptor;
mod equations3d;
mod layout3d;
mod computer3d;
#[cfg(all(test, feature = "console-oracle"))]
mod oracle;
#[cfg(all(test, feature = "console-oracle"))]
mod oracle_3d;
#[cfg(all(test, feature = "console-oracle"))]
mod oracle_xs;
#[cfg(all(test, feature = "console-oracle"))]
mod oracle_msaa;
#[cfg(test)]
mod exports_table;
mod dcc;
#[cfg(all(test, feature = "console-oracle"))]
mod dcc_oracle;

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};
use compat_common::{record, E_NOTIMPL, S_OK};
use layout::{Tex2DDesc, E_INVALIDARG, LAYOUT_BYTES};

const M: &str = "xg_xs";

#[repr(C)]
struct CriticalSection([usize; 5]);

extern "system" {
    fn InitializeCriticalSection(cs: *mut CriticalSection);
    fn EnterCriticalSection(cs: *mut CriticalSection);
    fn LeaveCriticalSection(cs: *mut CriticalSection);
}

static mut LOCK: CriticalSection = CriticalSection([0; 5]);
static LOCK_READY: std::sync::Once = std::sync::Once::new();
static LOCKING: AtomicBool = AtomicBool::new(false);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

fn lock_ptr() -> *mut CriticalSection {
    LOCK_READY.call_once(|| unsafe { InitializeCriticalSection(core::ptr::addr_of_mut!(LOCK)) });
    core::ptr::addr_of_mut!(LOCK)
}

struct Guard(bool);

impl Guard {
    fn take() -> Guard {
        if LOCKING.load(Ordering::Acquire) {
            unsafe { EnterCriticalSection(lock_ptr()) };
            Guard(true)
        } else {
            Guard(false)
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { LeaveCriticalSection(lock_ptr()) };
        }
    }
}

macro_rules! console {
    ($name:expr, fn($($t:ty),*) -> $r:ty, $($a:expr),*) => {
        if let Some(f) = forward::console(concat!($name, "\0")) {
            let f: unsafe extern "system" fn($($t),*) -> $r = core::mem::transmute(f);
            return f($($a),*);
        }
    };
}

#[no_mangle]
pub unsafe extern "system" fn XGInitializeLibrary() -> i32 {
    console!("XGInitializeLibrary", fn() -> i32,);
    if INITIALIZED.swap(true, Ordering::AcqRel) { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "system" fn XGTerminateLibrary() -> i32 {
    console!("XGTerminateLibrary", fn() -> i32,);
    if INITIALIZED.swap(false, Ordering::AcqRel) { 0 } else { 1 }
}

#[no_mangle]
pub unsafe extern "system" fn XGSetHardwareVersion(version: u32) -> i32 {
    console!("XGSetHardwareVersion", fn(u32) -> i32, version);
    layout::HARDWARE_V4.store(version == 4, Ordering::Release);
    record(M, "XGSetHardwareVersion", &format!("version={version}"));
    if INITIALIZED.swap(true, Ordering::AcqRel) { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "system" fn XGEnableExclusiveLocking(enable: i32) {
    if let Some(f) = forward::console("XGEnableExclusiveLocking\0") {
        let f: unsafe extern "system" fn(i32) = core::mem::transmute(f);
        return f(enable);
    }
    lock_ptr();
    LOCKING.store(enable != 0, Ordering::Release);
}

#[no_mangle]
pub unsafe extern "system" fn XGEnterExclusiveLock() -> i32 {
    console!("XGEnterExclusiveLock", fn() -> i32,);
    if LOCKING.load(Ordering::Acquire) {
        EnterCriticalSection(lock_ptr());
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "system" fn XGLeaveExclusiveLock() {
    if let Some(f) = forward::console("XGLeaveExclusiveLock\0") {
        let f: unsafe extern "system" fn() = core::mem::transmute(f);
        return f();
    }
    if LOCKING.load(Ordering::Acquire) {
        LeaveCriticalSection(lock_ptr());
    }
}

pub fn buffer_layout(byte_width: u32, bind: u32, misc: u32, out: &mut [u8]) -> i32 {
    if byte_width == 0 || bind & 0xffff_f900 != 0 || misc & 0xf385_6d0a != 0 {
        return E_INVALIDARG;
    }
    if misc & 0x0400_0000 != 0 && byte_width & 0xffff != 0 {
        return E_INVALIDARG;
    }
    out[..LAYOUT_BYTES].fill(0);
    out[0..8].copy_from_slice(&(byte_width as u64).to_le_bytes());
    let align: u64 = if misc & 0x0c00_0000 != 0 { 0x10000 } else { 4 };
    out[8..16].copy_from_slice(&align.to_le_bytes());
    out[0x10..0x18].copy_from_slice(&1u64.to_le_bytes());
    out[0x1b38..0x1b3c].copy_from_slice(&1u32.to_le_bytes());
    S_OK
}

#[no_mangle]
pub unsafe extern "system" fn XGComputeBufferLayout(desc: *const u8, out: *mut u8) -> i32 {
    console!("XGComputeBufferLayout", fn(*const u8, *mut u8) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    let u = |o: usize| core::ptr::read_unaligned(desc.add(o) as *const u32);
    buffer_layout(u(0), u(8), u(0x10), core::slice::from_raw_parts_mut(out, LAYOUT_BYTES))
}

pub fn optimal_swizzle(dimcode: u32, format: u32, width: u64, height: u32, depth: u32, samples: u32, rt: bool, uav: bool, misc27: bool) -> u32 {
    let bits = formats::BITS_PER_PIXEL[(format & 0xff) as usize] as u64;
    let uav = uav && !layout::legacy_19041();
    let small = |extra: u64| bits != 0 && (bits.wrapping_mul(width).wrapping_mul(height as u64).wrapping_mul(extra) & !7) <= 0x80000;
    match dimcode {
        3 | 5 => {
            if misc27 {
                10
            } else if rt {
                27
            } else if !uav {
                5
            } else if samples == 1 && small(1) {
                21
            } else {
                27
            }
        }
        4 => {
            if misc27 {
                10
            } else if !rt && !uav {
                5
            } else if !rt && small(depth as u64) {
                21
            } else {
                27
            }
        }
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "system" fn XGComputeOptimalSwizzleMode(dim: u32, format: u32, width: u64, height: u32, depth: u32, samples: u32, bind: u32, misc: u32) -> u32 {
    console!("XGComputeOptimalSwizzleMode", fn(u32, u32, u64, u32, u32, u32, u32, u32) -> u32, dim, format, width, height, depth, samples, bind, misc);
    if dim == 1 {
        return 0;
    }
    if bind & 0x40 != 0 {
        let stencil = matches!(format, 19 | 20 | 22 | 44 | 45 | 47 | 62 | 118 | 119 | 120);
        if layout::legacy_19041() && stencil && misc & 0x0800_8000 != 0 {
            record(M, "XGComputeOptimalSwizzleMode", "stencil format with misc 0x8008000 rejected (19041 rule)");
            return u32::MAX;
        }
        return 0x18;
    }
    let code = match dim {
        2 => 2,
        3 => 3,
        4 => 4,
        _ => return 0,
    };
    optimal_swizzle(code, format, width, height, depth, samples, bind & 0x20 != 0, bind & 0x80 != 0, misc >> 27 & 1 != 0)
}

#[no_mangle]
pub unsafe extern "system" fn XGComputeOptimalDepthStencilSwizzleMode(a1: usize, a2: usize, a3: usize, a4: usize, a5: usize, a6: usize, a7: usize, a8: usize, stencil: *mut u32) -> u32 {
    console!("XGComputeOptimalDepthStencilSwizzleMode", fn(usize, usize, usize, usize, usize, usize, usize, usize, *mut u32) -> u32, a1, a2, a3, a4, a5, a6, a7, a8, stencil);
    if !stencil.is_null() {
        *stencil = 0x18;
    }
    0x18
}

#[no_mangle]
pub unsafe extern "system" fn XGValidateDepthStencilSwizzleMode(a1: usize, a2: usize, mode: u32) -> i32 {
    console!("XGValidateDepthStencilSwizzleMode", fn(usize, usize, u32) -> i32, a1, a2, mode);
    if (4..=28).contains(&mode) && mode % 4 == 0 {
        S_OK
    } else {
        compat_common::E_FAIL
    }
}

pub struct D3D12Desc {
    pub dimension: u32,
    pub width: u64,
    pub height: u32,
    pub depth_or_array: u16,
    pub mips: u16,
    pub format: u32,
    pub samples: u32,
    pub quality: u32,
    pub layout: u32,
    pub flags: u32,
}

impl D3D12Desc {
    pub unsafe fn read(p: *const u8) -> D3D12Desc {
        let u = |o: usize| core::ptr::read_unaligned(p.add(o) as *const u32);
        D3D12Desc {
            dimension: u(0),
            width: core::ptr::read_unaligned(p.add(0x10) as *const u64),
            height: u(0x18),
            depth_or_array: core::ptr::read_unaligned(p.add(0x1c) as *const u16),
            mips: core::ptr::read_unaligned(p.add(0x1e) as *const u16),
            format: u(0x20),
            samples: u(0x24),
            quality: u(0x28),
            layout: u(0x2c),
            flags: u(0x30),
        }
    }

    pub fn to_tex2d(&self) -> Result<Tex2DDesc, layout::Fail> {
        use layout::Fail;
        if self.dimension <= 1 {
            return Err(Fail::Invalid("dimension"));
        }
        let fl = self.flags;
        let mut bind = 8 | (fl & 2) << 5;
        if fl & 1 != 0 {
            bind |= 0x20;
        }
        if fl & 4 != 0 {
            bind |= 0x80;
        }
        if fl & 8 != 0 {
            bind &= !8;
        }
        let mut misc = if fl & 0x20 != 0 { 0x38_0000 } else { 0 };
        if fl & 0x30_0000 != 0 && bind & 0x20 != 0 {
            misc |= 0x8_0000;
        }
        if fl & 0x40_0000 != 0 {
            misc |= 0x40_0000;
        }
        if fl & 0x80_0000 != 0 {
            misc |= 0x8000;
        }
        let lay = self.layout;
        let mode = if lay & !2 == 0 {
            if lay == 2 {
                misc |= 0x0800_0000;
            }
            if bind & 0x40 != 0 {
                24
            } else {
                let code = match self.dimension {
                    2 => 2,
                    3 => 3,
                    4 => 4,
                    _ => 0,
                };
                if code == 0 {
                    0
                } else {
                    optimal_swizzle(code, self.format, self.width, self.height, self.depth_or_array as u32, self.samples, bind & 0x20 != 0, bind & 0x80 != 0, misc >> 27 & 1 != 0)
                }
            }
        } else if lay == 1 {
            0
        } else if (0x100..=0x11f).contains(&lay) {
            if (0x110..=0x113).contains(&lay) && !CONSOLE_DS_FORMATS.contains(&self.format) && self.dimension != 4 {
                misc |= 0x0800_0000;
            }
            lay & 0x1f
        } else {
            return Err(Fail::Invalid("layout"));
        };
        if self.dimension != 3 {
            return Err(Fail::Unsupported("1D/3D texture"));
        }
        Ok(Tex2DDesc {
            width: self.width as u32,
            height: self.height,
            mip_levels: self.mips as u32,
            array_size: self.depth_or_array as u32,
            format: self.format,
            sample_count: self.samples,
            sample_quality: self.quality,
            usage: if mode == 0 { 3 } else { 0 },
            bind_flags: bind,
            cpu_access: 0,
            misc_flags: misc,
            swizzle_mode: mode,
            reserved: 0,
        })
    }
}

pub const CONSOLE_DS_FORMATS: [u32; 13] = [19, 20, 22, 44, 45, 47, 103, 104, 105, 106, 110, 118, 120];

unsafe fn create_2d(d: &Tex2DDesc, out: *mut *mut c_void, who: &str) -> i32 {
    match layout::compute(d) {
        Ok(l) => {
            *out = computer::create(l);
            S_OK
        }
        Err(e) => {
            record(M, who, &format!("native {:#010x}: {} ({:?})", e.hr() as u32, e.why(), d));
            e.hr()
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn XGCreateTextureComputer(desc: *const u8, out: *mut *mut c_void) -> i32 {
    console!("XGCreateTextureComputer", fn(*const u8, *mut *mut c_void) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    let d = D3D12Desc::read(desc);
    if d.dimension == 2 || d.dimension == 4 {
        *out = core::ptr::null_mut();
        return create_1d_3d(layout3d::from_d3d12(&d), out, "XGCreateTextureComputer");
    }
    if d.dimension > 4 {
        record(M, "XGCreateTextureComputer", &format!("native E_INVALIDARG: dimension {}", d.dimension));
        return E_INVALIDARG;
    }
    match d.to_tex2d() {
        Ok(t) => create_2d(&t, out, "XGCreateTextureComputer"),
        Err(e) => {
            record(M, "XGCreateTextureComputer", &format!("native {:#010x}: {} dim={} layout={:#x}", e.hr() as u32, e.why(), d.dimension, d.layout));
            e.hr()
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn XGCreateTexture2DComputer(desc: *const u8, out: *mut *mut c_void) -> i32 {
    console!("XGCreateTexture2DComputer", fn(*const u8, *mut *mut c_void) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    *out = core::ptr::null_mut();
    create_2d(&Tex2DDesc::read(desc), out, "XGCreateTexture2DComputer")
}

#[no_mangle]
pub unsafe extern "system" fn XGComputeTexture2DLayout(desc: *const u8, out: *mut u8) -> i32 {
    console!("XGComputeTexture2DLayout", fn(*const u8, *mut u8) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    match layout::compute(&Tex2DDesc::read(desc)) {
        Ok(l) => {
            l.write(core::slice::from_raw_parts_mut(out, LAYOUT_BYTES));
            S_OK
        }
        Err(e) => {
            record(M, "XGComputeTexture2DLayout", &format!("native {:#010x}: {}", e.hr() as u32, e.why()));
            e.hr()
        }
    }
}

macro_rules! unimplemented_export {
    ($name:ident, $ret:expr) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name(a1: usize, a2: usize, a3: usize, a4: usize, a5: usize, a6: usize, a7: usize, a8: usize) -> usize {
            console!(stringify!($name), fn(usize, usize, usize, usize, usize, usize, usize, usize) -> usize, a1, a2, a3, a4, a5, a6, a7, a8);
            record(M, stringify!($name), "[native E_NOTIMPL] no console xg_xs.dll configured (TOMBOX_XG_CONSOLE)");
            $ret
        }
    };
}

unsafe fn create_1d_3d(r: Result<layout3d::Surface, layout::Fail>, out: *mut *mut c_void, who: &str) -> i32 {
    match r {
        Ok(s) => {
            *out = computer3d::create(s);
            S_OK
        }
        Err(e) => {
            record(M, who, &format!("native {:#010x}: {}", e.hr() as u32, e.why()));
            e.hr()
        }
    }
}

unsafe fn layout_1d_3d(r: Result<layout3d::Surface, layout::Fail>, out: *mut u8, who: &str) -> i32 {
    match r {
        Ok(s) => {
            s.write(core::slice::from_raw_parts_mut(out, LAYOUT_BYTES));
            S_OK
        }
        Err(e) => {
            record(M, who, &format!("native {:#010x}: {}", e.hr() as u32, e.why()));
            e.hr()
        }
    }
}

fn surface_1d(d: &layout3d::Tex1DDesc) -> Result<layout3d::Surface, layout::Fail> {
    Ok(layout3d::Surface::One { l: layout3d::compute_1d(d)?, srd_height: 0 })
}

fn surface_3d(d: &layout3d::Tex3DDesc) -> Result<layout3d::Surface, layout::Fail> {
    Ok(layout3d::Surface::Three(layout3d::compute_3d(d)?))
}

#[no_mangle]
pub unsafe extern "system" fn XGComputeTexture1DLayout(desc: *const u8, out: *mut u8) -> i32 {
    console!("XGComputeTexture1DLayout", fn(*const u8, *mut u8) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    layout_1d_3d(surface_1d(&layout3d::Tex1DDesc::read(desc)), out, "XGComputeTexture1DLayout")
}

#[no_mangle]
pub unsafe extern "system" fn XGComputeTexture3DLayout(desc: *const u8, out: *mut u8) -> i32 {
    console!("XGComputeTexture3DLayout", fn(*const u8, *mut u8) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    layout_1d_3d(surface_3d(&layout3d::Tex3DDesc::read(desc)), out, "XGComputeTexture3DLayout")
}

#[no_mangle]
pub unsafe extern "system" fn XGCreateTexture1DComputer(desc: *const u8, out: *mut *mut c_void) -> i32 {
    console!("XGCreateTexture1DComputer", fn(*const u8, *mut *mut c_void) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    *out = core::ptr::null_mut();
    create_1d_3d(surface_1d(&layout3d::Tex1DDesc::read(desc)), out, "XGCreateTexture1DComputer")
}

#[no_mangle]
pub unsafe extern "system" fn XGCreateTexture3DComputer(desc: *const u8, out: *mut *mut c_void) -> i32 {
    console!("XGCreateTexture3DComputer", fn(*const u8, *mut *mut c_void) -> i32, desc, out);
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    *out = core::ptr::null_mut();
    create_1d_3d(surface_3d(&layout3d::Tex3DDesc::read(desc)), out, "XGCreateTexture3DComputer")
}
#[no_mangle]
pub unsafe extern "system" fn XGCreateTextureComputerFromDescriptor(desc: *const u32, out: *mut *mut c_void) -> i32 {
    if desc.is_null() || out.is_null() {
        return E_INVALIDARG;
    }
    let _g = Guard::take();
    let mut d = [0u32; 8];
    for (i, v) in d.iter_mut().enumerate() {
        *v = core::ptr::read_unaligned(desc.add(i));
    }
    *out = core::ptr::null_mut();
    if let Some(r) = layout3d::from_srd(&d) {
        return create_1d_3d(r, out, "XGCreateTextureComputerFromDescriptor");
    }
    match descriptor::layout_from_srd(&d) {
        Ok(l) => {
            *out = computer::create(l);
            S_OK
        }
        Err(e) => {
            record(M, "XGCreateTextureComputerFromDescriptor", &format!("native {:#010x}: {} srd={d:08x?}", e.hr() as u32, e.why()));
            e.hr()
        }
    }
}
#[no_mangle]
pub unsafe extern "system" fn XGCreateBVHComputer(out: *mut *mut c_void) -> i32 {
    bvh::create(out)
}
#[no_mangle]
pub unsafe extern "system" fn XGComputeTileShape(dim: u32, format: u32, bytes_per_element: u32, samples: u32, texels: *mut tile::TileShape, elements: *mut tile::TileShape) -> i32 {
    tile::compute(dim, format, bytes_per_element, samples, texels.as_mut(), elements.as_mut())
}
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "system" fn XGEncodeDCC(count: u32, src: *const u8, dst: *mut u8, meta: *mut u8, sizes: *mut u32, format: u32, swizzle: u32, dim: u32, params: *const u32) -> i32 {
    core::arch::naked_asm!(
        "push rcx", "push rdx", "push r8", "push r9",
        "lea rcx, [rsp + 0x20]",
        "sub rsp, 0x28",
        "call {set}",
        "add rsp, 0x28",
        "pop r9", "pop r8", "pop rdx", "pop rcx",
        "jmp {imp}",
        set = sym dcc::set_entry,
        imp = sym encode_dcc_entry,
    )
}
unsafe extern "system" fn encode_dcc_entry(count: u32, src: *const u8, dst: *mut u8, meta: *mut u8, sizes: *mut u32, format: u32, swizzle: u32, dim: u32, params: *const u32) -> i32 {
    let e = dcc::take_entry();
    dcc::with_entry(e, || dcc::encode_export(count, src, dst, meta, sizes, format, swizzle, dim, params))
}
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "system" fn XGDecodeDCC(count: u32, src: *const u8, meta: *const u8, dst: *mut u8, format: u32, swizzle: u32, dim: u32, params: *const u32) -> i32 {
    core::arch::naked_asm!(
        "push rcx", "push rdx", "push r8", "push r9",
        "lea rcx, [rsp + 0x20]",
        "sub rsp, 0x28",
        "call {set}",
        "add rsp, 0x28",
        "pop r9", "pop r8", "pop rdx", "pop rcx",
        "jmp {imp}",
        set = sym dcc::set_entry,
        imp = sym decode_dcc_entry,
    )
}
unsafe extern "system" fn decode_dcc_entry(count: u32, src: *const u8, meta: *const u8, dst: *mut u8, format: u32, swizzle: u32, dim: u32, params: *const u32) -> i32 {
    let e = dcc::take_entry();
    dcc::with_entry(e, || dcc::decode_export(count, src, meta, dst, format, swizzle, dim, params))
}
#[no_mangle]
pub unsafe extern "system" fn XGValidateDescriptor(kind: u32, desc: *const u32, flags: u32) -> i32 {
    let mut d = [0u32; 8];
    if !desc.is_null() {
        let n = if kind == 8 { 8 } else if kind == 4 { 4 } else { 0 };
        for (i, v) in d.iter_mut().enumerate().take(n) {
            *v = core::ptr::read_unaligned(desc.add(i));
        }
    }
    descriptor::validate(kind, if desc.is_null() { None } else { Some(&d) }, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_init_cycle() {
        unsafe {
            INITIALIZED.store(false, Ordering::SeqCst);
            assert_eq!(XGInitializeLibrary(), 0);
            assert_eq!(XGInitializeLibrary(), 1);
            assert_eq!(XGTerminateLibrary(), 0);
            assert_eq!(XGTerminateLibrary(), 1);
        }
    }

    #[test]
    fn buffer_layout_rules() {
        let mut b = vec![0xffu8; LAYOUT_BYTES];
        assert_eq!(buffer_layout(0, 0, 0, &mut b), E_INVALIDARG);
        assert_eq!(buffer_layout(100, 0x100, 0, &mut b), E_INVALIDARG);
        assert_eq!(buffer_layout(100, 0, 0x0400_0000, &mut b), E_INVALIDARG);
        assert_eq!(buffer_layout(0x20000, 0, 0x0400_0000, &mut b), S_OK);
        assert_eq!(u64::from_le_bytes(b[8..16].try_into().unwrap()), 0x10000);
        assert_eq!(buffer_layout(100, 0x8, 0, &mut b), S_OK);
        assert_eq!(u64::from_le_bytes(b[0..8].try_into().unwrap()), 100);
        assert_eq!(u64::from_le_bytes(b[8..16].try_into().unwrap()), 4);
        assert_eq!(u32::from_le_bytes(b[0x1b38..0x1b3c].try_into().unwrap()), 1);
    }

    #[test]
    fn optimal_swizzle_rules() {
        layout::set_legacy_19041(false);
        assert_eq!(optimal_swizzle(3, 28, 256, 256, 1, 1, false, false, false), 5);
        assert_eq!(optimal_swizzle(3, 28, 256, 256, 1, 1, true, false, false), 27);
        assert_eq!(optimal_swizzle(3, 28, 128, 128, 1, 1, false, true, false), 21);
        assert_eq!(optimal_swizzle(3, 28, 256, 256, 1, 1, false, true, false), 27);
        assert_eq!(optimal_swizzle(3, 28, 512, 512, 1, 1, false, true, false), 27);
        assert_eq!(optimal_swizzle(3, 28, 256, 256, 1, 1, false, false, true), 10);
        assert_eq!(optimal_swizzle(2, 28, 256, 1, 1, 1, false, false, false), 0);
    }

    #[test]
    fn validate_depth_modes() {
        unsafe {
            for m in 0..32 {
                let ok = XGValidateDepthStencilSwizzleMode(0, 0, m) == S_OK;
                assert_eq!(ok, m >= 4 && m % 4 == 0 && m <= 28, "mode {m}");
            }
        }
    }

    #[test]
    fn d3d12_desc_conversion() {
        let d = D3D12Desc { dimension: 3, width: 64, height: 64, depth_or_array: 1, mips: 1, format: 28, samples: 1, quality: 0, layout: 0x105, flags: 0 };
        let t = d.to_tex2d().unwrap();
        assert_eq!((t.swizzle_mode, t.bind_flags, t.usage), (5, 8, 0));
        let d = D3D12Desc { layout: 1, flags: 1 | 4 | 8, ..d };
        let t = d.to_tex2d().unwrap();
        assert_eq!((t.swizzle_mode, t.bind_flags, t.usage), (0, 0xa0, 3));
        let d = D3D12Desc { layout: 3, ..d };
        assert_eq!(d.to_tex2d().unwrap_err().hr(), E_INVALIDARG);
    }
}
