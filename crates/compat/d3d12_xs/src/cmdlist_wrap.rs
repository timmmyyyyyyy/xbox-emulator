//! Xbox D3D12.x `ID3D12GraphicsCommandList` vtable translation.
//!
//! Reverse-engineered from the Fortnite RHI call sites, the Xbox command-list
//! vtable differs from PC by **4 methods inserted after PC slot 27**
//! (ExecuteBundle). Confirmed slot mapping (Xbox slot -> PC method):
//!   * 0..=26  identical  (IUnknown, Close=9, Reset=10, DrawInstanced=12,
//!     DrawIndexedInstanced=13, Dispatch=14, SetPipelineState=25,
//!     ResourceBarrier=26 — all verified unshifted at their call sites)
//!   * 27..=31 ExecuteBundle (PC27) + 4 Xbox-specific inserted methods
//!   * 32..    PC method (slot - 4): SetDescriptorHeaps(PC28)=Xbox32,
//!     SetGraphicsRootSignature(PC30)=Xbox34, SetGraphicsRootDescriptorTable
//!     (PC32)=Xbox36, OMSetRenderTargets(PC46)=Xbox50 — all verified.
//!
//! Fix: build an "Xbox-layout" vtable whose slot i holds the *real PC method
//! pointer* for the mapped PC slot, then point each real command-list object at
//! it. Because the object IS the real list, no forwarding thunk is needed — the
//! real method is called with the real `this` directly. Slots 0..=26 stay
//! identical so the D3D12 runtime's own calls (QueryInterface, GetType, AddRef,
//! Release) still work when the list is handed to ExecuteCommandLists.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[allow(unused_imports)] use core::sync::atomic::AtomicU64;
use compat_common::record;

extern "system" {
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
}
const PAGE_READWRITE: u32 = 0x04;

/// Number of Xbox vtable slots we materialize. Max observed call is Xbox slot 50
/// (OMSetRenderTargets); 128 leaves generous headroom.
const N: usize = 256;
static XBOX_VT: [AtomicUsize; N] = [const { AtomicUsize::new(0) }; N];
static BUILT: AtomicBool = AtomicBool::new(false);
static mut REAL_VT_PTR: *const usize = core::ptr::null();
pub(crate) unsafe fn real_list_slot(slot: usize) -> usize {
    let p = unsafe { REAL_VT_PTR };
    if p.is_null() { 0 } else { unsafe { *p.add(slot) } }
}
static INSERTED_HITS: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn inserted_stub_addr() -> usize {
    inserted_stub as usize
}
pub(crate) const CONSOLE_NULL_SLOT: usize = 66;

pub(crate) unsafe fn real_slot_ptr(slot: usize) -> *mut usize {
    let p = unsafe { REAL_VT_PTR };
    if p.is_null() { core::ptr::null_mut() } else { unsafe { p.add(slot) as *mut usize } }
}

pub(crate) unsafe fn real_method(slot: usize) -> usize {
    unsafe { *REAL_VT_PTR.add(slot) }
}

#[track_caller]
pub(crate) unsafe fn pc_method(list: *mut c_void, slot: usize) -> usize {
    let vt = unsafe { *(list as *const *const usize) };
    let xbox = XBOX_VT.as_ptr() as *const usize;
    if vt == xbox && !unsafe { REAL_VT_PTR }.is_null() {
        if let Some(t) = crate::rootlog::route(slot, core::panic::Location::caller().file()) {
            return t;
        }
        unsafe { *REAL_VT_PTR.add(slot) }
    } else {
        unsafe { *vt.add(slot) }
    }
}

pub(crate) fn vt_get(i: usize) -> usize {
    XBOX_VT[i].load(Ordering::Relaxed)
}

pub(crate) fn vt_set(i: usize, f: usize) {
    XBOX_VT[i].store(f, Ordering::Relaxed);
}

pub(crate) fn map_slot(i: usize) -> Option<usize> {
    match i {
        0..=27 => Some(i),
        28..=31 => Some(i + 24),
        32..=55 => Some(i - 4),
        56..=59 => Some(i),
        _ => None,
    }
}

