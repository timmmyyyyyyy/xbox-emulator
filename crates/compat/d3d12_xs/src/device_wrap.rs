//! A COM wrapper around a real PC `ID3D12Device` that lets us intercept
//! `QueryInterface` (to supply Xbox-only interfaces such as the merged DXGI
//! device) while forwarding every other `ID3D12Device` method straight to the
//! real device via naked tail-call thunks.
//!
//! Layout: `{ vtable, real, refs }`. The forwarding thunks replace `this` (our
//! wrapper) with `this->real` and jump to the real method at the same vtable
//! slot — ABI-exact for any signature.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use compat_common::record;

const S_OK: i32 = 0;
const E_POINTER: i32 = 0x8000_4003u32 as i32;
const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

#[repr(C)]
#[derive(PartialEq)]
pub struct Guid {
    pub d1: u32,
    pub d2: u16,
    pub d3: u16,
    pub d4: [u8; 8],
}

const IID_IUNKNOWN: Guid = Guid { d1: 0, d2: 0, d3: 0, d4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

/// ID3D12Device through ID3D12Device8 — all resolve to our wrapper so every
/// device call (CreateDescriptorHeap etc.) stays intercepted.
const DEVICE_IIDS: [Guid; 9] = [
    Guid { d1: 0x1898_19F1, d2: 0x1DB6, d3: 0x4B57, d4: [0xBE, 0x54, 0x18, 0x21, 0x33, 0x9B, 0x85, 0xF7] }, // Device
    Guid { d1: 0x77ac_ce80, d2: 0x638e, d3: 0x4e65, d4: [0x88, 0x95, 0xc1, 0xf2, 0x33, 0x86, 0x86, 0x3e] }, // Device1
    Guid { d1: 0x30ba_a41e, d2: 0xb15b, d3: 0x475c, d4: [0xa0, 0xbb, 0x1a, 0xf5, 0xc5, 0xb6, 0x43, 0x28] }, // Device2
    Guid { d1: 0x81da_dc15, d2: 0x2bad, d3: 0x4392, d4: [0x93, 0xc5, 0x10, 0x13, 0x45, 0xc4, 0xaa, 0x98] }, // Device3
    Guid { d1: 0xe865_df17, d2: 0xa9ee, d3: 0x46f9, d4: [0xa4, 0x63, 0x30, 0x98, 0x31, 0x5a, 0xa2, 0xe5] }, // Device4
    Guid { d1: 0x8b4f_173b, d2: 0x2fea, d3: 0x4b80, d4: [0x8f, 0x58, 0x43, 0x07, 0x19, 0x1a, 0xb9, 0x5d] }, // Device5
    Guid { d1: 0xc70b_221b, d2: 0x40e4, d3: 0x4a17, d4: [0x89, 0xaf, 0x02, 0x5a, 0x07, 0x27, 0xa6, 0xdc] }, // Device6
    Guid { d1: 0x5c01_4b53, d2: 0x68a1, d3: 0x4b9b, d4: [0x8b, 0xd1, 0xdd, 0x60, 0x46, 0xb9, 0x35, 0x8b] }, // Device7
    Guid { d1: 0x9218_e6bb, d2: 0xf944, d3: 0x4f7e, d4: [0xa7, 0x5c, 0xb1, 0xb2, 0xc7, 0xb7, 0x01, 0xf3] }, // Device8
];

const IID_XSX_DEVICE8: Guid = Guid { d1: 0x728f1fa2, d2: 0xea11, d3: 0x4a9f, d4: [0x9b, 0x52, 0x9b, 0x42, 0xa4, 0x84, 0xd5, 0xc9] };

fn is_device_iid(g: &Guid) -> bool {
    DEVICE_IIDS.iter().any(|d| d == g)
}

#[repr(C)]
pub struct DeviceWrapper {
    vtable: *const usize,
    real: *mut c_void,
    refs: AtomicU32,
}

fn guid_str(g: &Guid) -> String {
    format!(
        "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        g.d1, g.d2, g.d3, g.d4[0], g.d4[1], g.d4[2], g.d4[3], g.d4[4], g.d4[5], g.d4[6], g.d4[7]
    )
}

unsafe fn real_vtbl(real: *mut c_void) -> *const usize {
    unsafe { *(real as *const *const usize) }
}

unsafe extern "system" fn qi(this: *mut DeviceWrapper, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    if ppv.is_null() || riid.is_null() {
        return E_POINTER;
    }
    unsafe { *ppv = core::ptr::null_mut() };
    let g = unsafe { &*riid };
    if is_device_iid(g) || *g == IID_IUNKNOWN || *g == IID_XSX_DEVICE8 {
        unsafe {
            *ppv = this as *mut c_void;
            (*this).refs.fetch_add(1, Ordering::SeqCst);
        }
        return S_OK;
    }
    // Xbox "DXGIDevice" interface: synthesize it (PC D3D12 devices lack IDXGIDevice).
    if *g == crate::dxgi_device::IID_XBOX_DXGIDEVICE {
        let shim = crate::dxgi_device::create();
        unsafe { *ppv = shim };
        record("d3d12_xs", "device.QueryInterface", "Xbox DXGIDevice -> synthesized shim");
        return S_OK;
    }
    if let Some(hr) = crate::xone_dxgi::query(this as *mut c_void, unsafe { (*this).real }, riid as *const c_void, ppv) { return hr; }
    // Forward anything else to the real device (higher ID3D12Device versions,
    // info-queue, etc.). Log the IID + result so unknown Xbox IIDs surface.
    let real = unsafe { (*this).real };
    let rvt = unsafe { real_vtbl(real) };
    let real_qi: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*rvt) };
    let hr = unsafe { real_qi(real, riid, ppv) };
    record(
        "d3d12_xs",
        "device.QueryInterface",
        &format!("{} -> hr={hr:#010x}{}", guid_str(g), if hr != S_OK { " (UNHANDLED)" } else { "" }),
    );
    if hr == S_OK { hr } else { E_NOINTERFACE }
}

unsafe extern "system" fn addref(this: *mut DeviceWrapper) -> u32 {
    unsafe { (*this).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn release(this: *mut DeviceWrapper) -> u32 {
    let n = unsafe { (*this).refs.fetch_sub(1, Ordering::SeqCst) } - 1;
    if n == 0 {
        unregister_wrapper(this as usize);
        unsafe {
            let real = (*this).real;
            let rvt = real_vtbl(real);
            let real_rel: unsafe extern "system" fn(*mut c_void) -> u32 =
                core::mem::transmute(*rvt.add(2));
            real_rel(real);
            drop(Box::from_raw(this));
        }
    }
    n
}

/// Intercept `CheckFeatureSupport` (slot 13): the Xbox feature structs differ in
/// size from PC, so a verbatim forward gets E_INVALIDARG. Zero the caller buffer
/// and negotiate the largest size <= the requested size that the real device
/// accepts (D3D12 feature structs are append-only, so filling the PC-known prefix
/// and leaving Xbox-only trailing fields zero is sound).
unsafe extern "system" fn check_feature_support(
    this: *mut DeviceWrapper, feature: u32, pdata: *mut c_void, size: u32,
) -> i32 {
    let real = unsafe { (*this).real };
    let rvt = unsafe { real_vtbl(real) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, u32) -> i32 =
        unsafe { core::mem::transmute(*rvt.add(13)) };
    let hr = unsafe { f(real, feature, pdata, size) };
    if hr == S_OK {
        return S_OK;
    }
    if !pdata.is_null() && size > 0 && size <= 65536 {
        unsafe { core::ptr::write_bytes(pdata as *mut u8, 0, size as usize) };
        let mut try_size = size & !3; // 4-byte aligned
        while try_size >= 4 {
            let hr2 = unsafe { f(real, feature, pdata, try_size) };
            if hr2 == S_OK {
                record("d3d12_xs", "CheckFeatureSupport",
                    &format!("feature={feature} xbox_size={size} -> PC size={try_size} S_OK"));
                return S_OK;
            }
            try_size -= 4;
        }
    }
    record("d3d12_xs", "CheckFeatureSupport", &format!("feature={feature} size={size} UNHANDLED hr={hr:#010x}"));
    hr
}

/// Intercept `CreateDescriptorHeap` (slot 14) and wrap the returned heap so the
/// descriptor-handle getters get the Xbox RAX return-convention bridge.
unsafe extern "system" fn create_descriptor_heap(
    this: *mut DeviceWrapper, pdesc: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let xone_desc = crate::xone_heap::translate(pdesc); let pdesc = xone_desc.as_ref().map_or(pdesc, |d| d.as_ptr() as *const c_void);
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(14)) };
    let hr = unsafe { f(real, pdesc, riid, ppv) };
    if hr != S_OK && !pdesc.is_null() { let d = pdesc as *const u32; record("d3d12_xs", "CreateDescriptorHeap", &unsafe { format!("FAILED type={} num={} flags={:#x} node={:#x} hr={hr:#010x}", *d, *d.add(1), *d.add(2), *d.add(3)) }); }
    if hr == S_OK && !ppv.is_null() {
        // Keep the heap REAL (so it works when passed to SetDescriptorHeaps etc.);
        // patch the shared heap vtable once to fix the handle-getter ABI.
        crate::heap_wrap::ensure_patched(unsafe { *ppv });
        if (crate::rtcap::enabled() || crate::passcap::tables_on()) && !pdesc.is_null() {
            // Binding trace: remember shader-visible heaps (GPU handle -> CPU handle).
            let d = pdesc as *const u32;
            let (htype, numd, flags) = unsafe { (*d.add(0), *d.add(1), *d.add(2)) };
            let inc: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { core::mem::transmute(*real_vtbl(real).add(15)) };
            for t in 0..4 {
                crate::bindtrace::set_increment(t, unsafe { inc(real, t) });
            }
            if flags & 1 != 0 {
                let heap = unsafe { *ppv };
                let hv = unsafe { *(heap as *const *const usize) };
                let c: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(*hv.add(9)) };
                let g: unsafe extern "system" fn(*mut c_void) -> u64 = unsafe { core::mem::transmute(*hv.add(10)) };
                crate::bindtrace::note_heap(unsafe { c(heap) } as usize, unsafe { g(heap) }, numd, htype);
            }
        }
        // Log DESC { Type@0, NumDescriptors@4, Flags@8, NodeMask@0xC } + GPU base so
        // we can correlate the shader-visible online heap with the handles the game
        // later binds. Only log shader-visible (Flags bit0) or the first few.
        static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if !pdesc.is_null() {
            let d = pdesc as *const u32;
            let (htype, numd, flags) = unsafe { (*d.add(0), *d.add(1), *d.add(2)) };
            let shader_visible = flags & 1 != 0;
            if shader_visible || N.fetch_add(1, Ordering::Relaxed) < 10 {
                let heap = unsafe { *ppv };
                // GetGPUDescriptorHandleForHeapStart (slot 10, now our bridge -> u64)
                let gpu_base: u64 = if shader_visible {
                    let g: unsafe extern "system" fn(*mut c_void) -> u64 =
                        unsafe { core::mem::transmute(*(*(heap as *const *const usize)).add(10)) };
                    unsafe { g(heap) }
                } else { 0 };
                record("d3d12_xs", "CreateDescriptorHeap",
                    &format!("type={htype} num={numd} flags={flags:#x} shaderVis={shader_visible} heap={heap:p} gpuBase={gpu_base:#x}"));
            }
        }
    }
    hr
}

/// Intercept the Xbox extension `CreateCommandQueueX` (slot 48) and translate to
/// the real PC `CreateCommandQueue` (slot 8). The Xbox
/// `D3D12XBOX_COMMAND_QUEUE_DESC` extends the PC `D3D12_COMMAND_QUEUE_DESC`; the
/// first 16 bytes ({Type, Priority, Flags, NodeMask}) are layout-compatible.
unsafe extern "system" fn create_command_queue_x(
    this: *mut DeviceWrapper, pdescx: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let real = unsafe { (*this).real };
    let mut pc_desc = [0u8; 16];
    if !pdescx.is_null() {
        unsafe { core::ptr::copy_nonoverlapping(pdescx as *const u8, pc_desc.as_mut_ptr(), 16) };
    }
    crate::xone_queue::translate(&mut pc_desc);
    let f: unsafe extern "system" fn(*mut c_void, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(8)) }; // PC CreateCommandQueue
    let hr = unsafe { f(real, pc_desc.as_ptr() as *const c_void, riid, ppv) };
    let qtype = u32::from_le_bytes([pc_desc[0], pc_desc[1], pc_desc[2], pc_desc[3]]);
    let qflags = u32::from_le_bytes([pc_desc[8], pc_desc[9], pc_desc[10], pc_desc[11]]);
    if hr == 0 && !ppv.is_null() {
        // Give the queue an Xbox-sized vtable (PresentX at slot 30, other extensions no-op).
        crate::present::apply_queue_vtable(unsafe { *ppv });
        crate::map_x::set_queue(unsafe { *ppv });
        if qtype == 0 {
            crate::upload::register_direct_queue(unsafe { *ppv });
        }
    }
    record("d3d12_xs", "CreateCommandQueueX", &format!("type={qtype} flags={qflags} -> hr={hr:#010x}"));
    hr
}

unsafe extern "system" fn create_command_allocator_x(this: *mut DeviceWrapper, t: u32, riid: *const c_void, ppv: *mut *mut c_void) -> i32 { unsafe { crate::xone_queue::create_allocator((*this).real, t, riid, ppv) } }

/// Xbox slot 73 = command-list create. Xbox signature (read from the call site):
///   HRESULT (this, const Params* [rdx], ID3D12CommandAllocator* [r8],
///            ID3D12PipelineState* [r9], REFIID, void** ppList)
/// where Params is `{ u32 NodeMask@0; u32 _@4; D3D12_COMMAND_LIST_TYPE Type@8; u32 _@0xC }`.
/// PC `CreateCommandList` (slot 12) takes (NodeMask, Type, alloc, initState, riid, ppList).
unsafe extern "system" fn create_command_list_x(
    this: *mut DeviceWrapper, params: *const u32, alloc: *mut c_void,
    init_state: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    // Struct is { Type@0, _@4, NodeMask@8, _@0xC } (confirmed: type=0 DIRECT,
    // nodeMask=1). PC requires a non-zero node mask.
    let (list_type, mut node_mask) = if params.is_null() {
        (0u32, 1u32)
    } else {
        unsafe { (*params.add(0), *params.add(2)) }
    };
    if node_mask == 0 {
        node_mask = 1;
    }
    let list_type = crate::xone_queue::list_type(list_type);
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(12)) };
    let dma = !riid.is_null() && unsafe { *riid == crate::dma_list::IID_XBOX_DMA_COMMAND_LIST };
    let pc_riid = if dma { &crate::dma_list::IID_GRAPHICS_COMMAND_LIST as *const Guid } else { riid };
    let hr = unsafe { f(real, node_mask, list_type, alloc, init_state, pc_riid, ppv) };
    if hr == S_OK && !ppv.is_null() {
        // Hand the title a shim object (Xbox vtable layout + inert inline-buffer fields)
        // instead of the real PC command list; see cmdlist_wrap.
        unsafe {
            *ppv = if dma { crate::dma_list::wrap(*ppv) } else { crate::cmdlist_wrap::wrap(*ppv) };
        }
    }
    record("d3d12_xs", "CreateCommandList", &format!("nodeMask={node_mask} type={list_type} dma={dma} -> hr={hr:#010x}"));
    hr
}

/// Slot 15: GetDescriptorHandleIncrementSize(type) -> UINT. Log what the game
/// queries and what the real (NVIDIA) device reports, then forward.
unsafe extern "system" fn get_descriptor_increment(this: *mut DeviceWrapper, heap_type: u32) -> u32 {
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, u32) -> u32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(15)) };
    let sz = unsafe { f(real, crate::xone_heap::heap_type(heap_type)) };
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    if N.fetch_add(1, Ordering::Relaxed) < 8 {
        record("d3d12_xs", "GetDescriptorHandleIncrementSize", &format!("type={heap_type} -> {sz}"));
    }
    sz
}

/// ID3D12Device::GetResourceAllocationInfo (slot 25) returns D3D12_RESOURCE_ALLOCATION_INFO
/// {UINT64 SizeInBytes; UINT64 Alignment;} by value -> sret ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct AllocInfo { size: u64, align: u64 }