/// Fail-loud-ish stub for an Xbox-inserted slot of unknown signature. Returns 0
/// in rax (valid for both void- and HRESULT(S_OK)-returning methods) so a stray
/// call doesn't corrupt state; logs the first few so we learn if the game needs one.
#[unsafe(naked)]
unsafe extern "system" fn inserted_stub() {
    core::arch::naked_asm!(
        "lea rax, [rip + {counter}]",
        "lock inc qword ptr [rax]",
        "xor eax, eax",
        "ret",
        counter = sym INSERTED_HITS,
    )
}

// High-slot diagnostic: the +4 shift is only valid through PC slot 46 (Xbox 50).
// Past that the Xbox command list has its own extension methods, so our naive map
// sends calls to the wrong PC method (e.g. a texture-copy landed on DispatchMesh ->
// device removal). Install logging no-op probes across the high range to capture,
// for every high slot the game actually calls, the slot + first arg + caller RVA,
// so we can reconstruct the true mapping. No-op avoids recording invalid GPU work.
static EXE_BASE: AtomicUsize = AtomicUsize::new(0);
static PROBE_HITS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn probe_impl(_this: *mut c_void, a: u64, caller: usize, slot: u64) {
    let h = PROBE_HITS.fetch_add(1, Ordering::Relaxed);
    static PER: [AtomicUsize; 128] = [const { AtomicUsize::new(0) }; 128];
    let _ = h;
    if PER[(slot as usize).min(127)].fetch_add(1, Ordering::Relaxed) < 4 {
        let base = EXE_BASE.load(Ordering::Relaxed);
        record("d3d12_xs", "cmdlist.hi",
            &format!("slot={slot} arg1={a:#x} caller={:#x}", caller.wrapping_sub(base)));
    }
}