/// Intercept GetResourceAllocationInfo to see pResourceDescs (the crash is a null
/// deref inside the real method), dump the desc, and avoid crashing on a null/bad
/// desc. Rust's sret for a 16-byte return matches the PC ABI
/// (rcx=retval, rdx=this, r8=visibleMask, r9=numDescs, stack=pResourceDescs).
unsafe extern "system" fn get_resource_alloc_info(
    this: *mut DeviceWrapper, visible_mask: u32, num_descs: u32, pdescs: *const u32,
) -> AllocInfo {
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let _ = (this, visible_mask);
    if num_descs == 0 {
        return AllocInfo { size: 0, align: 1 };
    }
    if pdescs.is_null() {
        record("d3d12_xs", "GetResourceAllocationInfo", &format!("numDescs={num_descs} pDescs=NULL -> {{0,0}}, the release driver's failure value"));
        return AllocInfo { size: 0, align: 0 };
    }
    let fam = crate::device_console_fix::title_family();
    let raw = unsafe { core::slice::from_raw_parts(pdescs as *const u8, num_descs as usize * crate::device_console_fix::RES_DESC_SIZE) };
    if crate::device_console_fix::xg_available(fam) {
        let mut why: Vec<String> = Vec::new();
        let console = crate::device_console_fix::alloc_info_with(raw, num_descs as usize, |d| {
            let xg = crate::device_console_fix::xg_texture_alloc(d, fam);
            if let Err(hr) = xg {
                why.push(format!("{} {:#010x}", desc_str(d), hr as u32));
            }
            crate::device_console_fix::texture_part(xg, || console_desc_estimate(d))
        });
        static GAP: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if n < 8 || console == (0, 0) || (!why.is_empty() && GAP.fetch_add(1, Ordering::Relaxed) < 32) {
            let tail = if why.is_empty() {
                String::new()
            } else if console == (0, 0) {
                format!(": XG rejected [{}], the console driver's {{0,0}} failure value", why.join("; "))
            } else {
                format!(": emulated XG lacks [{}], 64 KiB tile over-estimate used for those desc(s)", why.join("; "))
            };
            record("d3d12_xs", "GetResourceAllocationInfo", &format!("{fam:?} XG layout for {num_descs} desc(s) -> size={:#x} align={:#x}{tail}", console.0, console.1));
        }
        return AllocInfo { size: console.0, align: console.1 };
    }
    if n < 20 {
        record("d3d12_xs", "GetResourceAllocationInfo", &format!("{fam:?} XGCreateTextureComputer unavailable or failed; using the 64 KiB tile over-estimate"));
    }
    // Copy and sanitize the desc(s): Xbox may leave SampleDesc.Count / DepthOrArraySize
    // / MipLevels as 0, which are invalid on PC D3D12 and crash GetResourceAllocationInfo.
    // D3D12_RESOURCE_DESC is 0x40 bytes: Dimension@0, Alignment@8, Width@16, Height@24,
    // DepthOrArraySize@28(u16), MipLevels@30(u16), Format@32, SampleDesc{Count@36,Quality@40},
    // Layout@44, Flags@48.
    let count = num_descs.max(1) as usize;
    // Zeroed, DESC1-sized (0x48) copies so a runtime reading past D3D12_RESOURCE_DESC
    // sees zeros, not adjacent heap garbage.
    let mut descs: Vec<u8> = vec![0u8; count * 0x48];
    for i in 0..count {
        let src = unsafe { core::slice::from_raw_parts(pdescs.cast::<u8>().add(i * 0x38), 0x38) };
        descs[i * 0x48..i * 0x48 + 0x38].copy_from_slice(src);
        let b = &mut descs[i * 0x48..];
        let u16at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let u32at = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let u64at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o+8].try_into().unwrap());
        let align = u64at(b, 8);
        if align != 0 && align != 0x1000 && align != 0x10000 && align != 0x400000 {
            b[8..16].copy_from_slice(&0u64.to_le_bytes()); // bad alignment -> 0 (let D3D12 pick)
        }
        if u16at(b, 28) == 0 { b[28] = 1; } // DepthOrArraySize 0 -> 1
        if u16at(b, 30) == 0 { b[30] = 1; } // MipLevels 0 -> 1
        if u32at(b, 36) == 0 { b[36] = 1; } // SampleDesc.Count 0 -> 1
        if n < 8 && i == 0 {
            record("d3d12_xs", "GetResourceAllocationInfo",
                &format!("dim={} align={:#x} w={} h={} depthArr={} mips={} fmt={} sampleCount={} layout={} flags={:#x}",
                    u32at(b, 0), align, u64at(b, 16), u32at(b, 24),
                    u16at(b, 28), u16at(b, 30), u32at(b, 32), u32at(b, 36), u32at(b, 44), u32at(b, 48)));
        }
    }
    // The real D3D12Core GetResourceAllocationInfo faults deep in its texture-tiling
    // layout sub-function for the Xbox-origin texture descriptors (verified: valid
    // device + fully-sanitized valid desc still crashes at vtbl+0x25829). Compute the
    // info ourselves from the D3D12 64 KiB standard-swizzle tile model. We OVER-estimate
    // (round each mip up to whole tiles) so the pool allocator never under-reserves.
    let total = (0..count).map(|i| {
        let b = &descs[i * 0x48..];
        let u16at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let u32at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let u64at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let dim = u32at(0);
        let width = u64at(16).max(1);
        let height = (u32at(24) as u64).max(1);
        let arr = (u16at(28) as u64).max(1);
        let mips = (u16at(30) as u64).max(1);
        let samples = (u32at(36) as u64).max(1);
        alloc_size_for(dim, width, height, arr, mips, u32at(32), samples)
    }).max().unwrap_or(0x10000);
    let align = if descs.len() >= 16 && u64::from_le_bytes(descs[8..16].try_into().unwrap()) == 0x1000 && total <= 0x10000 {
        0x1000
    } else {
        0x10000
    };
    let info = AllocInfo { size: total, align };
    if n < 8 {
        record("d3d12_xs", "GetResourceAllocationInfo", &format!("COMPUTED size={:#x} align={:#x}", info.size, info.align));
    }
    let _ = (this, visible_mask);
    info
}

pub(crate) fn title_layout_size_with(desc: &[u8], xg: Option<&dyn Fn(&[u8]) -> Result<(u64, u64), i32>>) -> u64 {
    let d = &desc[..crate::device_console_fix::RES_DESC_SIZE];
    let s = match xg {
        Some(xg) => crate::device_console_fix::alloc_info_with(d, 1, |d| crate::device_console_fix::texture_part(xg(d), || console_desc_estimate(d))).0,
        None => 0,
    };
    if s != 0 { s } else { crate::gpu_va::xbox_texture_size(desc) }
}

pub(crate) fn title_layout_size(desc: &[u8]) -> u64 {
    let fam = crate::device_console_fix::title_family();
    if crate::device_console_fix::xg_available(fam) {
        title_layout_size_with(desc, Some(&|d: &[u8]| crate::device_console_fix::xg_texture_alloc(d, fam)))
    } else {
        title_layout_size_with(desc, None)
    }
}

fn console_desc_estimate(d: &[u8]) -> (u64, u64) {
    let u16at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as u64;
    let u32at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let width = u64::from_le_bytes(d[0x10..0x18].try_into().unwrap()).max(1);
    let size = alloc_size_for(u32at(0), width, (u32at(0x18) as u64).max(1), u16at(0x1c).max(1), u16at(0x1e).max(1), u32at(0x20), (u32at(0x24) as u64).max(1));
    (size, 0x10000)
}

/// Bytes per pixel for the common DXGI formats (block-compressed reported as their
/// effective per-pixel cost so the tile estimate stays an over-estimate).
fn format_bpp(fmt: u32) -> u64 {
    match fmt {
        1..=4 => 16,                   // R32G32B32A32
        5..=8 => 12,                   // R32G32B32
        9..=14 => 8,                   // R16G16B16A16
        15..=22 => 8,                  // R32G32 / depth-stencil combos
        70..=72 => 1,                  // BC1 (0.5 bpp, rounded up)
        73..=78 => 1,                  // BC2/BC3
        79..=84 => 1,                  // BC4/BC5
        94..=99 => 1,                  // BC6H/BC7
        54..=59 => 2,                  // R16
        60..=65 => 1,                  // R8
        _ => 4,                        // R8G8B8A8 / B8G8R8A8 / R10G10B10A2 / etc.
    }
}

/// D3D12 64 KiB standard-swizzle tile size, over-estimated (whole tiles per mip).
pub(crate) fn alloc_size_for(dim: u32, width: u64, height: u64, arr: u64, mips: u64, fmt: u32, samples: u64) -> u64 {
    const TILE: u64 = 0x10000;
    if dim <= 1 {
        // Buffer: 64 KiB-aligned length.
        return ((width + TILE - 1) & !(TILE - 1)).max(TILE);
    }
    let bpp = format_bpp(fmt);
    // 64 KiB tile pixel extents by bpp (D3D12 standard-swizzle 2D tile shapes).
    let (tw, th) = match bpp {
        16 => (64u64, 64u64),
        12 | 8 => (128, 64),
        4 => (128, 128),
        2 => (256, 128),
        _ => (256, 256), // 1 bpp and BC (approx)
    };
    let mut tiles = 0u64;
    let (mut w, mut h) = (width, height);
    for _ in 0..mips {
        let tx = (w + tw - 1) / tw;
        let ty = (h + th - 1) / th;
        tiles += (tx * ty).max(1);
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    (tiles * arr * samples * TILE).max(TILE)
}

/// Intercept CreateConstantBufferView (slot 17). Xbox allows a CBV SizeInBytes that
/// is not a multiple of 256; PC D3D12 rejects it and REMOVES THE DEVICE
/// (DXGI_ERROR_INVALID_CALL). Round SizeInBytes up to 256.
/// Args: rcx=this, rdx=pDesc, r8=DestDescriptor (CPU handle, by value).
/// D3D12_CONSTANT_BUFFER_VIEW_DESC { u64 BufferLocation; u32 SizeInBytes; }.
unsafe extern "system" fn create_cbv(this: *mut DeviceWrapper, pdesc: *const u8, dest: usize) {
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, usize) =
        unsafe { core::mem::transmute(*real_vtbl(real).add(17)) };
    if pdesc.is_null() {
        unsafe { f(real, pdesc, dest) };
        return;
    }
    let mut desc = [0u8; 16];
    unsafe { core::ptr::copy_nonoverlapping(pdesc, desc.as_mut_ptr(), 12) };
    let size = u32::from_le_bytes([desc[8], desc[9], desc[10], desc[11]]);
    let loc = u64::from_le_bytes(desc[0..8].try_into().unwrap());
    let aligned = (size + 255) & !255;
    if loc & 255 != 0 {
        static MIS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if MIS.fetch_add(1, Ordering::Relaxed) < 6 {
            record("d3d12_xs", "CreateConstantBufferView", &format!("BufferLocation {loc:#x} is not 256-aligned (size {size}); PC D3D12 requires D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT"));
        }
    }
    if aligned != size {
        desc[8..12].copy_from_slice(&aligned.to_le_bytes());
        static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if N.fetch_add(1, Ordering::Relaxed) < 6 {
            record("d3d12_xs", "CreateConstantBufferView", &format!("size {size} -> {aligned} (256-aligned)"));
        }
    }
    unsafe { f(real, desc.as_ptr(), dest) };
    unsafe { crate::ecl_probe::after_device_call(real, "CreateConstantBufferView", || format!("BufferLocation={loc:#x} size {size} -> {aligned} dest={dest:#x}")) };
}

/// PC IID_ID3D12Resource = {696442be-a72e-4059-bc79-5b5c98040fad}
static IID_ID3D12RESOURCE: Guid = Guid {
    d1: 0x6964_42be, d2: 0xa72e, d3: 0x4059, d4: [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad],
};

/// Intercept CreateCommittedResource (slot 27). Logs heap/desc/riid and, like the
/// device/file IIDs, substitutes the PC ID3D12Resource IID so a mismatched Xbox
/// resource IID doesn't make the real call return a null resource.
/// Args (system ABI): rcx=this, rdx=pHeapProps, r8=HeapFlags, r9=pDesc, then
/// stack: InitialState, pOptimizedClearValue, riid, ppvResource.
pub(crate) unsafe extern "system" fn create_committed_resource(
    this: *mut DeviceWrapper, pheap: *const u32, heap_flags: u32, pdesc: *const u32,
    init_state: u32, pclear: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let _ev = crate::frame_ev::begin(crate::frame_ev::COMMITTED);
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u32, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(27)) };
    let use_riid: *const Guid = &IID_ID3D12RESOURCE;
    // Mask D3D12_RESOURCE_DESC::Flags (offset 0x30) to PC-valid bits: Xbox sets
    // console-only flags (e.g. 0x300000) that PC D3D12 rejects, removing the device.
    let mut desc_copy = [0u8; 0x40];
    let desc_ptr = if pdesc.is_null() {
        pdesc
    } else {
        unsafe { core::ptr::copy_nonoverlapping(pdesc as *const u8, desc_copy.as_mut_ptr(), 0x40) };
        let flags = u32::from_le_bytes(desc_copy[0x30..0x34].try_into().unwrap());
        const PC_RESOURCE_FLAGS: u32 = 0xFF; // RT|DSV|UAV|DENY_SRV|CROSS_ADAPTER|SIMUL|VIDEO*
        desc_copy[0x30..0x34].copy_from_slice(&crate::xone_tex::committed_flags(flags & PC_RESOURCE_FLAGS).to_le_bytes());
        desc_copy.as_ptr() as *const u32
    };
    let init_state = if pheap.is_null() { init_state } else { crate::xone_tex::committed_state(unsafe { *pheap }, init_state) };
    let init_state = if pdesc.is_null() { init_state } else { crate::xone_barrier::initial_state(unsafe { *pdesc }, init_state) };
    let mut heap_words = [0u32; 5];
    let pheap = if pheap.is_null() {
        pheap
    } else {
        unsafe { core::ptr::copy_nonoverlapping(pheap, heap_words.as_mut_ptr(), 5) };
        heap_words = crate::device_console_fix::pc_heap_words(heap_words);
        heap_words.as_ptr()
    };
    let depth_uav = if pdesc.is_null() || !crate::depth_uav::enabled() { crate::depth_uav::Plan::None } else { crate::depth_uav::plan(&desc_copy[..0x38]) };
    let title_state = init_state;
    let init_state = if depth_uav == crate::depth_uav::Plan::None {
        init_state
    } else {
        let d = crate::depth_uav::depth_desc(desc_copy[..0x38].try_into().unwrap());
        desc_copy[..0x38].copy_from_slice(&d);
        crate::depth_uav::d_real(init_state)
    };
    let pooled = if depth_uav == crate::depth_uav::Plan::None { unsafe { crate::res_pool::try_create(real, pheap, heap_flags, desc_ptr, init_state, pclear, ppv) } } else { None };
    let hr = match pooled {
        Some(hr) => hr,
        None => unsafe { f(real, pheap, heap_flags, desc_ptr, init_state, pclear, use_riid, ppv) },
    };
    if hr >= 0 && !ppv.is_null() {
        crate::map_x::ensure_patched(unsafe { *ppv });
        let res = unsafe { *ppv };
        crate::depth_uav::forget(res as usize);
        match &depth_uav {
            crate::depth_uav::Plan::Pair(fm) => {
                let d: [u8; 0x38] = desc_copy[..0x38].try_into().unwrap();
                if let Err(e) = unsafe { crate::depth_uav::register(real, res, &d, &heap_words, title_state, *fm) } {
                    crate::depth_uav::refuse(res as usize, &e);
                }
            }
            crate::depth_uav::Plan::Refuse(why) => crate::depth_uav::refuse(res as usize, why),
            crate::depth_uav::Plan::None => {}
        }
    }
    if hr >= 0 && !ppv.is_null() && !pdesc.is_null() {
        let heap_type = if pheap.is_null() { 0 } else { unsafe { *pheap } };
        let xdesc: [u8; 0x38] = unsafe { core::ptr::read_unaligned(pdesc as *const [u8; 0x38]) };
        let backing = if heap_type == 1 { unsafe { crate::gpu_va::back_committed_texture(&xdesc) } } else { None };
        unsafe { crate::gpu_va::note_resource(real, *ppv, &desc_copy[..0x38], init_state, backing) };
        if crate::va_census::on() {
            crate::va_census::note_how(unsafe { *ppv } as usize, match backing { Some(v) => format!("CreateCommittedResource with title backing {v:#x}"), None => "CreateCommittedResource".to_string() });
        }
        let res = unsafe { *ppv };
        match backing {
            Some(va) => unsafe { crate::gpu_va::adopt_backing(res, va, xdesc, init_state) },
            None if heap_type == 1 && crate::gpu_va::is_texture_desc(&xdesc) => crate::gpu_va::note_committed_texture(res, xdesc),
            None => {}
        }
        unsafe { crate::rtcap::register(*ppv, &desc_copy[..0x38], init_state) };
        crate::res3d::note_create(unsafe { *ppv }, &desc_copy[..0x38], &format!("committed heapType={heap_type} heapFlags={heap_flags:#x} state={init_state:#x} backing={backing:x?}"));
        if (crate::rtcap::enabled() || crate::passcap::tables_on()) && desc_copy[0] == 1 && !pheap.is_null() {
            let width = u64::from_le_bytes(desc_copy[0x10..0x18].try_into().unwrap());
            unsafe { crate::bindtrace::note_buffer(*ppv, width, *pheap) };
        }
        if pooled.is_none() {
            unsafe { crate::title_gpu::committed(*ppv, &xdesc) };
        }
    }
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    if hr < 0 && !pdesc.is_null() {
        let orig = unsafe { core::slice::from_raw_parts(pdesc as *const u8, 0x38) };
        let hp = if pheap.is_null() { String::new() } else { unsafe { format!("heap{{type={} cpu={} pool={}}}", *pheap, *pheap.add(1), *pheap.add(2)) } };
        record("d3d12_xs", "CreateCommittedResource", &format!(
            "FAILED hr={hr:#010x} {hp} heapFlags={heap_flags:#x} state={init_state:#x} clear={pclear:p} {}", desc_str(orig)));
    }
    if N.fetch_add(1, Ordering::Relaxed) < 10 {
        let htype = if pheap.is_null() { 0xffff_ffff } else { unsafe { *pheap } };
        let (dim, w) = if pdesc.is_null() { (0xffff_ffff, 0) } else { unsafe { (*pdesc, *(pdesc.add(2) as *const u64)) } };
        let orig_riid = if riid.is_null() { String::from("null") } else { guid_str(unsafe { &*riid }) };
        let out = if ppv.is_null() { core::ptr::null_mut() } else { unsafe { *ppv } };
        record("d3d12_xs", "CreateCommittedResource",
            &format!("heapType={htype} dim={dim} width={w} initState={init_state:#x} riid={orig_riid} -> hr={hr:#010x} res={out:p}"));
    }
    hr
}