macro_rules! make_probes {
    ($(($name:ident, $slot:literal)),* $(,)?) => {
        $(
            #[unsafe(naked)]
            unsafe extern "system" fn $name() {
                core::arch::naked_asm!(
                    "mov r8, [rsp]",        // caller return address -> 3rd arg
                    "mov r9d, {s}",         // slot -> 4th arg
                    "jmp {i}",
                    s = const $slot, i = sym probe_impl,
                );
            }
        )*
        fn is_probe(p: usize) -> bool {
            false $( || p == $name as usize )*
        }
        #[allow(dead_code)]
        unsafe fn install_all_probes() {
            $( XBOX_VT[$slot].store($name as usize, Ordering::Relaxed); )*
        }
        /// The high-slot scan showed the game only calls slot 83 (an Xbox texture-copy
        /// command our +4 map misrouted to DispatchMesh). No-op just that slot for now.
        unsafe fn install_probes() {
            // The +4 shift is verified only through Xbox slot 50. Everything above is an
            // unverified/Xbox-only method: log-and-ignore until mapped from evidence
            // (e.g. slot 67 = FlushTextureCache-style flush, slot 83 = texture copy).
            install_all_probes();
            for i in 47..=49 { XBOX_VT[i].store(*REAL_VT_PTR.add(i - 4), Ordering::Relaxed); }
            // Verified from FD3D12CommandContext::RHIClearMRTImpl call sites:
            // Xbox 51 = ClearDepthStencilView (PC 47), Xbox 52 = ClearRenderTargetView (PC 48).
            for i in 51..=52 { XBOX_VT[i].store(*REAL_VT_PTR.add(i - 4), Ordering::Relaxed); }
            for i in 53..=58 {
                if let Some(pc) = map_slot(i) {
                    XBOX_VT[i].store(*REAL_VT_PTR.add(pc), Ordering::Relaxed);
                }
            }
            XBOX_VT[84].store(copy_resource_x as usize, Ordering::Relaxed);
            XBOX_VT[67].store(flush_texture_cache_x as usize, Ordering::Relaxed);
            // Xbox 68 = OMSetDepthBounds (PC ID3D12GraphicsCommandList1 slot 62): called from
            // FD3D12StateCacheBase::ApplyState (EXE+0x4e0480) with the two cached depth-bound
            // floats in xmm1/xmm2 when its depth-bounds dirty flag is set.
            XBOX_VT[68].store(*REAL_VT_PTR.add(62), Ordering::Relaxed);
            XBOX_VT[83].store(copy_texture_region_x as usize, Ordering::Relaxed);
            XBOX_VT[16].store(copy_texture_region_owned as usize, Ordering::Relaxed);
            XBOX_VT[17].store(copy_resource_owned as usize, Ordering::Relaxed);
            // Xbox 59 = ExecuteIndirect, same slot and argument order as PC:
            // FD3D12CommandContext::RHIDispatchIndirectComputeShader (EXE+0x85145b) calls it as
            // (pCommandSignature, 1, pArgumentBuffer, ArgumentBufferOffset, null, 0). Leaving it
            // a no-op silently dropped every indirect dispatch/draw.
            XBOX_VT[59].store(*REAL_VT_PTR.add(59), Ordering::Relaxed);
            if crate::rtcap::enabled() {
                XBOX_VT[26].store(resource_barrier_traced as usize, Ordering::Relaxed);
                XBOX_VT[25].store(set_pso_traced as usize, Ordering::Relaxed);
                XBOX_VT[50].store(om_set_targets_traced as usize, Ordering::Relaxed);
                XBOX_VT[12].store(draw_traced as usize, Ordering::Relaxed);
                XBOX_VT[13].store(draw_indexed_traced as usize, Ordering::Relaxed);
                XBOX_VT[14].store(dispatch_traced as usize, Ordering::Relaxed);
                XBOX_VT[59].store(indirect_traced as usize, Ordering::Relaxed);
                XBOX_VT[48].store(ia_set_vbs_traced as usize, Ordering::Relaxed);
                XBOX_VT[35].store(set_compute_table_traced as usize, Ordering::Relaxed);
                XBOX_VT[36].store(set_gfx_table_traced as usize, Ordering::Relaxed);
            }
            crate::blendclamp::install();
            crate::gpuprof::install();
        }
    };
}
make_probes!(
    (p47,47),(p48,48),(p49,49),(p51,51),(p52,52),(p53,53),(p54,54),(p55,55),(p56,56),(p57,57),
    (p58,58),(p59,59),(p60,60),(p61,61),(p62,62),(p63,63),(p64,64),(p65,65),(p66,66),(p67,67),
    (p68,68),(p69,69),(p70,70),(p71,71),(p72,72),(p73,73),(p74,74),(p75,75),(p76,76),(p77,77),
    (p78,78),(p79,79),(p80,80),(p81,81),(p82,82),(p83,83),(p84,84),(p85,85),(p86,86),(p87,87),
    (p88,88),(p89,89),(p90,90),(p91,91),(p92,92),(p93,93),(p94,94),(p95,95),(p96,96),(p97,97),
    (p98,98),(p99,99),(p100,100),(p101,101),(p102,102),(p103,103),(p104,104),(p105,105),(p106,106),
    (p107,107),(p108,108),(p109,109),(p110,110),(p111,111),(p112,112),(p113,113),(p114,114),(p115,115),
    (p116,116),(p117,117),(p118,118),(p119,119),(p120,120),(p121,121),(p122,122),(p123,123),(p124,124),
    (p125,125),(p126,126),(p127,127),
);

/// ResourceBarrier (slot 26, identical on Xbox and PC) with state tracking for the
/// diagnostic render-target capture; installed only when the capture is enabled.
unsafe extern "system" fn resource_barrier_traced(this: *mut c_void, n: u32, barriers: *const u8) {
    crate::rtcap::note_barriers(n, barriers);
    let f: unsafe extern "system" fn(*mut c_void, u32, *const u8) = core::mem::transmute(*REAL_VT_PTR.add(26));
    f(this, n, barriers)
}

// Draw-trace hooks (installed only with the diagnostic capture): note, then forward.
unsafe extern "system" fn set_pso_traced(this: *mut c_void, pso: *mut c_void) {
    crate::rtcap::on_set_pso(this as usize, pso as usize);
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void) = core::mem::transmute(*REAL_VT_PTR.add(25));
    f(this, pso)
}
unsafe extern "system" fn om_set_targets_traced(this: *mut c_void, n: u32, handles: *const usize, single: i32, dsv: *const usize) {
    crate::rtcap::on_set_targets(this as usize, n, handles, single != 0, dsv);
    let f: unsafe extern "system" fn(*mut c_void, u32, *const usize, i32, *const usize) = core::mem::transmute(*REAL_VT_PTR.add(46));
    f(this, n, handles, single, dsv)
}
/// SetCompute/GraphicsRootDescriptorTable (PC 31/32 = Xbox 35/36): (RootParameterIndex,
/// D3D12_GPU_DESCRIPTOR_HANDLE by value). Recorded for the binding trace.
unsafe extern "system" fn set_compute_table_traced(this: *mut c_void, index: u32, handle: u64) {
    crate::bindtrace::set_table(this as usize, true, index, handle);
    let f: unsafe extern "system" fn(*mut c_void, u32, u64) = core::mem::transmute(*REAL_VT_PTR.add(31));
    f(this, index, handle)
}
unsafe extern "system" fn set_gfx_table_traced(this: *mut c_void, index: u32, handle: u64) {
    crate::bindtrace::set_table(this as usize, false, index, handle);
    let f: unsafe extern "system" fn(*mut c_void, u32, u64) = core::mem::transmute(*REAL_VT_PTR.add(32));
    f(this, index, handle)
}
unsafe extern "system" fn draw_traced(this: *mut c_void, a: u32, b: u32, c: u32, d: u32) {
    crate::rtcap::trace_draw(this as usize, false, &format!("DrawInstanced(vtx={a} inst={b} startVtx={c} startInst={d})"));
    if !crate::rtcap::on_draw(this as usize, 0) {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32) = core::mem::transmute(*REAL_VT_PTR.add(12));
    f(this, a, b, c, d)
}
/// IASetVertexBuffers (PC 44 = Xbox 48): (StartSlot, NumViews, pViews); views are 16 bytes.
unsafe extern "system" fn ia_set_vbs_traced(this: *mut c_void, start: u32, n: u32, views: *const u8) {
    crate::rtcap::on_set_vbs(this as usize, start, n, views);
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, *const u8) = core::mem::transmute(*REAL_VT_PTR.add(44));
    f(this, start, n, views)
}
unsafe extern "system" fn draw_indexed_traced(this: *mut c_void, a: u32, b: u32, c: u32, d: i32, e: u32) {
    crate::rtcap::trace_draw(this as usize, false, &format!("DrawIndexedInstanced(idx={a} inst={b} startIdx={c} baseVtx={d} startInst={e})"));
    if !crate::rtcap::on_draw(this as usize, 1) {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32, i32, u32) = core::mem::transmute(*REAL_VT_PTR.add(13));
    f(this, a, b, c, d, e)
}
unsafe extern "system" fn dispatch_traced(this: *mut c_void, a: u32, b: u32, c: u32) {
    crate::rtcap::trace_draw(this as usize, true, &format!("Dispatch({a},{b},{c})"));
    if !crate::rtcap::on_draw(this as usize, 2) {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void, u32, u32, u32) = core::mem::transmute(*REAL_VT_PTR.add(14));
    f(this, a, b, c)
}
unsafe extern "system" fn indirect_traced(this: *mut c_void, sig: *mut c_void, n: u32, buf: *mut c_void, off: u64, cbuf: *mut c_void, coff: u64) {
    if !crate::rtcap::on_draw(this as usize, 3) {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut c_void, u64, *mut c_void, u64) = core::mem::transmute(*REAL_VT_PTR.add(59));
    f(this, sig, n, buf, off, cbuf, coff)
}