// Forwarding thunk: this(rcx) -> this->real; jmp real->vtbl[slot].
macro_rules! fwd {
    ($name:ident, $slot:expr) => {
        #[unsafe(naked)]
        unsafe extern "system" fn $name() {
            core::arch::naked_asm!(
                "mov rcx, [rcx+8]",
                "mov rax, [rcx]",
                "jmp qword ptr [rax + {o}]",
                o = const ($slot * 8usize),
            );
        }
    };
}

// sret forwarding thunk: for methods returning a struct >8 bytes BY VALUE, the
// x64 ABI puts the hidden return pointer in rcx and `this` in rdx. So we must
// redirect rdx (this -> this->real), leaving rcx (the return buffer) untouched.
// ID3D12Device::GetResourceAllocationInfo (25, returns 16B) and
// GetCustomHeapProperties (26, returns 20B) are the two such device methods.
macro_rules! fwd_sret {
    ($name:ident, $slot:expr) => {
        #[unsafe(naked)]
        unsafe extern "system" fn $name() {
            core::arch::naked_asm!(
                "mov rdx, [rdx+8]",
                "mov rax, [rdx]",
                "jmp qword ptr [rax + {o}]",
                o = const ($slot * 8usize),
            );
        }
    };
}

// ID3D12Device methods 3..=43 (0,1,2 are IUnknown, handled above).
fwd!(f3, 3); fwd!(f4, 4); fwd!(f5, 5); fwd!(f6, 6); fwd!(f7, 7); fwd!(f8, 8);
fwd!(f9, 9); fwd!(f10, 10); fwd!(f11, 11); fwd!(f12, 12);
/* 13 = check_feature_support, 14 = create_descriptor_heap */
fwd!(f15, 15); fwd!(f16, 16); fwd!(f17, 17); fwd!(f18, 18); fwd!(f19, 19); fwd!(f20, 20);
fwd!(f21, 21); fwd!(f22, 22); fwd!(f23, 23); fwd!(f24, 24);
fwd_sret!(f25, 25); fwd_sret!(f26, 26); // return structs by value: `this` is in rdx
fwd!(f27, 27); fwd!(f28, 28); fwd!(f29, 29); fwd!(f30, 30); fwd!(f31, 31); fwd!(f32, 32);
fwd!(f33, 33); fwd!(f34, 34); fwd!(f35, 35); fwd!(f36, 36); fwd!(f37, 37); fwd!(f38, 38);
fwd!(f39, 39); fwd!(f40, 40); fwd!(f41, 41); fwd!(f42, 42); fwd!(f43, 43);
// Slots 44+ are Xbox-specific device extensions (NOT PC ID3D12Device1-8), so they
// are NOT forwarded by slot number. Unrecognized ones get this fail-loud stub
// (returns E_NOTIMPL) so the game fails that call cleanly and names it in its log,
// instead of reading past the vtable and crashing. Recognized Xbox methods are
// intercepted individually (e.g. slot 48 = CreateCommandQueueX).
static DEV_STUB_HITS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

unsafe extern "system" fn dev_stub() -> i32 {
    DEV_STUB_HITS.fetch_add(1, Ordering::Relaxed);
    0x8000_4001u32 as i32
}

/// Xbox graphics-PSO create (device slot 60).
///
/// The desc is layout-compatible with PC `D3D12_GRAPHICS_PIPELINE_STATE_DESC`, but the
/// shaders are Xbox-native RDNA2 (XDAT/FSSM, DXIL stripped). Translate VS/PS to PC
/// bytecode (see `shader_xlat`), patch a copy of the desc, and create a real PC PSO.
/// If any stage cannot be translated the PSO honestly fails (no placeholder shaders).
/// Signature (from the RHI call site at EXE+0xd5564c): `(this, pDesc, numExt, pExtDesc, riid, ppv)`
unsafe fn bisect_gfx_pso(real: *mut c_void, desc: &[u8], riid: *const Guid) -> String {
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(10)) };
    let put = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let variants: Vec<(&str, Box<dyn Fn(&mut Vec<u8>)>)> = vec![
        ("no_ps", Box::new(|d: &mut Vec<u8>| d[0x18..0x28].fill(0))),
        ("no_depth", Box::new(move |d: &mut Vec<u8>| { put(d, 0x1f0, 0); put(d, 0x1f4, 0); put(d, 0x1f8, 1); put(d, 0x1fc, 0); put(d, 0x264, 0); })),
        ("no_rt", Box::new(move |d: &mut Vec<u8>| { put(d, 0x240, 0); d[0x244..0x264].fill(0); })),
        ("no_input_layout", Box::new(|d: &mut Vec<u8>| { d[0x228..0x238].fill(0); })),
        ("default_raster", Box::new(move |d: &mut Vec<u8>| { put(d, 0x1c4, 3); put(d, 0x1c8, 3); for k in 2..11 { put(d, 0x1c4 + 4 * k, 0); } put(d, 0x1dc, 1); })),
        ("default_blend", Box::new(|d: &mut Vec<u8>| { d[0x78..0x1c0].fill(0); for i in 0..8 { let o = 0x80 + i * 40; d[o + 8..o + 12].copy_from_slice(&2u32.to_le_bytes()); d[o + 12..o + 16].copy_from_slice(&1u32.to_le_bytes()); d[o + 16..o + 20].copy_from_slice(&1u32.to_le_bytes()); d[o + 20..o + 24].copy_from_slice(&2u32.to_le_bytes()); d[o + 24..o + 28].copy_from_slice(&1u32.to_le_bytes()); d[o + 28..o + 32].copy_from_slice(&1u32.to_le_bytes()); d[o + 32..o + 36].copy_from_slice(&4u32.to_le_bytes()); d[o + 36] = 15; } })),
        ("no_flags", Box::new(move |d: &mut Vec<u8>| put(d, 0x288, 0))),
        ("no_stream_out", Box::new(|d: &mut Vec<u8>| d[0x58..0x78].fill(0))),
        ("rtv_fill", Box::new(move |d: &mut Vec<u8>| { let rt0 = u32::from_le_bytes(d[0x244..0x248].try_into().unwrap()); let num = u32::from_le_bytes(d[0x240..0x244].try_into().unwrap()).min(8) as usize; for i in 0..num { let o = 0x244 + 4 * i; if d[o..o + 4] == [0u8; 4] { put(d, o, rt0); } } })),
        ("single_sample", Box::new(move |d: &mut Vec<u8>| { put(d, 0x268, 1); put(d, 0x26c, 0); })),
    ];
    let mut out = String::new();
    for (name, m) in variants {
        let mut d = desc.to_vec();
        m(&mut d);
        let mut p: *mut c_void = core::ptr::null_mut();
        let hr = unsafe { f(real, d.as_ptr(), riid, &mut p) };
        if hr >= 0 && !p.is_null() {
            let rel: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*(*(p as *const *const usize)).add(2)) };
            unsafe { rel(p) };
        }
        out.push_str(&format!(" {name}={}", if hr >= 0 { "OK" } else { "fail" }));
    }
    out
}

unsafe fn dump_failed_gfx_pso(n: usize, desc: &[u8]) {
    let dir = std::env::temp_dir().join("tombox").join("psofail");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join(format!("{n}_desc.bin")), desc);
    let rd = |o: usize| u64::from_le_bytes(desc[o..o + 8].try_into().unwrap());
    if let Some(rs) = crate::replay::root_blob(rd(0) as usize) {
        let _ = std::fs::write(dir.join(format!("{n}_rootsig.bin")), rs);
    }
    for (k, off) in [0x08usize, 0x18, 0x28, 0x38, 0x48].iter().enumerate() {
        let (p, len) = (rd(*off) as *const u8, rd(off + 8) as usize);
        if !p.is_null() && len > 0 && len < 64 << 20 {
            let _ = std::fs::write(dir.join(format!("{n}_stage{k}.bin")), unsafe { core::slice::from_raw_parts(p, len) });
        }
    }
    let ptr = rd(0x228) as *const u8;
    let cnt = (u32::from_le_bytes(desc[0x230..0x234].try_into().unwrap()) as usize).min(32);
    let mut txt = String::new();
    for i in 0..if ptr.is_null() { 0 } else { cnt } {
        let e = unsafe { core::slice::from_raw_parts(ptr.add(i * 32), 32) };
        let np = u64::from_le_bytes(e[0..8].try_into().unwrap()) as *const u8;
        let mut name = String::new();
        let mut k = 0;
        while !np.is_null() && k < 64 && unsafe { *np.add(k) } != 0 { name.push(unsafe { *np.add(k) } as char); k += 1; }
        let f = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
        txt.push_str(&format!("{name} {} {} {} {} {} {}\n", f(8), f(12), f(16), f(20), f(24), f(28)));
    }
    let _ = std::fs::write(dir.join(format!("{n}_input.txt")), txt);
}

const DXGI_XBOX_R10G10B10_SNORM_A2_UNORM: u32 = 189;
const DXGI_R10G10B10A2_UNORM: u32 = 24;

pub(crate) fn pc_input_format(fmt: u32) -> Option<u32> {
    match fmt {
        DXGI_XBOX_R10G10B10_SNORM_A2_UNORM => Some(DXGI_R10G10B10A2_UNORM),
        _ => None,
    }
}

unsafe fn snorm10_semantics(desc: &[u8]) -> Vec<String> {
    let ptr = u64::from_le_bytes(desc[0x228..0x230].try_into().unwrap()) as *const u8;
    let n = u32::from_le_bytes(desc[0x230..0x234].try_into().unwrap()) as usize;
    if ptr.is_null() || n == 0 || n > 64 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let e = unsafe { core::slice::from_raw_parts(ptr.add(i * 32), 32) };
        if u32::from_le_bytes(e[12..16].try_into().unwrap()) != DXGI_XBOX_R10G10B10_SNORM_A2_UNORM {
            continue;
        }
        let np = u64::from_le_bytes(e[0..8].try_into().unwrap()) as *const u8;
        if np.is_null() {
            continue;
        }
        let mut name = String::new();
        let mut k = 0;
        while k < 64 && unsafe { *np.add(k) } != 0 {
            name.push(unsafe { *np.add(k) } as char);
            k += 1;
        }
        out.push(format!("{name}{}", u32::from_le_bytes(e[8..12].try_into().unwrap())));
    }
    out
}

unsafe fn remap_xbox_input_formats(desc: &mut [u8]) -> Option<Vec<u8>> {
    let ptr = u64::from_le_bytes(desc[0x228..0x230].try_into().unwrap()) as *const u8;
    let n = u32::from_le_bytes(desc[0x230..0x234].try_into().unwrap()) as usize;
    if ptr.is_null() || n == 0 || n > 64 {
        return None;
    }
    let src = unsafe { core::slice::from_raw_parts(ptr, n * 32) };
    if !(0..n).any(|i| pc_input_format(u32::from_le_bytes(src[i * 32 + 12..i * 32 + 16].try_into().unwrap())).is_some()) {
        return None;
    }
    let mut v = src.to_vec();
    for i in 0..n {
        let o = i * 32 + 12;
        let f = u32::from_le_bytes(v[o..o + 4].try_into().unwrap());
        if let Some(pc) = pc_input_format(f) {
            static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            if N.fetch_add(1, Ordering::Relaxed) < 4 {
                record("d3d12_xs", "CreateGraphicsPSO_X", &format!("input element {i}: Xbox-only DXGI format {f} (R10G10B10_SNORM_A2_UNORM) fetched as {pc} (R10G10B10A2_UNORM): the VS decodes SNORM from the UNORM bits (vs_snorm)"));
            }
            v[o..o + 4].copy_from_slice(&pc.to_le_bytes());
        }
    }
    desc[0x228..0x230].copy_from_slice(&(v.as_ptr() as u64).to_le_bytes());
    Some(v)
}

unsafe extern "system" fn create_gfx_pso_x(
    this: *mut DeviceWrapper, pdesc: *const u8, num_ext: u32, pext: *const u8,
    riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    unsafe { crate::pso_xrec::note(crate::pso_xrec::GFX, pdesc, num_ext) };
    let mut ev = crate::frame_ev::begin(crate::frame_ev::PSO_SYNC);
    if let Some(hr) = unsafe { crate::pso_async::create_gfx(this, (*this).real, pdesc, num_ext, pext, riid, ppv, create_gfx_pso_sync) } {
        if let Some(s) = ev.as_mut() {
            s.set_kind(if crate::frame_ev::take_async_mark() { crate::frame_ev::PSO_ASYNC } else { crate::frame_ev::PSO_INLINE });
        }
        return crate::cbv_copy::after_gfx_pso(hr, pdesc, ppv);
    }
    let t0 = std::time::Instant::now();
    let _ = crate::pso_lib::take_lock_wait_us();
    let hr = unsafe { create_gfx_pso_sync(this, pdesc, num_ext, pext, riid, ppv) };
    note_sync_pso("graphics (CreateGraphicsPipelineState_X)", unsafe { crate::pso_async::sync_reason(pdesc, riid, false) }, t0, hr);
    crate::cbv_copy::after_gfx_pso(hr, pdesc, ppv)
}

pub(crate) unsafe fn replay_create(wrapper: *mut c_void, gfx: bool, desc: *const u8, riid: *const Guid, out: *mut usize) -> i32 {
    let this = wrapper as *mut DeviceWrapper;
    let ppv = out as *mut *mut c_void;
    if gfx {
        unsafe { create_gfx_pso_sync(this, desc, 0, core::ptr::null(), riid, ppv) }
    } else {
        unsafe { create_compute_pso_sync(this, desc, 0, core::ptr::null(), riid, ppv) }
    }
}

pub(crate) fn forget_pso(pso: usize) {
    crate::frame_ev::lock(pso_registry()).remove(&pso);
}

fn note_sync_pso(kind: &str, why: &str, t0: std::time::Instant, hr: i32) {
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    let lock_ms = crate::pso_lib::take_lock_wait_us() as f64 / 1000.0;
    if ms < 50.0 {
        return;
    }
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n < 40 || n % 100 == 0 {
        let name = std::thread::current().name().map(str::to_owned).unwrap_or_else(|| format!("{:?}", std::thread::current().id()));
        record("d3d12_xs", "pso_sync", &format!("#{} {kind} took {ms:.1} ms on {name} (sync because: {why}; of that {lock_ms:.1} ms waiting for the pipeline library lock) hr={hr:#010x}", n + 1));
    }
}

unsafe extern "system" fn create_gfx_pso_sync(
    this: *mut DeviceWrapper, pdesc: *const u8, _num_ext: u32, _pext: *const u8,
    riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    const E_FAIL: i32 = 0x8000_4005u32 as i32;
    const DESC_SIZE: usize = 0x290;
    static OK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static FAIL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static XFAIL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let real = unsafe { (*this).real };
    if pdesc.is_null() {
        return E_FAIL;
    }
    let mut desc = unsafe { core::slice::from_raw_parts(pdesc, DESC_SIZE).to_vec() };
    if std::env::var_os("TOMBOX_DBG_CULL_NONE").is_some() {
        desc[0x1c8..0x1cc].copy_from_slice(&1u32.to_le_bytes());
    }
    if std::env::var_os("TOMBOX_DBG_NO_BLEND").is_some() {
        desc[0x80..0x84].copy_from_slice(&0u32.to_le_bytes());
    }
    if crate::rtcap::enabled() {
        let ptr = u64::from_le_bytes(desc[0x228..0x230].try_into().unwrap()) as *const u8;
        let n = u32::from_le_bytes(desc[0x230..0x234].try_into().unwrap()) as usize;
        if !ptr.is_null() && n > 0 && n <= 16 {
            let src = unsafe { core::slice::from_raw_parts(ptr, n * 32) };
            let els: Vec<String> = (0..n).map(|i| {
                let e = &src[i * 32..i * 32 + 32];
                let u = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
                let np = u64::from_le_bytes(e[0..8].try_into().unwrap()) as *const u8;
                let mut name = String::new();
                let mut k = 0;
                while !np.is_null() && k < 64 && unsafe { *np.add(k) } != 0 {
                    name.push(unsafe { *np.add(k) } as char);
                    k += 1;
                }
                format!("{name}{} fmt={} slot={} off={} class={} step={}", u(8), u(12), u(16), u(20), u(24), u(28))
            }).collect();
            let vs = u64::from_le_bytes(desc[0x08..0x10].try_into().unwrap()) as *const u8;
            let vn = u64::from_le_bytes(desc[0x10..0x18].try_into().unwrap()) as usize;
            let h = if vs.is_null() || vn == 0 { 0 } else { crate::shader_xlat::fnv1a(unsafe { core::slice::from_raw_parts(vs, vn) }) };
            record("d3d12_xs", "psolayout", &format!("vs={h:016x} elements: {}", els.join(" | ")));
        }
    }
    let rd = |d: &[u8], o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
    // Keep translated bytecode alive until the PSO is created.
    let mut keep: Vec<std::sync::Arc<Vec<u8>>> = Vec::new();
    let mut failure: Option<String> = None;
    let stage_rtfmt = |stage: &str, desc: &[u8]| if stage == "PS" {
        let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
        let fmts: [u32; 8] = core::array::from_fn(|i| u32at(0x244 + 4 * i));
        crate::centroid_bind::tag_rtfmt(rd(desc, 0), crate::shader_xlat::export_formats(u32at(0x240), &fmts))
    } else {
        [0; 8]
    };
    let ps_opts = if _pext.is_null() || _num_ext == 0 { None } else { crate::pso_ext::ps::options_in_entries(unsafe { core::slice::from_raw_parts(_pext, _num_ext as usize * crate::pso_ext::EXT_STRIDE) }, _num_ext as usize) };
    let mut ps_origin: Option<crate::pso_ext::ps::PsOrigin> = None;
    let mut edits: Vec<&'static str> = Vec::new();
    let stages = [("VS", 0x08usize), ("PS", 0x18), ("DS", 0x28), ("HS", 0x38), ("GS", 0x48)];
    let present = |o: usize| rd(&desc, o) != 0 && rd(&desc, o + 8) != 0;
    let feeder = if present(0x48) { 0x48 } else if present(0x28) { 0x28 } else { 0x08 };
    let ps_len = rd(&desc, 0x20) as usize;
    let ps_link = if present(0x18) && (32..=0x40_0000).contains(&ps_len) {
        let pc_centroid = stage_rtfmt("PS", &desc)[0] & crate::centroid_bind::RTFMT_PC_CENTROID != 0;
        crate::shader_xlat::ps_link(unsafe { core::slice::from_raw_parts(rd(&desc, 0x18) as *const u8, ps_len) }, pc_centroid)
    } else {
        crate::shader_xlat::no_link()
    };
    let link = |off: usize| if off == feeder { ps_link.clone() } else { crate::shader_xlat::no_link() };
    for &(stage, off) in stages.iter().filter(|&&(_, o)| present(o)).skip(1) {
        unsafe { crate::shader_xlat::prefetch(rd(&desc, off) as *const u8, rd(&desc, off + 8) as usize, stage_rtfmt(stage, &desc), &link(off)) };
    }
    for (stage, off) in stages {
        let (p, n) = (rd(&desc, off) as *const u8, rd(&desc, off + 8) as usize);
        if p.is_null() || n == 0 {
            continue;
        }
        let rtfmt = stage_rtfmt(stage, &desc);
        match crate::frame_ev::time(crate::frame_ev::XLAT, || unsafe { crate::shader_xlat::translate_for_targets(p, n, rtfmt, &link(off)) }) {
            Ok(code) => {
                let code = if stage == "PS" {
                    ps_origin = crate::pso_ext::ps::origin_from_blob(p, n, rtfmt, ps_opts.unwrap_or_default());
                    match (ps_opts, &ps_origin) {
                        (Some(_), Some(o)) => match crate::pso_ext::ps::variant(o, crate::pso_ext::ps::depth_stencil_writes(&desc)) {
                            Some(v) => {
                                edits.push("ps:pso_ext_variant");
                                v
                            }
                            None => code,
                        },
                        _ => code,
                    }
                } else if stage == "VS" {
                    let sems = unsafe { snorm10_semantics(&desc) };
                    match unsafe { crate::vs_snorm::variant(p, n, &sems, &link(off)) } {
                        Some(v) => {
                            edits.push("vs:vs_snorm");
                            v
                        }
                        None => code,
                    }
                } else {
                    code
                };
                desc[off..off + 8].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
                desc[off + 8..off + 16].copy_from_slice(&(code.len() as u64).to_le_bytes());
                keep.push(code);
            }
            Err(e) => {
                failure = Some(format!("{stage}: {}", e.lines().next().unwrap_or("")));
                break;
            }
        }
    }
    let hr = if let Some(why) = failure {
        let n = XFAIL.fetch_add(1, Ordering::Relaxed);
        if n < 6 {
            record("d3d12_xs", "CreateGraphicsPSO_X", &format!("untranslatable shader -> E_FAIL ({why})"));
        }
        E_FAIL
    } else {
        let _il_keep = unsafe { remap_xbox_input_formats(&mut desc) };
        static DUMP1: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if DUMP1.fetch_add(1, Ordering::Relaxed) < 2 {
            let ptr = rd(&desc, 0x228) as *const u8;
            let n = (u32::from_le_bytes(desc[0x230..0x234].try_into().unwrap()) as usize).min(8);
            let mut s = format!("inputLayout ptr={ptr:p} n={n} rootSig={:#x} riid={:p}", rd(&desc, 0), riid);
            if !ptr.is_null() {
                let raw = unsafe { core::slice::from_raw_parts(ptr, n * 40) };
                for (i, c) in raw.chunks(8).enumerate() {
                    if i % 5 == 0 { s.push_str("\n   "); }
                    s.push_str(&format!(" {:016x}", u64::from_le_bytes(c.try_into().unwrap())));
                }
            }
            record("d3d12_xs", "CreateGraphicsPSO_X", &s);
        }
        // Diagnostic: depth-only pipelines (no colour target) — input layout + original VS.
        {
            static DO: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
            if u32at(0x240) == 0 && u32at(0x264) != 0 && DO.fetch_add(1, Ordering::Relaxed) < 6 {
                let ptr = rd(&desc, 0x228) as *const u8;
                let n = (u32at(0x230) as usize).min(16);
                let mut s = format!("depth-only PSO: {n} input elements");
                for i in 0..n {
                    let e = unsafe { core::slice::from_raw_parts(ptr.add(i * 32), 32) };
                    let name_ptr = u64::from_le_bytes(e[0..8].try_into().unwrap()) as *const u8;
                    let mut name = String::new();
                    if !name_ptr.is_null() {
                        let mut k = 0;
                        while k < 32 && unsafe { *name_ptr.add(k) } != 0 { name.push(unsafe { *name_ptr.add(k) } as char); k += 1; }
                    }
                    let f = |o: usize| u32::from_le_bytes(e[o..o + 4].try_into().unwrap());
                    s.push_str(&format!("\n    {name}{} fmt={} slot={} off={} class={} step={}", f(8), f(12), f(16), f(20), f(24), f(28)));
                }
                record("d3d12_xs", "PSOFMT", &s);
                let (p, len) = (unsafe { core::ptr::read_unaligned(pdesc.add(0x08) as *const u64) } as *const u8, unsafe { core::ptr::read_unaligned(pdesc.add(0x10) as *const u64) } as usize);
                if !p.is_null() && len > 0 && len < 0x100000 {
                    let dir = std::env::temp_dir().join("tombox_shaders");
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::write(dir.join(format!("depthonly_vs_{}.dxbc", DO.load(Ordering::Relaxed))), unsafe { core::slice::from_raw_parts(p, len) });
                }
            }
        }
        // Diagnostic: output-merger formats of the first PSOs plus the (so far ignored)
        // Xbox extension struct, to check whether formats live outside the PC-layout desc.
        static FMTLOG: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if FMTLOG.fetch_add(1, Ordering::Relaxed) < 60 {
            let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
            let ext = if _pext.is_null() || _num_ext == 0 { String::from("none") } else {
                unsafe { core::slice::from_raw_parts(_pext, 0x40) }.chunks(4).map(|c| format!("{:x}", u32::from_le_bytes(c.try_into().unwrap()))).collect::<Vec<_>>().join(" ")
            };
            record("d3d12_xs", "PSOFMT", &format!(
                "base numRT={} rtv=[{} {} {} {}] dsv={} depthEnable={} depthWrite={} samples={} numExt={} ext[{}]",
                u32at(0x240), u32at(0x244), u32at(0x248), u32at(0x24c), u32at(0x250), u32at(0x264), u32at(0x1f0), u32at(0x1f4), u32at(0x268), _num_ext, ext));
        }
        let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
            unsafe { core::mem::transmute(*real_vtbl(real).add(10)) };
        unsafe { crate::cl2_state::patch_gfx_pso_desc(real, &mut desc) };
        let t0 = std::time::Instant::now();
        let mut via_ext = false;
        let hr = crate::perf::timed(crate::perf::PSO, || unsafe {
            match crate::pso_ext::create(real, &desc, _num_ext, _pext, riid, ppv) {
                Some(h) => {
                    via_ext = true;
                    h
                }
                None => crate::pso_lib::gfx(real, &desc, riid, ppv, &|d| f(real, d, riid, ppv)),
            }
        });
        if hr >= 0 && !ppv.is_null() {
            crate::hang_cap::note_origin(unsafe { *ppv } as usize, || format!("original edits=[{}] numExt={_num_ext} created_via={}", edits.join(","), if via_ext { "pso_ext::create (Xbox ext entries applied)" } else { "CreateGraphicsPipelineState(desc)" }));
            unsafe { crate::pso_ext::note(*ppv, _num_ext, _pext) };
            crate::pso_ext::ps::note_pso(unsafe { *ppv }, ps_origin.clone());
        }
        let drv_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if drv_ms > 30.0 {
            static SLOW: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            if SLOW.fetch_add(1, Ordering::Relaxed) < 40 {
                record("d3d12_xs", "CreateGraphicsPSO_X", &format!("driver CreateGraphicsPipelineState took {drv_ms:.0} ms (hr={hr:#010x})"));
            }
        }
        if hr == 0 {
            let n = OK.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 5 {
                record("d3d12_xs", "CreateGraphicsPSO_X", &format!("PSO #{n} created from translated shaders"));
            }
        } else {
            let n = FAIL.fetch_add(1, Ordering::Relaxed);
            if n < 4 {
                unsafe { crate::cmdlist_x::dump_device_messages(real, "CreateGraphicsPSO_X") };
                let b = unsafe { bisect_gfx_pso(real, &desc, riid) };
                record("d3d12_xs", "CreateGraphicsPSO_X", &format!("failure #{n} bisection (each variant changes one part of the desc):{b}"));
                unsafe { dump_failed_gfx_pso(n, &desc) };
                unsafe { dump_failed_gfx_pso(n + 100, core::slice::from_raw_parts(pdesc, DESC_SIZE)) };
            }
            if n < 10 {
                let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
                record("d3d12_xs", "CreateGraphicsPSO_X", &format!(
                    "real CreateGraphicsPipelineState FAILED hr={hr:#010x} (translated shaders ok) topo={} numRT={} rtv0={} dsv={} samples={} inputElems={}",
                    u32at(0x23c), u32at(0x240), u32at(0x244), u32at(0x264), u32at(0x268), u32at(0x230)));
            }
        }
        if hr == 0 && !ppv.is_null() {
            crate::dred_marks::note(unsafe { *ppv }, unsafe { crate::dred_marks::gfx_hashes(pdesc) }, false);
            unsafe { register_pso(*ppv, desc.clone(), keep.clone()) };
        }
        hr
    };
    drop(keep);
    static LOGN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    if LOGN.fetch_add(1, Ordering::Relaxed) % 32 == 0 {
        let (sok, sfail) = crate::shader_xlat::counts();
        record("d3d12_xs", "CreateGraphicsPSO_X", &format!(
            "counts: pso_ok={} pso_fail={} untranslatable={} | shaders ok={sok} fail={sfail} | {}",
            OK.load(Ordering::Relaxed), FAIL.load(Ordering::Relaxed), XFAIL.load(Ordering::Relaxed), crate::pso_async::summary()));
    }
    hr
}

/// A created PSO's PC desc plus everything it points at, so slot 55 can derive variants.
struct PsoRecord {
    desc: Vec<u8>,
    _shaders: Vec<std::sync::Arc<Vec<u8>>>,
    _layout: Vec<u8>,
    _names: Vec<Vec<u8>>,
    _so: Vec<u8>,
    _strides: Vec<u8>,
    root_sig: usize,
}

impl Drop for PsoRecord {
    fn drop(&mut self) {
        if self.root_sig != 0 {
            unsafe {
                let rel: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*(*(self.root_sig as *const *const usize)).add(2));
                rel(self.root_sig as *mut c_void);
            }
        }
    }
}
// SAFETY: the raw pointers inside `desc` only reference `_shaders`/`_layout`/`_names` (owned here)
// and the title's root signature object.
unsafe impl Send for PsoRecord {}
unsafe impl Sync for PsoRecord {}

fn pso_registry() -> &'static std::sync::Mutex<std::collections::HashMap<usize, std::sync::Arc<PsoRecord>>> {
    static R: OnceLock<std::sync::Mutex<std::collections::HashMap<usize, std::sync::Arc<PsoRecord>>>> = OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Snapshot a (translated) desc so it stays valid after the title frees its copy:
/// deep-copies the input-element array and repoints the desc at the copy.
unsafe fn register_pso(pso: *mut c_void, mut desc: Vec<u8>, shaders: Vec<std::sync::Arc<Vec<u8>>>) {
    let ptr = u64::from_le_bytes(desc[0x228..0x230].try_into().unwrap()) as *const u8;
    let n = u32::from_le_bytes(desc[0x230..0x234].try_into().unwrap()) as usize;
    let mut layout = if ptr.is_null() || n == 0 || n > 64 {
        Vec::new()
    } else {
        unsafe { core::slice::from_raw_parts(ptr, n * 32).to_vec() }
    };
    let names = unsafe { own_semantic_names(&mut layout) };
    if !layout.is_empty() {
        desc[0x228..0x230].copy_from_slice(&(layout.as_ptr() as u64).to_le_bytes());
    }
    desc[0x278..0x288].fill(0);
    let snap = |d: &mut Vec<u8>, po: usize, no: usize, stride: usize, max: usize| -> Vec<u8> {
        let p = u64::from_le_bytes(d[po..po + 8].try_into().unwrap()) as *const u8;
        let n = u32::from_le_bytes(d[no..no + 4].try_into().unwrap()) as usize;
        if p.is_null() || n == 0 || n > max {
            d[po..po + 8].fill(0);
            d[no..no + 4].fill(0);
            return Vec::new();
        }
        let v = unsafe { core::slice::from_raw_parts(p, n * stride).to_vec() };
        d[po..po + 8].copy_from_slice(&(v.as_ptr() as u64).to_le_bytes());
        v
    };
    let so = snap(&mut desc, 0x58, 0x60, 24, 64);
    let strides = snap(&mut desc, 0x68, 0x70, 4, 4);
    let root_sig = u64::from_le_bytes(desc[0..8].try_into().unwrap()) as usize;
    if root_sig != 0 {
        unsafe {
            let add: unsafe extern "system" fn(*mut c_void) -> u32 = core::mem::transmute(*(*(root_sig as *const *const usize)).add(1));
            add(root_sig as *mut c_void);
        }
    }
    let rec = std::sync::Arc::new(PsoRecord { desc, _shaders: shaders, _layout: layout, _names: names, _so: so, _strides: strides, root_sig });
    crate::frame_ev::lock(pso_registry()).insert(pso as usize, rec);
}

unsafe fn own_semantic_names(layout: &mut [u8]) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    for e in layout.chunks_exact_mut(32) {
        let p = u64::from_le_bytes(e[0..8].try_into().unwrap()) as *const u8;
        if p.is_null() {
            continue;
        }
        let mut s = Vec::new();
        while s.len() < 256 {
            let b = unsafe { *p.add(s.len()) };
            if b == 0 {
                break;
            }
            s.push(b);
        }
        s.push(0);
        e[0..8].copy_from_slice(&(s.as_ptr() as u64).to_le_bytes());
        names.push(s);
    }
    names
}

pub(crate) fn pso_record_bytes(pso: usize) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<Vec<u8>>)> {
    let rec = crate::frame_ev::lock(pso_registry()).get(&pso).cloned()?;
    let rd = |o: usize| u64::from_le_bytes(rec.desc[o..o + 8].try_into().unwrap());
    let blob = |o: usize| {
        let (p, n) = (rd(o) as *const u8, rd(o + 8) as usize);
        if p.is_null() || n == 0 || n > 0x100_0000 { Vec::new() } else { unsafe { core::slice::from_raw_parts(p, n).to_vec() } }
    };
    Some((rec.desc.clone(), blob(0x08), blob(0x18), rec._layout.clone(), rec._names.clone()))
}

pub(crate) unsafe fn register_gfx_pso(pso: *mut c_void, desc: Vec<u8>, shaders: Vec<std::sync::Arc<Vec<u8>>>) {
    unsafe { register_pso(pso, desc, shaders) }
}

pub(crate) fn gfx_pso_identity(pso: usize) -> usize {
    crate::frame_ev::lock(pso_registry()).get(&pso).map(|r| std::sync::Arc::as_ptr(r) as usize).unwrap_or(0)
}

pub(crate) fn gfx_pso_desc(pso: usize) -> Option<(Vec<u8>, std::sync::Arc<impl Sized + Send + Sync>)> {
    let rec = crate::frame_ev::lock(pso_registry()).get(&pso).cloned()?;
    Some((rec.desc.clone(), rec))
}

pub(crate) unsafe fn real_device(this: *mut c_void) -> *mut c_void {
    unsafe { (*(this as *mut DeviceWrapper)).real }
}

/// For the diagnostic draw trace: (translated VS pointer, translated PS pointer, state text).
pub fn pso_desc_text(pso: usize) -> Option<String> {
    let real = crate::pso_async::real_of(pso);
    let reg = crate::frame_ev::lock(pso_registry());
    let (rec, via) = match reg.get(&pso) {
        Some(r) => (r.clone(), String::new()),
        None => (reg.get(&real)?.clone(), format!("async real {real:#x} ")),
    };
    drop(reg);
    Some(format!("{via}{}", desc_text(&rec.desc)))
}

pub fn desc_text(d: &[u8]) -> String {
    let u = |o: usize| d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
    let q = |o: usize| d.get(o..o + 8).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()));
    let mut t = format!("vs={}B ps={}B ds={}B hs={}B gs={}B so(n={} strides={} rast={}) a2c={} indep={}", q(0x10), q(0x20), q(0x30), q(0x40), q(0x50), u(0x60), u(0x70), u(0x74), u(0x78), u(0x7c));
    let nrt = u(0x240).min(8) as usize;
    for i in 0..nrt.max(1) {
        let b = 0x80 + i * 40;
        t.push_str(&format!(" rt{i}(fmt={} blend={} logic={} src={} dst={} op={} srcA={} dstA={} opA={} lop={} mask={:#x})", u(0x244 + i * 4), u(b), u(b + 4), u(b + 8), u(b + 12), u(b + 16), u(b + 20), u(b + 24), u(b + 28), u(b + 32), d.get(b + 36).copied().unwrap_or(0)));
    }
    t.push_str(&format!(" samplemask={:#x} raster(fill={} cull={} ccw={} bias={} biasclamp={} slope={} clip={} msaa={} aaline={} forced={} cons={})", u(0x1c0), u(0x1c4), u(0x1c8), u(0x1cc), u(0x1d0) as i32, f32::from_bits(u(0x1d4)), f32::from_bits(u(0x1d8)), u(0x1dc), u(0x1e0), u(0x1e4), u(0x1e8), u(0x1ec)));
    t.push_str(&format!(" ds(en={} write={} func={} st={} rm={:#x} wm={:#x} front={}/{}/{}/{} back={}/{}/{}/{})", u(0x1f0), u(0x1f4), u(0x1f8), u(0x1fc), d.get(0x200).copied().unwrap_or(0), d.get(0x201).copied().unwrap_or(0), u(0x204), u(0x208), u(0x20c), u(0x210), u(0x214), u(0x218), u(0x21c), u(0x220)));
    t.push_str(&format!(" layout(n={}) cut={} topo={} nrt={} dsv={} sample={}/{} node={:#x} cached={} flags={:#x} len={}", u(0x230), u(0x238), u(0x23c), u(0x240), u(0x264), u(0x268), u(0x26c), u(0x270), q(0x280), u(0x288), d.len()));
    t
}

pub fn pso_summary(pso: usize) -> Option<(usize, usize, String)> {
    let rec = crate::frame_ev::lock(pso_registry()).get(&pso).cloned()?;
    let d = &rec.desc;
    let q = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap()) as usize;
    let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    // D3D12_BLEND_DESC at 0x78: AlphaToCoverage, IndependentBlend, then RT0 {BlendEnable, LogicOpEnable, Src, Dest, Op, SrcA, DestA, OpA, LogicOp, WriteMask}
    let state = format!("depth(en={} write={} func={}) stencil(en={} rmask={:#x} wmask={:#x}) raster(fill={} cull={} ccw={} clip={}) samplemask={:#x} topo={} blend0(en={} src={} dst={} op={} srcA={} dstA={} mask={:#x}) nRT={}",
        u(0x1f0), u(0x1f4), u(0x1f8), u(0x1fc), d[0x200], d[0x201], u(0x1c4), u(0x1c8), u(0x1cc), u(0x1dc), u(0x1c0), u(0x23c),
        u(0x80), u(0x88), u(0x8c), u(0x90), u(0x94), u(0x98), d[0xa4], u(0x240));
    Some((q(0x08), q(0x18), state))
}

/// CreateRenderTargetView (20) / CreateDepthStencilView (21): forwarded unchanged; the
/// (descriptor handle -> resource) pair is remembered for the diagnostic draw trace.
// Binding-trace hooks (diagnostic capture only). They are patched into the REAL device's
// method table rather than installed on the wrapper, because the title also reaches the
// real device directly (ID3D12DeviceChild::GetDevice on a resource or command list returns
// it) and views created that way would otherwise go unrecorded.
static REAL_ORIG: [core::sync::atomic::AtomicUsize; 40] = [const { core::sync::atomic::AtomicUsize::new(0) }; 40];
fn real_orig(slot: usize) -> usize {
    REAL_ORIG[slot].load(Ordering::Relaxed)
}
pub(crate) fn real_device_orig(slot: usize) -> usize {
    REAL_ORIG.get(slot).map_or(0, |v| v.load(Ordering::Relaxed))
}
unsafe extern "system" fn real_create_root_signature(real: *mut c_void, mask: u32, blob: *const u8, len: usize, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32 = unsafe { core::mem::transmute(real_orig(16)) };
    let stripped = unsafe { crate::rs_xbox::pc_blob_raw(blob, len) };
    let (blob, len) = stripped.as_ref().map_or((blob, len), |b| (b.as_ptr(), b.len()));
    let aliased = if blob.is_null() || len == 0 { None } else { crate::cbv_raw::alias_blob(unsafe { core::slice::from_raw_parts(blob, len) }).ok().flatten() };
    let (blob, len) = aliased.as_ref().map_or((blob, len), |b| (b.as_ptr(), b.len()));
    let copy = |b: *const u8, l: usize| if b.is_null() || l == 0 { Vec::new() } else { unsafe { core::slice::from_raw_parts(b, l) }.to_vec() };
    let sent = core::cell::RefCell::new(Vec::<u8>::new());
    let mut gds = unsafe { crate::gds::patch_root_signature(blob, len) };
    let (gb, gl) = gds.as_ref().map_or((blob, len), |(b, _)| (b.as_ptr(), b.len()));
    let cen = unsafe {
        crate::centroid_bind::create(gb, gl, &|b, l| {
            *sent.borrow_mut() = copy(b, l);
            f(real, mask, b, l, riid, ppv)
        }, ppv)
    };
    let mut hr = match (&cen, &gds) {
        (Some(hr), _) => *hr,
        (None, Some((b, _))) => {
            *sent.borrow_mut() = b.clone();
            unsafe { f(real, mask, b.as_ptr(), b.len(), riid, ppv) }
        }
        (None, None) => {
            *sent.borrow_mut() = copy(blob, len);
            unsafe { f(real, mask, blob, len, riid, ppv) }
        }
    };
    if hr < 0 && gds.is_some() {
        record("d3d12_xs", "gds", &format!("root signature with the GDS parameter rejected hr={hr:#010x}; created without GDS"));
        gds = None;
        *sent.borrow_mut() = copy(blob, len);
        hr = unsafe { f(real, mask, blob, len, riid, ppv) };
    }
    if hr >= 0 && !ppv.is_null() && unsafe { !(*ppv).is_null() } {
        let s = sent.borrow();
        if !s.is_empty() {
            crate::pso_lib::note_real_root(unsafe { *ppv } as usize, &s);
        }
    }
    if hr >= 0 && !ppv.is_null() {
        if let Some((_, idx)) = gds { crate::gds::note_root_signature(unsafe { *ppv }, idx) }
        if cen.is_none() { crate::centroid_bind::forget(unsafe { *ppv }) }
    }
    if hr >= 0 && !ppv.is_null() && !blob.is_null() && len != 0 {
        crate::replay::note_root_signature(unsafe { *ppv } as usize, unsafe { core::slice::from_raw_parts(blob, len) });
    }
    hr
}
unsafe extern "system" fn real_create_cbv(real: *mut c_void, pdesc: *const u8, dest: usize) {
    if !pdesc.is_null() {
        let va = unsafe { core::ptr::read_unaligned(pdesc as *const u64) };
        let size = unsafe { core::ptr::read_unaligned(pdesc.add(8) as *const u32) };
        crate::bindtrace::note_desc(dest, crate::bindtrace::Desc::Cbv { va, size });
    }
    let f: unsafe extern "system" fn(*mut c_void, *const u8, usize) = unsafe { core::mem::transmute(real_orig(17)) };
    unsafe { f(real, pdesc, dest) }
}
unsafe extern "system" fn real_create_srv(real: *mut c_void, res: *mut c_void, pdesc: *const u8, dest: usize) {
    let desc = if pdesc.is_null() { None } else { Some(unsafe { core::ptr::read_unaligned(pdesc as *const [u8; 40]) }) };
    crate::bindtrace::note_desc(dest, crate::bindtrace::Desc::Srv { res: res as usize, desc });
    crate::res3d::note_view(res, "SRV", desc.as_ref(), dest);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *const u8, usize) = unsafe { core::mem::transmute(real_orig(18)) };
    crate::placed_offset::with_true_desc(|| unsafe { f(real, res, pdesc, dest) });
    crate::bindtrace::note_desc_after(dest, 0, crate::bindtrace::Desc::Srv { res: res as usize, desc });
}
unsafe extern "system" fn real_create_uav(real: *mut c_void, res: *mut c_void, counter: *mut c_void, pdesc: *const u8, dest: usize) {
    let desc = if pdesc.is_null() { None } else { Some(unsafe { core::ptr::read_unaligned(pdesc as *const [u8; 40]) }) };
    crate::bindtrace::note_desc(dest, crate::bindtrace::Desc::Uav { res: res as usize, desc });
    crate::res3d::note_view(res, "UAV", desc.as_ref(), dest);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *const u8, usize) = unsafe { core::mem::transmute(real_orig(19)) };
    crate::placed_offset::with_true_desc(|| unsafe { f(real, res, counter, pdesc, dest) });
    crate::bindtrace::note_desc_after(dest, 0, crate::bindtrace::Desc::Uav { res: res as usize, desc });
}
unsafe extern "system" fn real_create_sampler(real: *mut c_void, pdesc: *const u8, dest: usize) {
    if !pdesc.is_null() {
        let desc = unsafe { core::ptr::read_unaligned(pdesc as *const [u8; 0x34]) };
        crate::bindtrace::note_desc(dest, crate::bindtrace::Desc::Sampler { desc });
    }
    let f: unsafe extern "system" fn(*mut c_void, *const u8, usize) = unsafe { core::mem::transmute(real_orig(22)) };
    unsafe { f(real, pdesc, dest) }
}
/// CopyDescriptors (slot 23): a null size array means every range has one descriptor.
unsafe extern "system" fn real_copy_descriptors(
    real: *mut c_void, nd: u32, dstarts: *const usize, dsizes: *const u32,
    ns: u32, sstarts: *const usize, ssizes: *const u32, heap_type: u32,
) {
    if !dstarts.is_null() && !sstarts.is_null() {
        // Walk both range lists in step, as the API does.
        let size = |p: *const u32, i: u32| if p.is_null() { 1 } else { unsafe { *p.add(i as usize) } };
        let inc = crate::bindtrace::increment(heap_type);
        let (mut di, mut doff, mut si, mut soff) = (0u32, 0u32, 0u32, 0u32);
        while di < nd && si < ns {
            let (dn, sn) = (size(dsizes, di), size(ssizes, si));
            let n = (dn - doff).min(sn - soff);
            if n == 0 {
                break;
            }
            unsafe {
                crate::bindtrace::copy_desc(*dstarts.add(di as usize) + doff as usize * inc, *sstarts.add(si as usize) + soff as usize * inc, n, heap_type);
            }
            doff += n;
            soff += n;
            if doff == dn { di += 1; doff = 0; }
            if soff == sn { si += 1; soff = 0; }
        }
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, *const usize, *const u32, u32, *const usize, *const u32, u32) =
        unsafe { core::mem::transmute(real_orig(23)) };
    unsafe { f(real, nd, dstarts, dsizes, ns, sstarts, ssizes, heap_type) }
}
unsafe extern "system" fn real_copy_descriptors_simple(real: *mut c_void, n: u32, dest: usize, src: usize, heap_type: u32) {
    crate::bindtrace::copy_desc(dest, src, n, heap_type);
    let f: unsafe extern "system" fn(*mut c_void, u32, usize, usize, u32) = unsafe { core::mem::transmute(real_orig(24)) };
    unsafe { f(real, n, dest, src, heap_type) }
}
unsafe extern "system" fn real_make_resident(real: *mut c_void, n: u32, objs: *const *mut c_void) -> i32 {
    unsafe { crate::hang_cap::note_residency(false, n, objs) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const *mut c_void) -> i32 = unsafe { core::mem::transmute(real_orig(34)) };
    unsafe { f(real, n, objs) }
}
unsafe extern "system" fn real_evict(real: *mut c_void, n: u32, objs: *const *mut c_void) -> i32 {
    unsafe { crate::hang_cap::note_residency(true, n, objs) };
    let f: unsafe extern "system" fn(*mut c_void, u32, *const *mut c_void) -> i32 = unsafe { core::mem::transmute(real_orig(35)) };
    unsafe { f(real, n, objs) }
}
/// Patch the real device's method table (shared by every interface version of the device).
unsafe fn patch_real_device(real: *mut c_void) {
    extern "system" {
        fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
    }
    static DONE: std::sync::Once = std::sync::Once::new();
    DONE.call_once(|| unsafe {
        let vt = *(real as *const *mut usize);
        let hooks: [(usize, usize); 7] = [
            (16, real_create_root_signature as usize),
            (17, real_create_cbv as usize), (18, real_create_srv as usize), (19, real_create_uav as usize),
            (22, real_create_sampler as usize), (23, real_copy_descriptors as usize), (24, real_copy_descriptors_simple as usize),
        ];
        let mut old = 0u32;
        if VirtualProtect(vt.add(16) as *mut c_void, 9 * 8, 0x04, &mut old) == 0 {
            record("d3d12_xs", "bindtrace", "could not unprotect the device method table - descriptor trace disabled");
            return;
        }
        let full = crate::rtcap::enabled();
        for (slot, f) in hooks {
            if slot == 16 && !full {
                continue;
            }
            REAL_ORIG[slot].store(*vt.add(slot), Ordering::Relaxed);
            *vt.add(slot) = f;
        }
        let mut tmp = 0u32;
        VirtualProtect(vt.add(16) as *mut c_void, 9 * 8, old, &mut tmp);
        record("d3d12_xs", "bindtrace", "descriptor hooks installed on the real device method table");
        if crate::hang_cap::on() {
            let mut old2 = 0u32;
            if VirtualProtect(vt.add(34) as *mut c_void, 2 * 8, 0x04, &mut old2) != 0 {
                for (slot, f) in [(34usize, real_make_resident as usize), (35, real_evict as usize)] {
                    REAL_ORIG[slot].store(*vt.add(slot), Ordering::Relaxed);
                    *vt.add(slot) = f;
                }
                VirtualProtect(vt.add(34) as *mut c_void, 2 * 8, old2, &mut tmp);
                record("d3d12_xs", "hang_cap", "MakeResident/Evict hooks installed on the real device method table");
            }
        }
    });
}