/// Xbox slot 67: reached (by tail call) from `FD3D12CommandContext::FlushTextureCacheIfNeeded`
/// (EXE+0x4e0718). The console GPU's texture cache has no PC counterpart, so there is
/// nothing to do; this is a deliberate no-op, not a placeholder.
static FLUSH_HITS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn flush_texture_cache_x(this: *mut c_void) {
    let _ij = unsafe { crate::inject::scope(this, crate::inject::BARRIER) };
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| std::env::var("TOMBOX_FLUSH_UAV").map(|v| v != "0").unwrap_or(true));
    if FLUSH_HITS.fetch_add(1, Ordering::Relaxed) == 0 {
        record("d3d12_xs", "FlushTextureCacheX", &format!("first call; mapped to a global UAV barrier = {on}"));
    }
    if !on || this.is_null() {
        return;
    }
    let b: [u64; 4] = [2, 0, 0, 0];
    let f: unsafe extern "system" fn(*mut c_void, u32, *const [u64; 4]) = core::mem::transmute(*REAL_VT_PTR.add(26));
    f(this, 1, &b)
}

pub fn flush_hits() -> usize {
    FLUSH_HITS.load(Ordering::Relaxed)
}

unsafe extern "system" fn copy_texture_region_owned(this: *mut c_void, dst: *const u8, x: u32, y: u32, z: u32, src: *const u8, src_box: *const c_void) {
    if !dst.is_null() {
        crate::placed::note_gpu_write(core::ptr::read_unaligned(dst as *const u64) as *mut c_void);
        let d = core::ptr::read_unaligned(dst as *const u64) as usize;
        if crate::res3d::is_3d(d) {
            let s = if src.is_null() { 0 } else { core::ptr::read_unaligned(src as *const u64) as usize };
            crate::res3d::note(d, "CopyTextureRegion", &format!("at ({x},{y},{z}) src={s:#x} srcType={}", if src.is_null() { 9 } else { core::ptr::read_unaligned(src.add(8) as *const u32) }));
        }
    }
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void) =
        core::mem::transmute(*REAL_VT_PTR.add(16));
    f(this, dst, x, y, z, src, src_box)
}

unsafe extern "system" fn copy_resource_owned(this: *mut c_void, dst: *mut c_void, src: *mut c_void) {
    crate::placed::note_gpu_write(dst);
    crate::res3d::note(dst as usize, "CopyResource", &format!("src={src:p}"));
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) = core::mem::transmute(*REAL_VT_PTR.add(17));
    f(this, dst, src)
}

/// Xbox slot 83 = CopyTextureRegion with one extra trailing argument.
///
/// Evidence: `FD3D12TextureBase::UpdateTexture` (EXE+0xc7cc5f) and
/// `FD3D12RHICommandInitializeTexture::ExecuteNoCmdList` (EXE+0x13e7da9) call it as
/// (this, pDst, DstX, DstY, DstZ, pSrc, pSrcBox, 0) with 48-byte copy locations laid out as
/// on PC (resource, type, 32-byte placed footprint / subresource index). The eighth argument
/// is always 0 at both sites (probably Xbox copy flags) and is dropped.
unsafe extern "system" fn copy_texture_region_x(
    this: *mut c_void, dst: *const u8, x: u32, y: u32, z: u32, src: *const u8, src_box: *const c_void, flags: u32,
) {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let n = CALLS.fetch_add(1, Ordering::Relaxed);
    if dst.is_null() || src.is_null() {
        record("d3d12_xs", "CopyTextureRegionX", "null copy location - ignored");
        return;
    }
    let q = |p: *const u8, o: usize| core::ptr::read_unaligned(p.add(o) as *const u64);
    let d = |p: *const u8, o: usize| core::ptr::read_unaligned(p.add(o) as *const u32);
    if n < 6 || flags != 0 {
        record("d3d12_xs", "CopyTextureRegionX", &format!(
            "#{n} dst{{res={:#x} type={} sub={}}} at ({x},{y},{z}) src{{res={:#x} type={} off={:#x} fmt={} {}x{}x{} pitch={}}} box={src_box:p} flags={flags:#x}",
            q(dst, 0), d(dst, 8), d(dst, 16), q(src, 0), d(src, 8), q(src, 16), d(src, 24), d(src, 28), d(src, 32), d(src, 36), d(src, 40)));
    }
    if q(dst, 0) == 0 || q(src, 0) == 0 {
        record("d3d12_xs", "CopyTextureRegionX", "null resource in copy location - ignored");
        return;
    }
    crate::placed::note_gpu_write(q(dst, 0) as *mut c_void);
    crate::res3d::scan_grid_upload(q(dst, 0) as usize, q(src, 0) as *mut c_void, q(src, 16), d(src, 24), d(src, 28), d(src, 32), d(src, 36), d(src, 40));
    crate::res3d::note(q(dst, 0) as usize, "CopyTextureRegionX", &format!("at ({x},{y},{z}) src={:#x} type={} off={:#x} fmt={} {}x{}x{} pitch={}", q(src, 0), d(src, 8), q(src, 16), d(src, 24), d(src, 28), d(src, 32), d(src, 36), d(src, 40)));
    let f: unsafe extern "system" fn(*mut c_void, *const u8, u32, u32, u32, *const u8, *const c_void) =
        core::mem::transmute(*REAL_VT_PTR.add(16));
    f(this, dst, x, y, z, src, src_box);
}

unsafe extern "system" fn copy_resource_x(this: *mut c_void, dst: *mut c_void, src: *mut c_void, flags: u32) {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    if CALLS.fetch_add(1, Ordering::Relaxed) < 4 || flags != 0 {
        record("d3d12_xs", "CopyResourceX", &format!("dst={dst:p} src={src:p} flags={flags:#x}"));
    }
    if dst.is_null() || src.is_null() {
        return;
    }
    crate::placed::note_gpu_write(dst);
    crate::res3d::note(dst as usize, "CopyResourceX", &format!("src={src:p} flags={flags:#x}"));
    let f: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) = core::mem::transmute(*REAL_VT_PTR.add(17));
    f(this, dst, src);
}

#[cfg(test)]
mod tests {
    use super::map_slot;

    #[test]
    fn xbox_cmdlist_slot_map() {
        for i in 0..=27 { assert_eq!(map_slot(i), Some(i)); }
        assert_eq!(map_slot(28), Some(52));
        assert_eq!(map_slot(29), Some(53));
        assert_eq!(map_slot(30), Some(54));
        assert_eq!(map_slot(31), Some(55));
        assert_eq!(map_slot(32), Some(28));
        assert_eq!(map_slot(36), Some(32));
        assert_eq!(map_slot(50), Some(46));
        assert_eq!(map_slot(53), Some(49));
        assert_eq!(map_slot(55), Some(51));
        for i in 56..=59 { assert_eq!(map_slot(i), Some(i)); }
        assert_eq!(map_slot(60), None);
        assert_eq!(map_slot(83), None);
        let mut seen = std::collections::HashSet::new();
        for i in 0..=59 { assert!(seen.insert(map_slot(i).unwrap())); }
        assert_eq!(seen.len(), 60);
        assert!((0..60).all(|p| seen.contains(&p)));
    }
}