unsafe extern "system" fn create_rtv_traced(this: *mut DeviceWrapper, res: *mut c_void, pdesc: *const u8, dest: usize) {
    let real = unsafe { (*this).real };
    let inc: unsafe extern "system" fn(*mut c_void, u32) -> u32 = unsafe { core::mem::transmute(*real_vtbl(real).add(15)) };
    crate::firstuse::note(res as usize, crate::firstuse::RTV);
    crate::rtcap::note_view(dest, res as usize, unsafe { inc(real, 2) } as usize);
    unsafe { crate::blendclamp::note_rtv(dest, res as usize, pdesc) };
    unsafe { crate::rtcap::register_view_target(res, 4) };
    crate::replay::note_view(dest, 'r', res as usize, if pdesc.is_null() { None } else { Some(unsafe { core::slice::from_raw_parts(pdesc, 24) }) });
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *const u8, usize) = unsafe { core::mem::transmute(*real_vtbl(real).add(20)) };
    unsafe { f(real, res, pdesc, dest) };
}
unsafe extern "system" fn create_dsv_traced(this: *mut DeviceWrapper, res: *mut c_void, pdesc: *const u8, dest: usize) {
    let real = unsafe { (*this).real };
    crate::firstuse::note(res as usize, crate::firstuse::DSV);
    crate::rtcap::note_view(dest, res as usize, 0);
    unsafe { crate::rtcap::register_view_target(res, 0x10) };
    crate::replay::note_view(dest, 'd', res as usize, if pdesc.is_null() { None } else { Some(unsafe { core::slice::from_raw_parts(pdesc, 24) }) });
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *const u8, usize) = unsafe { core::mem::transmute(*real_vtbl(real).add(21)) };
    unsafe { f(real, res, pdesc, dest) };
}

/// Xbox "derived pipeline" create (device slot 55), from the RHI call site at EXE+0xd56979:
/// `(this, pBasePSO, numPatches, pPatches, riid, ppv)`. Each patch is a 0x14C-byte tagged
/// struct (u32 tag, payload at +4) overriding one block of the base pipeline's state:
///   0 = D3D12_DEPTH_STENCIL_DESC (0x34)   1 = D3D12_RASTERIZER_DESC (0x2C)
///   2 = D3D12_BLEND_DESC (0x148)          3 = SampleMask (u32)
/// Tags/payload sizes are read off the code that fills them (FD3D12 CreatePipelineState).
unsafe extern "system" fn create_derived_pso_x(
    this: *mut DeviceWrapper, base: *mut c_void, num: u32, patches: *const u8,
    riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let _ev = crate::frame_ev::begin(crate::frame_ev::PSO_SYNC);
    let t0 = std::time::Instant::now();
    let _ = crate::pso_lib::take_lock_wait_us();
    let hr = unsafe { create_derived_pso_inner(this, base, num, patches, riid, ppv) };
    note_sync_pso("derived graphics (CreateDerivedPipelineState_X)", "derived PSOs are always created on the calling thread", t0, hr);
    hr
}

unsafe fn create_derived_pso_inner(
    this: *mut DeviceWrapper, base: *mut c_void, num: u32, patches: *const u8,
    riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    const E_FAIL: i32 = 0x8000_4005u32 as i32;
    const STRIDE: usize = 0x14C;
    static OK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static FAIL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let real = unsafe { (*this).real };
    let base = crate::pso_async::wait_real(base);
    let rec = crate::frame_ev::lock(pso_registry()).get(&(base as usize)).cloned();
    let Some(rec) = rec else {
        if FAIL.fetch_add(1, Ordering::Relaxed) < 6 {
            record("d3d12_xs", "CreateDerivedPSO_X", &format!("base PSO {base:p} unknown (its creation failed?) -> E_FAIL"));
        }
        return E_FAIL;
    };
    let mut desc = rec.desc.clone();
    let mut tags = String::new();
    let mut ds_ext = crate::pso_ext::entry_of(base);
    let mut ps_origin = crate::pso_ext::ps::origin_of(base);
    let mut shaders = rec._shaders.clone();
    if !patches.is_null() {
        for i in 0..num as usize {
            let p = unsafe { patches.add(i * crate::pso_ext::DERIVED_PATCH_STRIDE) };
            let tag = unsafe { core::ptr::read_unaligned(p as *const u32) };
            let payload = |n: usize| unsafe { core::slice::from_raw_parts(p.add(4), n) };
            tags.push_str(&format!("{tag} "));
            if tag == 0 {
                static T0: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
                if u32::from_le_bytes(desc[0x264..0x268].try_into().unwrap()) != 0 && T0.fetch_add(1, Ordering::Relaxed) < 400 {
                    let hex = payload(0x34).chunks(4).map(|c| format!("{:x}", u32::from_le_bytes(c.try_into().unwrap()))).collect::<Vec<_>>().join(" ");
                    let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
                    record("d3d12_xs", "PSOFMT", &format!("tag0 numRT={} dsv={} payload[{hex}]", u32at(0x240), u32at(0x264)));
                }
            }
            if crate::pso_ext::apply_derived_patch(tag, payload(crate::pso_ext::DERIVED_PATCH_STRIDE - 4), &mut desc, &mut ds_ext) {
                continue;
            }
            match tag {
                4 => {
                    crate::pso_ext::note_derived(tag, payload(0x3c));
                    match ps_origin.as_mut() {
                        Some(o) => o.options = crate::pso_ext::ps::merge(&o.options, &crate::pso_ext::ps::options_of(payload(0x20))),
                        None => record("d3d12_xs", "CreateDerivedPSO_X", "tag 4 pixel-shader options on a base PSO without a recorded pixel shader; options not applied"),
                    }
                }
                6 | 7 => {
                    crate::pso_ext::note_derived(tag, payload(0x3c));
                }
                other => {
                    if FAIL.fetch_add(1, Ordering::Relaxed) < 6 {
                        record("d3d12_xs", "CreateDerivedPSO_X", &format!("patch tag {other}: the console (xgs12_10 0x5b180) stores payload dword v as v<<16|v&0xffff at PSO state +0x98, a register not yet decoded with no PC mapping -> E_FAIL"));
                    }
                    return E_FAIL;
                }
            }
        }
    }
    {
        static DL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if DL.fetch_add(1, Ordering::Relaxed) < 60 {
            let u32at = |o: usize| u32::from_le_bytes(desc[o..o + 4].try_into().unwrap());
            record("d3d12_xs", "PSOFMT", &format!(
                "derived tags=[{}] numRT={} rtv0={} dsv={} depthEnable={} depthWrite={} depthFunc={} stencil={}",
                tags.trim(), u32at(0x240), u32at(0x244), u32at(0x264), u32at(0x1f0), u32at(0x1f4), u32at(0x1f8), u32at(0x1fc)));
        }
    }
    if let Some(o) = ps_origin.as_ref().filter(|o| o.options.iter().any(|&v| v != 0)) {
        let code = crate::pso_ext::ps::variant(o, crate::pso_ext::ps::depth_stencil_writes(&desc)).or_else(|| crate::frame_ev::time(crate::frame_ev::XLAT, || unsafe { crate::shader_xlat::translate_for_targets(o.blob.as_ptr(), o.blob.len(), o.rtfmt, &crate::shader_xlat::no_link()) }).ok());
        if let Some(code) = code {
            desc[0x18..0x20].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
            desc[0x20..0x28].copy_from_slice(&(code.len() as u64).to_le_bytes());
            shaders.push(code);
        }
    }
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(10)) };
    let ext_ptr = ds_ext.as_ref().map_or(core::ptr::null(), |e| e.as_ptr());
    let mut via_ext = false;
    let hr = unsafe {
        match crate::pso_ext::create(real, &desc, ds_ext.is_some() as u32, ext_ptr, riid, ppv) {
            Some(h) => {
                via_ext = true;
                h
            }
            None => crate::pso_lib::gfx(real, &desc, riid, ppv, &|d| f(real, d, riid, ppv)),
        }
    };
    if hr >= 0 && !ppv.is_null() {
        crate::hang_cap::note_origin(unsafe { *ppv } as usize, || format!("derived from base {base:p} patch tags [{}] ds_ext={} created_via={}", tags.trim(), ds_ext.is_some(), if via_ext { "pso_ext::create" } else { "CreateGraphicsPipelineState(desc)" }));
        unsafe { crate::pso_ext::note(*ppv, ds_ext.is_some() as u32, ext_ptr) };
        crate::pso_ext::ps::note_pso(unsafe { *ppv }, ps_origin.clone());
    }
    if hr == 0 && !ppv.is_null() {
        let n = OK.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= 4 || n % 100 == 0 {
            record("d3d12_xs", "CreateDerivedPSO_X", &format!("derived PSO #{n} created (patch tags: {tags})"));
        }
        crate::dred_marks::inherit(unsafe { *ppv }, base);
        unsafe { register_pso(*ppv, desc, shaders) };
    } else if FAIL.fetch_add(1, Ordering::Relaxed) < 10 {
        record("d3d12_xs", "CreateDerivedPSO_X", &format!("real CreateGraphicsPipelineState FAILED hr={hr:#010x} (tags: {tags})"));
        if std::env::var_os("TOMBOX_PSO_BISECT").is_none() {
            return hr;
        }
        let b = unsafe { bisect_gfx_pso(real, &desc, riid) };
        let hex = |a: usize, n: usize| desc[a..a + n].chunks(4).map(|c| format!("{:x}", u32::from_le_bytes(c.try_into().unwrap()))).collect::<Vec<_>>().join(" ");
        record("d3d12_xs", "CreateDerivedPSO_X", &format!("failure bisection:{b} | raster[{}] dss[{}] blend0[{}] mask={} flags={:x} topo={} samples={} base_raster[{}]", hex(0x1c4, 0x2c), hex(0x1f0, 0x34), hex(0x78, 0x30), hex(0x1c0, 4), u32::from_le_bytes(desc[0x288..0x28c].try_into().unwrap()), hex(0x23c, 4), hex(0x268, 8), rec.desc[0x1c4..0x1f0].chunks(4).map(|c| format!("{:x}", u32::from_le_bytes(c.try_into().unwrap()))).collect::<Vec<_>>().join(" ")));
    }
    hr
}

/// Xbox compute-PSO create (device slot 62), call site EXE+0xd54c30:
/// `(this, pDesc, numExt, pExtDesc, riid, ppv)` with a PC-layout
/// D3D12_COMPUTE_PIPELINE_STATE_DESC { pRootSignature@0, CS{ptr@8,len@0x10}, NodeMask@0x18,
/// CachedPSO@0x20, Flags@0x30 }. The CS is translated like graphics stages.
pub fn dxbc_part<'a>(blob: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    if blob.len() < 32 || &blob[0..4] != b"DXBC" {
        return None;
    }
    let n = u32::from_le_bytes(blob[28..32].try_into().ok()?) as usize;
    for i in 0..n.min(64) {
        let o = 32 + 4 * i;
        let off = u32::from_le_bytes(blob.get(o..o + 4)?.try_into().ok()?) as usize;
        let hdr = blob.get(off..off + 8)?;
        let len = u32::from_le_bytes(hdr[4..8].try_into().ok()?) as usize;
        if &hdr[0..4] == tag {
            return blob.get(off + 8..off + 8 + len);
        }
    }
    None
}

pub fn wrap_root_signature(rts: &[u8]) -> Vec<u8> {
    let total = (32 + 4 + 8 + rts.len()) as u32;
    let mut v = Vec::with_capacity(total as usize);
    v.extend_from_slice(b"DXBC");
    v.extend_from_slice(&[0u8; 16]);
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&total.to_le_bytes());
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(&36u32.to_le_bytes());
    v.extend_from_slice(b"RTS0");
    v.extend_from_slice(&(rts.len() as u32).to_le_bytes());
    v.extend_from_slice(rts);
    v
}

static ROOT_FROM_SHADER: std::sync::Mutex<Option<std::collections::HashMap<u64, usize>>> = std::sync::Mutex::new(None);

pub(crate) fn shader_root_container(rts: &[u8], raw: bool) -> Vec<u8> {
    let console = wrap_root_signature(rts);
    let container = crate::rs_xbox::pc_blob(&console).unwrap_or(console);
    if !raw {
        return match crate::cbv_raw::copy_mode_blob(&container) {
            Ok(Some(b)) => b,
            Ok(None) => container,
            Err(e) => {
                record("d3d12_xs", "PSO.rootFromShader", &format!("bindless alias failed ({e}); root signature left as the shader gives it"));
                container
            }
        };
    }
    match crate::cbv_raw::patch_blob(&container) {
        Ok(Some((b, _))) => b,
        Ok(None) => container,
        Err(e) => {
            record("d3d12_xs", "PSO.rootFromShader", &format!("cbv_raw patch failed ({e}); root signature left as the shader gives it"));
            container
        }
    }
}

unsafe fn root_from_shaders(real: *mut c_void, desc: &[u8], stages: &[usize]) -> usize {
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    for &o in stages {
        let p = u64::from_le_bytes(desc[o..o + 8].try_into().unwrap()) as *const u8;
        let n = u64::from_le_bytes(desc[o + 8..o + 16].try_into().unwrap()) as usize;
        if p.is_null() || n < 32 || n > 64 << 20 {
            continue;
        }
        let blob = unsafe { core::slice::from_raw_parts(p, n) };
        let Some(rts) = dxbc_part(blob, b"RTS0") else { continue };
        let key = crate::device_x::fnv64(rts);
        let mut g = ROOT_FROM_SHADER.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(std::collections::HashMap::new);
        if let Some(&r) = m.get(&key) {
            return r;
        }
        let f: unsafe extern "system" fn(*mut c_void, u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32 =
            unsafe { core::mem::transmute(*real_vtbl(real).add(16)) };
        let mut rs: *mut c_void = core::ptr::null_mut();
        let container = shader_root_container(rts, crate::cbv_raw::enabled());
        let hr = unsafe { f(real, 0, container.as_ptr(), container.len(), &IID_ROOT_SIGNATURE, &mut rs) };
        if N.fetch_add(1, Ordering::Relaxed) < 6 {
            record("d3d12_xs", "PSO.rootFromShader", &format!("null pRootSignature; built root signature from the shader's RTS0 part ({} bytes, header {:x?}) -> hr={hr:#010x}", rts.len(), rts.chunks_exact(4).take(6).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect::<Vec<_>>()));
            let _ = std::fs::create_dir_all(std::env::temp_dir().join("tombox").join("psodump"));
            let _ = std::fs::write(std::env::temp_dir().join("tombox").join("psodump").join(format!("rts0_{key:016x}.bin")), rts);
        }
        if hr >= 0 && !rs.is_null() {
            crate::pso_lib::note_root(rs as usize, 0, &container, true);
            m.insert(key, rs as usize);
            return rs as usize;
        }
    }
    0
}

const IID_ROOT_SIGNATURE: Guid = Guid { d1: 0xc54a_6b66, d2: 0x72df, d3: 0x4ee8, d4: [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14] };

unsafe extern "system" fn create_gfx_pso_std(this: *mut DeviceWrapper, pdesc: *const u8, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    if pdesc.is_null() || unsafe { core::ptr::read_unaligned(pdesc as *const u64) } != 0 {
        return unsafe { create_gfx_pso_x(this, pdesc, 0, core::ptr::null(), riid, ppv) };
    }
    let mut d = unsafe { core::slice::from_raw_parts(pdesc, crate::pso_serial::GFX_DESC_SIZE) }.to_vec();
    let root = unsafe { root_from_shaders((*this).real, &d, &crate::pso_serial::GFX_STAGES) };
    d[0..8].copy_from_slice(&(root as u64).to_le_bytes());
    unsafe { create_gfx_pso_x(this, d.as_ptr(), 0, core::ptr::null(), riid, ppv) }
}

unsafe extern "system" fn create_compute_pso_std(this: *mut DeviceWrapper, pdesc: *const u8, riid: *const Guid, ppv: *mut *mut c_void) -> i32 {
    if pdesc.is_null() || unsafe { core::ptr::read_unaligned(pdesc as *const u64) } != 0 {
        return unsafe { create_compute_pso_x(this, pdesc, 0, core::ptr::null(), riid, ppv) };
    }
    let mut d = unsafe { core::slice::from_raw_parts(pdesc, crate::pso_serial::COMPUTE_DESC_SIZE) }.to_vec();
    let root = unsafe { root_from_shaders((*this).real, &d, &[8]) };
    d[0..8].copy_from_slice(&(root as u64).to_le_bytes());
    unsafe { create_compute_pso_x(this, d.as_ptr(), 0, core::ptr::null(), riid, ppv) }
}

unsafe extern "system" fn create_compute_pso_x(
    this: *mut DeviceWrapper, pdesc: *const u8, num_ext: u32, pext: *const u8,
    riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    unsafe { crate::pso_xrec::note(crate::pso_xrec::CS, pdesc, num_ext) };
    let mut ev = crate::frame_ev::begin(crate::frame_ev::PSO_SYNC);
    if let Some(hr) = unsafe { crate::pso_async::create_compute(this, (*this).real, pdesc, num_ext, pext, riid, ppv, create_compute_pso_sync) } {
        if let Some(s) = ev.as_mut() {
            s.set_kind(if crate::frame_ev::take_async_mark() { crate::frame_ev::PSO_ASYNC } else { crate::frame_ev::PSO_INLINE });
        }
        return crate::cbv_copy::after_compute_pso(hr, pdesc, ppv);
    }
    let t0 = std::time::Instant::now();
    let _ = crate::pso_lib::take_lock_wait_us();
    let hr = unsafe { create_compute_pso_sync(this, pdesc, num_ext, pext, riid, ppv) };
    note_sync_pso("compute (CreateComputePipelineState_X)", unsafe { crate::pso_async::sync_reason(pdesc, riid, true) }, t0, hr);
    crate::cbv_copy::after_compute_pso(hr, pdesc, ppv)
}

unsafe extern "system" fn create_compute_pso_sync(
    this: *mut DeviceWrapper, pdesc: *const u8, _num_ext: u32, _pext: *const u8,
    riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    const E_FAIL: i32 = 0x8000_4005u32 as i32;
    static OK: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    static FAIL: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let real = unsafe { (*this).real };
    if pdesc.is_null() {
        return E_FAIL;
    }
    let mut desc = unsafe { core::slice::from_raw_parts(pdesc, 0x38).to_vec() };
    unsafe { crate::pso_ext::note_compute(_num_ext, _pext) };
    if u64::from_le_bytes(desc[0..8].try_into().unwrap()) == 0 {
        let root = unsafe { root_from_shaders(real, &desc, &[8]) };
        desc[0..8].copy_from_slice(&(root as u64).to_le_bytes());
    }
    let p = u64::from_le_bytes(desc[8..16].try_into().unwrap()) as *const u8;
    let n = u64::from_le_bytes(desc[16..24].try_into().unwrap()) as usize;
    let code = match crate::frame_ev::time(crate::frame_ev::XLAT, || unsafe { crate::shader_xlat::translate(p, n) }) {
        Ok(c) => c,
        Err(e) => {
            if FAIL.fetch_add(1, Ordering::Relaxed) < 6 {
                record("d3d12_xs", "CreateComputePSO_X",
                    &format!("untranslatable compute shader -> E_FAIL ({})", e.lines().next().unwrap_or("")));
            }
            return E_FAIL;
        }
    };
    desc[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    desc[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(11)) }; // PC CreateComputePipelineState
    let hr = unsafe { crate::pso_lib::compute(real, &desc, riid, ppv, &|d| f(real, d, riid, ppv)) };
    if hr == 0 && !ppv.is_null() && crate::rtcap::enabled() {
        crate::rtcap::note_compute_pso(unsafe { *ppv } as usize, code.as_ptr() as usize);
    }
    if hr == 0 && !ppv.is_null() {
        crate::dred_marks::note(unsafe { *ppv }, unsafe { crate::dred_marks::compute_hashes(pdesc) }, false);
        crate::device_x::register_compute(unsafe { *ppv }, &desc, code.clone());
    }
    if hr == 0 {
        let k = OK.fetch_add(1, Ordering::Relaxed) + 1;
        if k <= 4 || k % 100 == 0 {
            record("d3d12_xs", "CreateComputePSO_X", &format!("compute PSO #{k} created from translated shader"));
        }
    } else if FAIL.fetch_add(1, Ordering::Relaxed) < 10 {
        let src = if p.is_null() || n == 0 { &[][..] } else { unsafe { core::slice::from_raw_parts(p, n) } };
        let h = crate::device_x::fnv64(src);
        let cached = (u64::from_le_bytes(desc[0x20..0x28].try_into().unwrap()), u64::from_le_bytes(desc[0x28..0x30].try_into().unwrap()));
        let dir = std::env::temp_dir().join("tombox").join("psodump");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("cs_fail_{h:016x}.xbox")), src);
        let _ = std::fs::write(dir.join(format!("cs_fail_{h:016x}.dxil")), code.as_slice());
        record("d3d12_xs", "CreateComputePSO_X", &format!(
            "real CreateComputePipelineState FAILED hr={hr:#010x} cs={h:016x} in={n}B out={}B root={:#x} cachedPSO={:#x}/{}B flags={:#x} thread={:?} | {} | dumped to {}",
            code.len(), u64::from_le_bytes(desc[0..8].try_into().unwrap()), cached.0, cached.1,
            u32::from_le_bytes(desc[0x30..0x34].try_into().unwrap()), std::thread::current().id(), memory_status(), dir.display()));
        unsafe { crate::cmdlist_x::dump_device_messages(real, "CreateComputePSO_X") };
    }
    hr
}

#[repr(C)]
struct MemStatusEx { len: u32, load: u32, total_phys: u64, avail_phys: u64, total_page: u64, avail_page: u64, total_virt: u64, avail_virt: u64, avail_ext: u64 }

#[repr(C)]
#[derive(Default)]
struct ProcMemCounters { cb: u32, faults: u32, peak_ws: usize, ws: usize, qpp: usize, qp: usize, qnpp: usize, qnp: usize, pagefile: usize, peak_pagefile: usize, private: usize }

#[link(name = "kernel32")]
extern "system" {
    fn GlobalMemoryStatusEx(s: *mut MemStatusEx) -> i32;
    fn K32GetProcessMemoryInfo(p: *mut c_void, c: *mut ProcMemCounters, cb: u32) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}

fn memory_status() -> String {
    let mb = |v: u64| v >> 20;
    let mut g = MemStatusEx { len: core::mem::size_of::<MemStatusEx>() as u32, load: 0, total_phys: 0, avail_phys: 0, total_page: 0, avail_page: 0, total_virt: 0, avail_virt: 0, avail_ext: 0 };
    let mut c = ProcMemCounters { cb: core::mem::size_of::<ProcMemCounters>() as u32, ..Default::default() };
    let gs = unsafe { GlobalMemoryStatusEx(&mut g) } != 0;
    let ps = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) } != 0;
    format!(
        "system commit avail {} of {} MB, phys avail {} of {} MB, process VA avail {} of {} MB{} | process private {} MB (peak pagefile {} MB), working set {} MB{}",
        mb(g.avail_page), mb(g.total_page), mb(g.avail_phys), mb(g.total_phys), mb(g.avail_virt), mb(g.total_virt), if gs { "" } else { " (GlobalMemoryStatusEx failed)" },
        mb(c.private as u64), mb(c.peak_pagefile as u64), mb(c.ws as u64), if ps { "" } else { " (GetProcessMemoryInfo failed)" })
}

/// ID3D12Device::CreateHeap (slot 28). Logs the desc and result; Xbox-only heap flag bits
/// are masked off (PC accepts only the low D3D12_HEAP_FLAGS bits).
unsafe extern "system" fn create_heap(
    this: *mut DeviceWrapper, pdesc: *const u8, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let _ev = crate::frame_ev::begin(crate::frame_ev::HEAP);
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *const u8, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(28)) };
    if pdesc.is_null() {
        return unsafe { f(real, pdesc, riid, ppv) };
    }
    let x = unsafe { core::slice::from_raw_parts(pdesc, crate::device_console_fix::HEAP_DESC_SIZE) };
    let u32at = |d: &[u8], o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let u64at = |d: &[u8], o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
    let (size, htype, align, flags) = (u64at(x, 0), u32at(x, 8), u64at(x, 0x20), u32at(x, 0x28));
    let d = match crate::device_console_fix::heap_desc_for_pc(x) {
        Ok(d) => d,
        Err(e) => {
            if !ppv.is_null() {
                unsafe { *ppv = core::ptr::null_mut() };
            }
            record("d3d12_xs", "CreateHeap", &format!("size={size:#x} type={htype} flags={flags:#x}: ALLOW_DISPLAY is rejected by the console driver -> hr={e:#010x}"));
            return e;
        }
    };
    let hr = unsafe { f(real, d.as_ptr(), riid, ppv) };
    if hr >= 0 && !ppv.is_null() {
        unsafe { crate::title_gpu::heap(*ppv, size) };
    }
    let note = format!(" | pc size={:#x} align={:#x} flags={:#x}", u64at(&d, 0), u64at(&d, 0x20), u32at(&d, 0x28));
    if hr >= 0 && crate::rtcap::enabled() && !ppv.is_null() {
        crate::bindtrace::note_heap_type(unsafe { *ppv } as usize, htype);
    }
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    if N.fetch_add(1, Ordering::Relaxed) < 8 || hr < 0 {
        record("d3d12_xs", "CreateHeap", &format!(
            "size={size:#x} type={htype} cpuPage={} pool={} align={align:#x} flags={flags:#x} -> hr={hr:#010x}{note}",
            u32at(&d, 0xc), u32at(&d, 0x10)));
    }
    hr
}

/// Describe a D3D12_RESOURCE_DESC for diagnostics.
fn desc_str(d: &[u8]) -> String {
    let u32at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let u16at = |o: usize| u16::from_le_bytes(d[o..o + 2].try_into().unwrap());
    let u64at = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
    format!("dim={} align={:#x} {}x{}x{} mips={} fmt={} samples={} layout={} flags={:#x}",
        u32at(0), u64at(8), u64at(0x10), u32at(0x18), u16at(0x1c), u16at(0x1e), u32at(0x20), u32at(0x24), u32at(0x2c), u32at(0x30))
}

/// ID3D12Device::CreatePlacedResource (slot 29): (pHeap, HeapOffset, pDesc, InitialState,
/// pClearValue, riid, ppv). Same Xbox→PC desc fixups as committed resources.
pub(crate) fn resource_riid(riid: *const Guid) -> bool {
    !riid.is_null() && unsafe { &*riid } == &IID_ID3D12RESOURCE
}

unsafe extern "system" fn create_placed_resource(
    this: *mut DeviceWrapper, heap: *mut c_void, offset: u64, pdesc: *const u8,
    init_state: u32, pclear: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let _ev = crate::frame_ev::begin(crate::frame_ev::PLACED);
    let real = unsafe { (*this).real };
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u64, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(29)) };
    if !resource_riid(riid) {
        record("d3d12_xs", "CreatePlacedResource", "riid is not IID_ID3D12Resource -> E_NOINTERFACE, nothing created (xgs12 0x4765f / xgs12_10 0x4d6be)");
        return E_NOINTERFACE;
    }
    if heap.is_null() {
        return unsafe { create_placed_resource_x(this, offset, pdesc, init_state, pclear, riid, ppv) };
    }
    if pdesc.is_null() {
        return unsafe { f(real, heap, offset, pdesc, init_state, pclear, riid, ppv) };
    }
    let mut d = unsafe { core::slice::from_raw_parts(pdesc, 0x38).to_vec() };
    let flags = u32::from_le_bytes(d[0x30..0x34].try_into().unwrap());
    d[0x30..0x34].copy_from_slice(&(flags & 0xFF).to_le_bytes());
    let init_state = crate::xone_barrier::initial_state(u32::from_le_bytes(d[0..4].try_into().unwrap()), init_state);
    let da: [u8; 0x38] = d[..0x38].try_into().unwrap();
    let mut hr = match (d[0], ppv.is_null()) {
        (1, false) => unsafe { crate::placed_offset::place_in_heap(real, heap, offset, &da, init_state, pclear, ppv) },
        _ => None,
    }
    .unwrap_or_else(|| unsafe { f(real, heap, offset, d.as_ptr(), init_state, pclear, &IID_ID3D12RESOURCE, ppv) });
    if hr < 0 && d[0] >= 2 && offset % 0x1000 == 0 && offset % 0x1_0000 != 0 && u64::from_le_bytes(d[8..16].try_into().unwrap()) == 0 {
        let mut small = d.clone();
        small[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
        hr = unsafe { f(real, heap, offset, small.as_ptr(), init_state, pclear, &IID_ID3D12RESOURCE, ppv) };
        if hr >= 0 {
            d = small;
        }
    }
    if hr < 0 && d[0] >= 2 {
        let da: [u8; 0x38] = d[..0x38].try_into().unwrap();
        if let Some(h) = unsafe { crate::alias::committed_when_too_big(real, heap, offset, &da, init_state, pclear, ppv) } {
            hr = h;
        }
    }
    if hr >= 0 && !ppv.is_null() {
        crate::map_x::ensure_patched(unsafe { *ppv });
        unsafe { crate::gpu_va::note_resource(real, *ppv, &d, init_state, None) };
        crate::res3d::note_create(unsafe { *ppv }, &d, &format!("placed heap={heap:p}+{offset:#x} state={init_state:#x}"));
    }
    if hr >= 0 && crate::rtcap::enabled() && d[0] == 1 && !ppv.is_null() {
        let width = u64::from_le_bytes(d[0x10..0x18].try_into().unwrap());
        unsafe { crate::bindtrace::note_buffer(*ppv, width, crate::bindtrace::heap_type_of(heap as usize)) };
    }
    if hr >= 0 && !ppv.is_null() && d[0] == 3 {
        unsafe { crate::rtcap::register(*ppv, &d[..0x38], init_state) };
    }
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    if hr < 0 || N.fetch_add(1, Ordering::Relaxed) < 4 {
        let orig = unsafe { core::slice::from_raw_parts(pdesc, 0x38) };
        record("d3d12_xs", "CreatePlacedResource", &format!(
            "heap={heap:p} off={offset:#x} state={init_state:#x} {} -> hr={hr:#010x}", desc_str(orig)));
    }
    hr
}