/// Point a freshly created real command list at the Xbox-layout vtable.
pub fn apply(real_list: *mut c_void) {
    if real_list.is_null() {
        return;
    }
    unsafe {
        let real_vt = *(real_list as *const *const usize);
        if !BUILT.swap(true, Ordering::SeqCst) {
            for i in 0..N {
                let p = match map_slot(i) {
                    Some(src) => *real_vt.add(src),
                    None => inserted_stub as usize,
                };
                XBOX_VT[i].store(p, Ordering::Relaxed);
            }
            extern "system" { fn GetModuleHandleW(n: *const u16) -> *mut c_void; }
            EXE_BASE.store(GetModuleHandleW(core::ptr::null()) as usize, Ordering::Relaxed);
            REAL_VT_PTR = real_vt;
            install_probes(); // high-slot logging no-op probes (47..95, except 50)
            let console_defaults = crate::console_slots::snapshot(&XBOX_VT);
            crate::cl_console_fix::install_console_bodies(&XBOX_VT);
            crate::cmdlist_x::install(&XBOX_VT, real_vt);
            crate::query_x::install(&XBOX_VT, real_vt);
            crate::bindtrace::install_root_cbv_hooks(&XBOX_VT, real_vt);
            crate::cbv_raw::install_root_cbv_hooks(&XBOX_VT, real_vt);
            crate::replay::install(&XBOX_VT);
            crate::replay_ext::install(&XBOX_VT);
            crate::hot_reload::install(&XBOX_VT);
            crate::cmd_spy::install(&XBOX_VT);
            crate::cmdlist_validate::install();
            crate::console_slots::install_cmdlist(&XBOX_VT, real_vt, &console_defaults);
            crate::console_slots_cl2::install_cmdlist(&XBOX_VT, &console_defaults);
            crate::cl2_state::install(&XBOX_VT, &console_defaults);
            crate::placed_offset::install_cmdlist(&XBOX_VT);
            crate::list_split::install(&XBOX_VT);
            crate::title_heap::install(&XBOX_VT);
            crate::console_slots_mesh::install_cmdlist(&XBOX_VT);
            crate::perfctr_cl::install(&XBOX_VT);
            crate::cl_console_fix::install_copy_memory(&XBOX_VT);
            crate::dbcount::install(&XBOX_VT);
            crate::cl_console_fix::install_family_tail(&XBOX_VT);
            crate::resolve_x::install(&XBOX_VT);
            crate::dred_marks::install(&XBOX_VT);
            crate::depth_uav::install_cmdlist(&XBOX_VT);
            crate::pso_async::install(&XBOX_VT);
            crate::cbv_raw::install_draw_check(&XBOX_VT);
            crate::passcap::install(&XBOX_VT);
            crate::gpuprof::install_ops(&XBOX_VT);
            crate::cl_edition::install_cmdlist(&XBOX_VT);
            XBOX_VT[CONSOLE_NULL_SLOT].store(0, Ordering::Relaxed);
            record("d3d12_xs", "cmdlist.vtable",
                &format!("built Xbox-layout cmdlist vtable from real {real_vt:p} (+4 shift at PC28)"));
            // Diagnostic: RVAs of the real PC methods inside D3D12Core, to attribute crash frames.
            let core = GetModuleHandleW("D3D12Core.dll ".encode_utf16().collect::<Vec<u16>>().as_ptr()) as usize;
            let mut s = String::new();
            for i in 0..86 { s.push_str(&format!("{i}:{:x} ", (*real_vt.add(i)).wrapping_sub(core))); }
            record("d3d12_xs", "cmdlist.vtable", &format!("XBOX_VT at {:p}, slot98={:#x} stub={:#x}", XBOX_VT.as_ptr(), XBOX_VT[98].load(Ordering::Relaxed), inserted_stub as usize));
            record("d3d12_xs", "cmdlist.vtable", &format!("PC cmdlist method RVAs in D3D12Core: {s}"));
        }
        // The real object keeps its PC vtable; the title only ever sees a shim (below).
        let _ = (VirtualProtect as usize, PAGE_READWRITE);
    }
}

// ---- command-list shim object ------------------------------------------------------------
//
// On Xbox the command list is a public struct with an inline GPU command buffer: the title
// writes raw command packets straight into it for hot state changes, e.g. in
// FD3D12StateCacheBase::ApplyState (EXE+0x4e0433):
//
//     mov rcx,[list+18h]             ; current write pointer
//     cmp rcx,[list+30h]             ; end of buffer -> jae slow path
//     mov dword ptr [rcx],0C000B600h ; mov [rcx+4],eax ; lea rax,[rcx+8] ; mov [list+18h],rax
//
// and only when the buffer is full does it call the method (slow path = vtable slot 20,
// IASetPrimitiveTopology). Handing the title the real PC object therefore lets it read
// D3D12-internal fields as buffer pointers and scribble through them. Instead the title
// gets this shim: our own zeroed block whose write pointer == end pointer (both 0), so
// every inline fast path of that form falls back to the API call, with a vtable of thunks
// that swap the shim for the real list and jump through the Xbox-layout table.
//
// Status: the layout fact (+0x18 write ptr, +0x30 end ptr) is observed at one call site;
// other inline fields the title may read are served zeros (unverified).

/// Offset of the real ID3D12GraphicsCommandList pointer inside the shim.
const SHIM_REAL_OFF: usize = 0x38;
/// Shim size: generous and zero-filled, for any other inline field the title reads.
const SHIM_SIZE: usize = 0x4000;
static SHIM_VT: [AtomicUsize; N] = [const { AtomicUsize::new(0) }; N];
static SHIM_BUILT: std::sync::Once = std::sync::Once::new();

unsafe fn build_shim_thunks() {
    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, ty: u32, protect: u32) -> *mut c_void;
    }
    let code = VirtualAlloc(core::ptr::null_mut(), N * 32, 0x3000, 0x40) as *mut u8;
    assert!(!code.is_null(), "d3d12_xs: cannot allocate command-list shim thunks");
    let timed = crate::slowcall::enabled();
    if timed {
        crate::slowcall::calibrate();
        crate::slowcall::CL_TABLE.store(XBOX_VT.as_ptr() as usize, Ordering::SeqCst);
    }
    for i in 0..N {
        let p = code.add(i * 32);
        let mut b: Vec<u8> = vec![0x48, 0x8B, 0x49, SHIM_REAL_OFF as u8];
        if timed && !is_probe(XBOX_VT[i].load(Ordering::Relaxed)) {
            b.extend_from_slice(&[0x41, 0xBA]);
            b.extend_from_slice(&(i as u32).to_le_bytes());
            b.extend_from_slice(&[0x48, 0xB8]);
            b.extend_from_slice(&(crate::slowcall::cl_timed as usize as u64).to_le_bytes());
            b.extend_from_slice(&[0xFF, 0xE0]);
        } else {
            b.extend_from_slice(&[0x48, 0xB8]);
            b.extend_from_slice(&(XBOX_VT.as_ptr().add(i) as u64).to_le_bytes());
            b.extend_from_slice(&[0xFF, 0x20]);
        }
        core::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
        SHIM_VT[i].store(p as usize, Ordering::Relaxed);
    }
}

/// Wrap a freshly created real command list in a shim object for the title.
pub fn wrap(real_list: *mut c_void) -> *mut c_void {
    if real_list.is_null() {
        return real_list;
    }
    apply(real_list); // builds the Xbox-layout table on first use
    crate::cl2_state::on_new_list(real_list);
    unsafe {
        SHIM_BUILT.call_once(|| build_shim_thunks());
        let layout = std::alloc::Layout::from_size_align(SHIM_SIZE, 16).unwrap();
        let p = std::alloc::alloc_zeroed(layout);
        assert!(!p.is_null(), "d3d12_xs: cannot allocate command-list shim");
        *(p as *mut usize) = SHIM_VT.as_ptr() as usize;
        *(p.add(SHIM_REAL_OFF) as *mut usize) = real_list as usize;
        crate::cmdlist_x::note_shim(real_list, p as *mut c_void);
        static N_WRAPPED: AtomicUsize = AtomicUsize::new(0);
        if N_WRAPPED.fetch_add(1, Ordering::Relaxed) < 3 {
            record("d3d12_xs", "cmdlist.shim", &format!("real {real_list:p} wrapped as shim {p:p}"));
        }
        p as *mut c_void
    }
}

pub unsafe fn set_real(shim: *mut c_void, real: *mut c_void) {
    unsafe { *((shim as *mut u8).add(SHIM_REAL_OFF) as *mut usize) = real as usize };
}

/// If `p` is one of our shims return the real command list, else `p` unchanged.
pub fn unwrap(p: *mut c_void) -> *mut c_void {
    if p.is_null() {
        return p;
    }
    unsafe {
        if *(p as *const usize) == SHIM_VT.as_ptr() as usize {
            *((p as *const u8).add(SHIM_REAL_OFF) as *const usize) as *mut c_void
        } else if let Some(real) = crate::dma_list::unwrap(p) {
            real
        } else {
            p
        }
    }
}

/// How many times an Xbox-inserted (unmapped) slot was called — a nonzero value
/// means the game needs one of those 4 methods implemented.
pub fn inserted_hits() -> usize {
    INSERTED_HITS.load(Ordering::Relaxed)
}