/// Xbox slot 44 `CreatePlacedResourceX` (see placed.rs for evidence and model).
pub(crate) unsafe extern "system" fn create_placed_resource_x(
    this: *mut DeviceWrapper, va: u64, pdesc: *const u8, init_state: u32,
    pclear: *const c_void, riid: *const Guid, ppv: *mut *mut c_void,
) -> i32 {
    let _ev = crate::frame_ev::begin(crate::frame_ev::PLACED);
    let real = unsafe { (*this).real };
    if !resource_riid(riid) {
        record("d3d12_xs", "CreatePlacedResourceX", "riid is not IID_ID3D12Resource -> E_NOINTERFACE, nothing created (xgs12 0x477cb / xgs12_10 0x4d85e)");
        return E_NOINTERFACE;
    }
    if pdesc.is_null() || ppv.is_null() {
        record("d3d12_xs", "CreatePlacedResourceX", "null desc/out -> E_INVALIDARG");
        return 0x80070057u32 as i32;
    }
    let orig: [u8; 0x38] = unsafe { core::slice::from_raw_parts(pdesc, 0x38) }.try_into().unwrap();
    let mut d = orig;
    let dim = u32::from_le_bytes(d[0..4].try_into().unwrap());
    let init_state = crate::xone_barrier::initial_state(dim, init_state);
    let flags = u32::from_le_bytes(d[0x30..0x34].try_into().unwrap());
    d[8..16].copy_from_slice(&0u64.to_le_bytes()); // Alignment: let the host choose
    d[0x2c..0x30].copy_from_slice(&(if dim == 1 { 1u32 } else { 0u32 }).to_le_bytes()); // Layout: ROW_MAJOR for buffers, UNKNOWN for textures (Xbox tile modes have no PC meaning)
    d[0x30..0x34].copy_from_slice(&(flags & 0xFF).to_le_bytes());
    let heap: [u32; 5] = [1 /* DEFAULT */, 0, 0, 1, 1];
    let f: unsafe extern "system" fn(*mut c_void, *const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32 =
        unsafe { core::mem::transmute(*real_vtbl(real).add(27)) };
    let xbox_size = if dim >= 2 { title_layout_size(&orig) } else { crate::gpu_va::xbox_texture_size(&orig) };
    let made = unsafe { crate::placed_offset::create(real, va, &d, xbox_size, init_state, pclear, ppv) }
        .or_else(|| unsafe { crate::alias::create_placed_x(real, va, &d, xbox_size, init_state, pclear, ppv) });
    let hr = match &made {
        Some(m) => m.hr,
        None => unsafe { f(real, heap.as_ptr(), 0, d.as_ptr(), init_state, pclear, &IID_ID3D12RESOURCE, ppv) },
    };
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    if hr < 0 || n < 12 || (crate::passcap::tables_on() && n < 20000) {
        let how = match &made {
            Some(m) => format!("aliased {:?} heap {:#x}+{:#x}", m.kind, m.heap, m.offset),
            None => "separate committed resource".to_string(),
        };
        record("d3d12_xs", "CreatePlacedResourceX", &format!(
            "#{n} va={va:#x} state={init_state:#x} clear={pclear:p} {} -> {how} hr={hr:#010x} res={:p}",
            desc_str(&orig), unsafe { *ppv }));
    }
    if hr >= 0 {
        // One reference for the placement registry (released by upload.rs).
        let res = unsafe { *ppv };
        crate::map_x::ensure_patched(res);
        let addref: unsafe extern "system" fn(*mut c_void) -> u32 = unsafe { core::mem::transmute(*real_vtbl(res).add(1)) };
        unsafe { addref(res) };
        unsafe { crate::gpu_va::note_resource(real, res, &d, init_state, Some(va)) };
        if crate::va_census::on() {
            crate::va_census::set_len(res as usize, xbox_size);
            crate::va_census::note_how(res as usize, match &made { Some(m) => format!("CreatePlacedResourceX aliased {:?} heap {:#x}+{:#x}", m.kind, m.heap, m.offset), None => "CreatePlacedResourceX separate committed".to_string() });
        }
        crate::placed::register(res, crate::placed::Placed { va, desc: orig, init_state });
        crate::res3d::note_create(res, &orig, &format!("placedX va={va:#x} {}", match &made { Some(m) => format!("aliased {:?} heap {:#x}+{:#x}", m.kind, m.heap, m.offset), None => "separate committed".to_string() }));
        if flags & 0x7 != 0 {
            unsafe { crate::rtcap::register(res, &d[..0x38], init_state) };
            static RT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            if RT.fetch_add(1, Ordering::Relaxed) < 300 {
                record("d3d12_xs", "CreatePlacedResourceX", &format!(
                    "rt/ds/uav placement va={va:#x} state={init_state:#x} {} res={res:p}", desc_str(&orig)));
            }
        }
    }
    hr
}

/// Xbox slot 61: extended sampler create, `(this, const XboxSamplerDesc*, D3D12_CPU_DESCRIPTOR_HANDLE)`.
///
/// Evidence: `FD3D12Device::CreateSamplerInternal` (EXE+0xd2c908). For every filter except
/// `D3D12_FILTER_ANISOTROPIC` (0x55) it calls the standard `CreateSampler` (slot 22). For
/// anisotropic it zero-fills a 0x6C-byte block and copies the standard desc into it:
///   +0x18/+0x1C/+0x20 AddressU/V/W, +0x24 MinLOD, +0x28 MaxLOD, +0x2C MipLODBias,
///   +0x34 MaxAnisotropy, +0x40 ComparisonFunc
/// plus constants at +0x04/+0x08 (3), +0x0C (2) and two console-variable clamps at +0x3C
/// (<= 7) and +0x54 (<= 15) whose meaning is unverified (they look like per-axis anisotropy
/// limits). The border colour is left zero.
///
unsafe extern "system" fn create_sampler_x(this: *mut DeviceWrapper, xdesc: *const u8, dest: usize) {
    let real = unsafe { (*this).real };
    let dflt = crate::device_console_fix::default_xsamp();
    let x: &[u8] = if xdesc.is_null() { &dflt } else { unsafe { core::slice::from_raw_parts(xdesc, crate::device_console_fix::XSAMP_SIZE) } };
    let d = crate::device_console_fix::sampler_desc_from_xbox(x);
    let f: unsafe extern "system" fn(*mut c_void, *const u8, usize) = unsafe { core::mem::transmute(*real_vtbl(real).add(22)) };
    unsafe { f(real, d.as_ptr(), dest) };
    let extra = crate::device_console_fix::sampler_unmapped_fields(x);
    static N: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
    if N.fetch_add(1, Ordering::Relaxed) < 4 || (!extra.is_empty() && N.load(Ordering::Relaxed) < 64) {
        let u = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        record("d3d12_xs", "CreateSamplerX", &format!(
            "filter={:#x} addr={}/{}/{} maxAniso={} cmp={} dest={dest:#x} xbox-only fields with no D3D12 PC equivalent: {extra:x?}",
            u(0), u(4), u(8), u(0xc), u(0x14), u(0x18)));
    }
}

// Per-slot logging stubs for the Xbox device extensions (44+): capture slot +
// caller RVA and return S_OK (most are hints like SetFrameIntervalX that the RHI
// only checks for success). This reveals which Xbox device methods the game needs.
static DEVSTUB_EXE_BASE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static DEVSTUB_HITS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
unsafe extern "system" fn dev_stub_impl(_this: *mut c_void, _a: u64, caller: usize, slot: u64) -> i32 {
    static PER_SLOT: [core::sync::atomic::AtomicUsize; 160] = [const { core::sync::atomic::AtomicUsize::new(0) }; 160];
    DEVSTUB_HITS.fetch_add(1, Ordering::Relaxed);
    if PER_SLOT[(slot as usize).min(159)].fetch_add(1, Ordering::Relaxed) < 3 {
        if DEVSTUB_EXE_BASE.load(Ordering::Relaxed) == 0 {
            extern "system" { fn GetModuleHandleW(n: *const u16) -> *mut c_void; }
            DEVSTUB_EXE_BASE.store(unsafe { GetModuleHandleW(core::ptr::null()) } as usize, Ordering::Relaxed);
        }
        let base = DEVSTUB_EXE_BASE.load(Ordering::Relaxed);
        record("d3d12_xs", "dev_ext", &format!("slot={slot} caller={:#x} -> S_OK", caller.wrapping_sub(base)));
    }
    S_OK
}
macro_rules! dev_stubs {
    ($(($name:ident, $slot:literal)),* $(,)?) => {
        $(
            #[unsafe(naked)]
            unsafe extern "system" fn $name() -> i32 {
                core::arch::naked_asm!(
                    "mov r8, [rsp]", "mov r9d, {s}", "jmp {i}",
                    s = const $slot, i = sym dev_stub_impl,
                );
            }
        )*
        unsafe fn install_dev_stubs(v: &mut [usize]) {
            $( v[$slot] = $name as usize; )*
        }
    };
}
dev_stubs!(
    (d44,44),(d45,45),(d46,46),(d47,47),(d49,49),(d50,50),(d51,51),(d52,52),(d53,53),(d54,54),
    (d55,55),(d56,56),(d57,57),(d58,58),(d59,59),(d60,60),(d61,61),(d62,62),(d63,63),(d64,64),
    (d65,65),(d66,66),(d67,67),(d68,68),(d69,69),(d70,70),(d71,71),(d72,72),(d74,74),(d75,75),
    (d76,76),(d77,77),(d78,78),(d79,79),(d80,80),(d81,81),(d82,82),(d83,83),(d84,84),(d85,85),
    (d86,86),(d87,87),(d88,88),(d89,89),(d90,90),
);

pub(crate) fn vtable() -> *const usize {
    static VT: OnceLock<usize> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut v: Vec<usize> = vec![
            qi as usize, addref as usize, release as usize,
            f3 as usize, f4 as usize, f5 as usize, f6 as usize, f7 as usize, f8 as usize,
            f9 as usize, f10 as usize, f11 as usize, f12 as usize,
            check_feature_support as usize,   // 13
            create_descriptor_heap as usize,  // 14
            f15 as usize, f16 as usize, f17 as usize, f18 as usize, f19 as usize, f20 as usize,
            f21 as usize, f22 as usize, f23 as usize, f24 as usize, f25 as usize, f26 as usize,
            f27 as usize, f28 as usize, f29 as usize, f30 as usize, f31 as usize, f32 as usize,
            f33 as usize, f34 as usize, f35 as usize, f36 as usize, f37 as usize, f38 as usize,
            f39 as usize, f40 as usize, f41 as usize, f42 as usize, f43 as usize,
        ];
        // 44..160: Xbox-extension slots — fail-loud stub unless intercepted below.
        while v.len() < 160 {
            v.push(dev_stub as usize);
        }
        unsafe { install_dev_stubs(&mut v); } // 44..90: logging S_OK stubs (diagnostic)
        let console_defaults = crate::console_slots::snapshot_slice(&v);
        // Frame pacing (see frame.rs).
        v[78] = crate::frame::set_frame_interval_x as usize;
        v[79] = crate::frame::schedule_frame_event_x as usize;
        v[80] = crate::frame::wait_frame_event_x as usize;
        v[81] = crate::frame::frame_statistics_x as usize;
        v[44] = create_placed_resource_x as usize; // CreatePlacedResourceX
        v[61] = create_sampler_x as usize; // Xbox extended (anisotropic) sampler create
        v[60] = create_gfx_pso_x as usize; // Xbox graphics PSO create -> PC CreateGraphicsPipelineState
        v[55] = create_derived_pso_x as usize; // Xbox derived-PSO create (base PSO + state patches)
        v[62] = create_compute_pso_x as usize; // Xbox compute PSO create
        v[10] = create_gfx_pso_std as usize;
        v[11] = create_compute_pso_std as usize;
        v[28] = create_heap as usize; // CreateHeap (logged; Xbox-only flag bits masked on retry)
        v[29] = create_placed_resource as usize; // CreatePlacedResource (desc flag fixups)
        v[48] = create_command_queue_x as usize; // CreateCommandQueueX
        v[9] = create_command_allocator_x as usize;
        v[8] = create_command_queue_x as usize; // PC-slot CreateCommandQueue: same desc; needs the Xbox-sized queue vtable too
        v[15] = get_descriptor_increment as usize; // GetDescriptorHandleIncrementSize
        v[25] = get_resource_alloc_info as usize; // GetResourceAllocationInfo (null-safe)
        v[17] = create_cbv as usize; // CreateConstantBufferView (256-align size)
        v[27] = create_committed_resource as usize; // CreateCommittedResource (IID subst)
        v[73] = create_command_list_x as usize;  // Xbox command-list create
        v[38] = crate::device_x::get_copyable_footprints_x as usize;
        v[41] = crate::device_x::create_command_signature_x as usize;
        v[52] = crate::device_x::set_debug_flags_x as usize;
        v[56] = crate::device_x::serialize_pso_release_x as usize;
        v[57] = crate::device_x::deserialize_gfx_pso_x as usize;
        v[66] = crate::device_x::serialize_pso_release_x as usize;
        v[67] = crate::device_x::deserialize_compute_pso_x as usize;
        v[74] = crate::device_x::register_page_pool_x as usize;
        v[75] = crate::device_x::unregister_page_pool_x as usize;
        v[87] = crate::device_x::device87_release_x as usize;
        crate::device_ext_a::install(&mut v);
        if crate::rtcap::enabled() || crate::passcap::on() || crate::blendclamp::lazy() {
            v[20] = create_rtv_traced as usize;
            v[21] = create_dsv_traced as usize;
        }
        crate::dbcount::install_device(&mut v);
        crate::console_slots_dev2::install(&mut v);
        crate::perfctr::install_device(&mut v);
        crate::placed_offset::install(&mut v);
        crate::console_slots::install_device(&mut v, &console_defaults);
        crate::cmdlist_validate::hook_device(&mut v);
        crate::title_heap::hook_device(&mut v);
        crate::console_slots_mesh::install_device(&mut v);
        crate::depth_uav::hook_device(&mut v);
        crate::ecl_probe::install_device(&mut v);
        crate::cbv_raw::hook_device(&mut v);
        crate::pso_lib::hook_device(&mut v);
        crate::cl_edition::install_device(&mut v);
        Box::leak(v.into_boxed_slice()).as_ptr() as usize
    }) as *const usize
}

/// Wrap a real `ID3D12Device` so we can intercept QueryInterface.
pub fn wrap_device(real: *mut c_void) -> *mut c_void {
    if !real.is_null() {
        unsafe { crate::shader_xlat::probe_typed_atomic64(real) };
        crate::blend_probe::note_device(real);
    }
    if (crate::rtcap::enabled() || crate::passcap::tables_on()) && !real.is_null() {
        unsafe { patch_real_device(real) };
    }
    let w = Box::new(DeviceWrapper { vtable: vtable(), real, refs: AtomicU32::new(1) });
    let p = Box::into_raw(w) as *mut c_void;
    crate::pso_lib::warm(real, p);
    WRAPPED.lock().unwrap_or_else(|e| e.into_inner()).push((real as usize, p as usize));
    p
}

static WRAPPED: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn is_wrapper(p: *mut c_void) -> bool {
    WRAPPED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|e| e.1 == p as usize)
}

pub(crate) fn wrapper_qi(real: *mut c_void, riid: *const Guid, ppv: *mut *mut c_void) -> Option<i32> {
    let g = WRAPPED.lock().unwrap_or_else(|e| e.into_inner());
    let w = g.iter().rev().find(|e| e.0 == real as usize)?.1;
    Some(unsafe { qi(w as *mut DeviceWrapper, riid, ppv) })
}

fn unregister_wrapper(w: usize) {
    WRAPPED.lock().unwrap_or_else(|e| e.into_inner()).retain(|e| e.1 != w);
}

#[cfg(test)]
mod xsx_device8_tests {
    use super::*;

    #[test]
    fn qi_answers_the_scarlett_device8_iid_with_the_wrapper() {
        let mut w = DeviceWrapper { vtable: core::ptr::null(), real: core::ptr::null_mut(), refs: AtomicU32::new(1) };
        let mut out: *mut c_void = core::ptr::null_mut();
        let hr = unsafe { qi(&mut w, &IID_XSX_DEVICE8, &mut out) };
        assert_eq!(hr, S_OK);
        assert_eq!(out, &mut w as *mut DeviceWrapper as *mut c_void);
        assert_eq!(w.refs.load(Ordering::SeqCst), 2);
        assert_eq!(guid_str(&IID_XSX_DEVICE8), "{728f1fa2-ea11-4a9f-9b52-9b42a484d5c9}");
    }
}
