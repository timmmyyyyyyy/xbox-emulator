use super::*;
use crate::alias::{create_placed_x, Kind};

extern "system" {
    fn VirtualAlloc(addr: *mut c_void, size: usize, ty: u32, protect: u32) -> *mut c_void;
    fn VirtualFree(p: *mut c_void, n: usize, ty: u32) -> i32;
}

const IID_ALLOCATOR: Guid = Guid(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
const IID_GFX_LIST: Guid = Guid(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
const IID_QUEUE: Guid = Guid(0x0ec870a6, 0x5d7e, 0x4c22, [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed]);

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

unsafe fn e2e_ms(g: &mut Gpu, rb: *mut c_void, up: *mut c_void, n: usize) -> f64 {
    let mut t = Vec::new();
    for _ in 0..n {
        let t0 = std::time::Instant::now();
        unsafe { g.run(|l| copy(l, rb, 0, up, 0, 4), || {}) };
        t.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    median(t)
}

#[test]
#[ignore]
fn hw_shadow_sync_cost_100_ranges() {
    unsafe {
        let mut g = Gpu::new();
        let rb = g.committed(3, 0x1_0000, 0x400);
        let up = g.committed(2, 0x1_0000, 0xac3);
        let bare = e2e_ms(&mut g, rb, up, 200);
        for (width, watch) in [(0x1000u64, 0u32), (0x1_0000, 0), (0x1000, 0x20_0000), (0x1_0000, 0x20_0000)] {
            let base = VirtualAlloc(core::ptr::null_mut(), 100 << 16, 0x3000 | watch, 4);
            assert!(!base.is_null());
            let mut res = Vec::new();
            for i in 0..100u64 {
                let mut r: *mut c_void = core::ptr::null_mut();
                let m = create_placed_x(g.dev, base as u64 + (i << 16), &buf(width), width, 0, core::ptr::null(), &mut r).expect("placement");
                assert_eq!(m.kind, Kind::Shadow);
                res.push(r);
            }
            let mut down = Vec::new();
            let mut up_t = Vec::new();
            for k in 0..200u32 {
                *(base as *mut u32).add((k as usize % 100) << 14) = k;
                let t0 = std::time::Instant::now();
                before_execute();
                down.push(t0.elapsed().as_secs_f64() * 1e3);
                let t1 = std::time::Instant::now();
                sync_up();
                up_t.push(t1.elapsed().as_secs_f64() * 1e3);
            }
            let e2e = e2e_ms(&mut g, rb, up, 200);
            print!("write_watch={} ", watch != 0);
            println!("ranges=100 width={width:#x} live_bytes={:#x}: before_execute median {:.4} ms, sync_up median {:.4} ms, execute->fence {:.4} ms vs {:.4} ms without shadows (+{:.4} ms)",
                100 * width, median(down), median(up_t), e2e, bare, e2e - bare);
            for r in res {
                vcall!(r, 2, fn() -> u32);
            }
            crate::alias::flush_spans();
            VirtualFree(base, 0, 0x8000);
        }
    }
}

#[test]
fn write_watched_title_memory_merges_only_dirty_pages() {
    unsafe {
        let mut g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 0x4_0000, 0x3000 | 0x20_0000, 4);
        assert!(!base.is_null());
        let va = base as u64;
        let cpu = va as *mut u32;
        *cpu.add(1) = 0x0101;
        assert!(dirty_pages(va, va + 0x1_0000).is_some_and(|p| p.contains(&va)), "write-watch reports the written page");
        assert_eq!(dirty_pages(va, va + 0x1_0000), Some(vec![]), "the query resets the dirty state");
        let mut a: *mut c_void = core::ptr::null_mut();
        let ma = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut a).expect("shadow placement");
        assert_eq!(ma.kind, Kind::Shadow);
        *cpu.add(0x2000 / 4 + 3) = 0x3030;
        let rb = g.committed(3, 0x1_0000, 0x400);
        g.run(|l| copy(l, rb, 0, a, 0, 0x1_0000), || {});
        let r = g.mapped(rb);
        assert_eq!((*r.add(1), *r.add(0x2000 / 4 + 3)), (0x0101, 0x3030), "the snapshot bytes and the dirty page both reach the GPU");
        let up = g.committed(2, 0x1_0000, 0xac3);
        *g.mapped(up).add(0x5000 / 4) = 0x5050;
        g.run(|l| copy(l, a, 0x5000, up, 0x5000, 4), || {});
        assert_eq!(*cpu.add(0x5000 / 4), 0x5050, "GPU writes still reach write-watched title memory");
        *cpu.add(0x5000 / 4 + 1) = 0x5151;
        g.run(|l| copy(l, rb, 0, a, 0, 0x1_0000), || {});
        assert_eq!((*r.add(0x5000 / 4), *r.add(0x5000 / 4 + 1)), (0x5050, 0x5151), "a CPU write next to a copied-back GPU write merges");
        vcall!(a, 2, fn() -> u32);
        VirtualFree(base, 0, 0x8000);
    }
}

#[test]
fn merge_down_takes_cpu_bytes_and_keeps_unmerged_gpu_bytes() {
    let mut snap = vec![0u8; 600];
    let mut shadow = vec![0u8; 600];
    let mut title = vec![0u8; 600];
    title[3] = 9;
    title[400] = 4;
    shadow[7] = 5;
    assert_eq!(merge_down(&title, &mut snap, &mut shadow), 2);
    assert_eq!((shadow[3], shadow[400], shadow[7]), (9, 4, 5));
    assert_eq!((snap[3], snap[400], snap[7]), (9, 4, 0));
    assert_eq!(merge_down(&title, &mut snap, &mut shadow), 0);
    shadow[8] = 6;
    shadow[300] = 1;
    shadow[301] = 2;
    assert_eq!(merge_up(&shadow, &mut snap), vec![(7, 2), (300, 2)]);
    assert_eq!(snap, shadow);
    assert!(merge_up(&shadow, &mut snap).is_empty());
}

#[test]
fn merge_up_run_crossing_a_block_edge_is_one_run() {
    let mut snap = vec![0u8; 1024];
    let mut shadow = vec![0u8; 1024];
    for b in &mut shadow[250..262] {
        *b = 0x7f;
    }
    shadow[1023] = 1;
    assert_eq!(merge_up(&shadow, &mut snap), vec![(250, 12), (1023, 1)]);
}

#[test]
fn live_ranges_coalesce() {
    assert_eq!(coalesce(vec![(0x2000, 0x3000), (0, 0x1000), (0x800, 0x1800), (5, 5)]), vec![(0, 0x1800), (0x2000, 0x3000)]);
    assert!(coalesce(Vec::new()).is_empty());
}

fn buf(width: u64) -> [u8; 0x38] {
    buf_desc(width)
}

struct Gpu {
    dev: *mut c_void,
    queue: *mut c_void,
    alloc: *mut c_void,
    list: *mut c_void,
    fence: *mut c_void,
    value: u64,
}

impl Gpu {
    unsafe fn new() -> Gpu {
        unsafe {
            let p = crate::host_proc(b"D3D12CreateDevice\0");
            assert!(p != 0);
            let f: extern "system" fn(*mut c_void, u32, *const Guid, *mut *mut c_void) -> i32 = core::mem::transmute(p);
            let mut dev: *mut c_void = core::ptr::null_mut();
            assert!(f(core::ptr::null_mut(), 0xc000, &IID_DEVICE, &mut dev) >= 0);
            let mut g = Gpu { dev, queue: core::ptr::null_mut(), alloc: core::ptr::null_mut(), list: core::ptr::null_mut(), fence: core::ptr::null_mut(), value: 0 };
            let qd = [0u32; 4];
            assert!(vcall!(dev, 8, fn(*const u32, *const Guid, *mut *mut c_void) -> i32, qd.as_ptr(), &IID_QUEUE, &mut g.queue) >= 0);
            assert!(vcall!(dev, 9, fn(u32, *const Guid, *mut *mut c_void) -> i32, 0, &IID_ALLOCATOR, &mut g.alloc) >= 0);
            assert!(vcall!(dev, 12, fn(u32, u32, *mut c_void, *mut c_void, *const Guid, *mut *mut c_void) -> i32, 0, 0, g.alloc, core::ptr::null_mut(), &IID_GFX_LIST, &mut g.list) >= 0);
            assert!(vcall!(dev, 36, fn(u64, u32, *const Guid, *mut *mut c_void) -> i32, 0, 0, &IID_FENCE, &mut g.fence) >= 0);
            vcall!(g.list, 9, fn() -> i32);
            g
        }
    }

    unsafe fn run(&mut self, rec: impl FnOnce(*mut c_void), during: impl FnOnce()) {
        unsafe {
            vcall!(self.alloc, 8, fn() -> i32);
            vcall!(self.list, 10, fn(*mut c_void, *mut c_void) -> i32, self.alloc, core::ptr::null_mut());
            rec(self.list);
            assert!(vcall!(self.list, 9, fn() -> i32) >= 0);
            before_execute();
            let l = [self.list];
            vcall!(self.queue, 10, fn(u32, *const *mut c_void) -> (), 1, l.as_ptr());
            during();
            assert!(after_execute(self.queue, 0, 0).is_some() || count() == 0);
            self.value += 1;
            vcall!(self.queue, 14, fn(*mut c_void, u64) -> i32, self.fence, self.value);
            let mut n = 0;
            while vcall!(self.fence, 8, fn() -> u64) < self.value {
                std::thread::sleep(std::time::Duration::from_millis(1));
                n += 1;
                assert!(n < 10_000);
            }
        }
    }

    unsafe fn committed(&self, heap_type: u32, size: u64, state: u32) -> *mut c_void {
        let heap: [u32; 5] = [heap_type, 0, 0, 1, 1];
        let bd = buf(size);
        let mut r: *mut c_void = core::ptr::null_mut();
        assert!(unsafe { vcall!(self.dev, 27, fn(*const u32, u32, *const u8, u32, *const c_void, *const Guid, *mut *mut c_void) -> i32, heap.as_ptr(), 0, bd.as_ptr(), state, core::ptr::null(), &IID_RESOURCE, &mut r) } >= 0);
        r
    }

    unsafe fn mapped(&self, r: *mut c_void) -> *mut u32 {
        let mut m: *mut u32 = core::ptr::null_mut();
        assert!(unsafe { vcall!(r, 8, fn(u32, *const c_void, *mut *mut u32) -> i32, 0, core::ptr::null(), &mut m) } >= 0);
        m
    }
}

unsafe fn copy(l: *mut c_void, dst: *mut c_void, doff: u64, src: *mut c_void, soff: u64, n: u64) {
    unsafe { vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), dst, doff, src, soff, n) };
}

#[test]
fn shadowed_title_buffer_shares_bytes_both_ways_and_leaves_memory_unpinned() {
    unsafe {
        let mut g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 1 << 20, 0x3000, 4);
        assert!(!base.is_null());
        let va = base as u64 + 0x2_0000;
        let cpu = va as *mut u32;
        *cpu.add(3) = 0xfeed_beef;
        let d = buf(0x1_0000);
        let mut a: *mut c_void = core::ptr::null_mut();
        let mut b: *mut c_void = core::ptr::null_mut();
        let ma = create_placed_x(g.dev, va, &d, 0x1_0000, 0, core::ptr::null(), &mut a).expect("shadow placement");
        let mb = create_placed_x(g.dev, va, &d, 0x1_0000, 0, core::ptr::null(), &mut b).expect("second placement");
        assert_eq!((ma.kind, mb.kind), (Kind::Shadow, Kind::Shadow));
        assert_eq!((ma.heap, ma.offset), (mb.heap, mb.offset));
        assert_eq!(ma.offset, 0x2_0000);
        assert_eq!(map_va(a), Some(va));
        crate::map_x::ensure_patched(a);
        let mut p: *mut u8 = core::ptr::null_mut();
        assert_eq!(vcall!(a, 8, fn(u32, *const c_void, *mut *mut u8) -> i32, 0, core::ptr::null(), &mut p), 0);
        assert_eq!(p as u64, va, "Map of a title placement returns the title address");
        vcall!(a, 9, fn(u32, *const c_void) -> (), 0, core::ptr::null());
        *cpu.add(4) = 0x11;
        let rb = g.committed(3, 0x1_0000, 0x400);
        let up = g.committed(2, 0x1_0000, 0xac3);
        let u = g.mapped(up);
        *u.add(5) = 0x1234_5678;
        *u.add(7) = 0x7777_0007;
        g.run(|l| {
            copy(l, rb, 0, b, 0, 0x100);
            copy(l, a, 20, up, 20, 4);
        }, || {});
        assert_eq!(*cpu.add(5), 0x1234_5678, "the GPU write is in title memory once the title fence completes");
        let r = g.mapped(rb);
        assert_eq!((*r.add(3), *r.add(4)), (0xfeed_beef, 0x11), "the GPU read CPU bytes written before and after placement");
        g.run(|l| copy(l, a, 28, up, 28, 4), || { *cpu.add(6) = 0xaaaa; });
        assert_eq!(*cpu.add(6), 0xaaaa, "a CPU write during GPU work on neighbouring bytes survives copy-back");
        assert_eq!(*cpu.add(7), 0x7777_0007);
        assert_eq!((*cpu.add(3), *cpu.add(4), *cpu.add(5)), (0xfeed_beef, 0x11, 0x1234_5678));
        g.run(|l| copy(l, rb, 0, a, 0, 0x100), || {});
        assert_eq!(*r.add(6), 0xaaaa, "the CPU write reached the GPU at the next execute");
        assert_eq!(VirtualFree(base, 0, 0x8000), 1, "title memory under a shadowed placement can be freed");
        let hold = VirtualAlloc(base, 1 << 20, 0x2000, 1);
        *u.add(8) = 0x5151;
        g.run(|l| copy(l, a, 32, up, 32, 4), || {});
        g.run(|l| copy(l, rb, 0, b, 0, 0x100), || {});
        assert_eq!(*r.add(8), 0x5151, "after the free the GPU keeps its bytes and no copy touches freed memory");
        vcall!(a, 2, fn() -> u32);
        vcall!(b, 2, fn() -> u32);
        crate::alias::flush_spans();
        assert_ne!(map_va(a), Some(va), "the released placement no longer maps the title address; a parallel test may reuse the freed pointer for its own resource");
        assert!(!shadows(|m| m.contains_key(&ma.heap)), "the shadow heap is dropped with its last placement");
        if !hold.is_null() {
            VirtualFree(hold, 0, 0x8000);
        }
    }
}

#[test]
fn merged_view_prefers_unsynced_cpu_bytes_over_shadow_bytes() {
    assert_eq!(merged_view(&[1, 9, 3, 4], &[1, 2, 3, 4], &[7, 2, 8, 4]), vec![7, 9, 8, 4]);
}

#[test]
fn polls_read_gpu_writes_before_copy_back_and_cpu_writes_before_sync() {
    unsafe {
        let g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 0x2_0000, 0x3000, 4);
        let va = base as u64;
        let cpu = va as *mut u32;
        let mut a: *mut c_void = core::ptr::null_mut();
        let ma = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut a).expect("shadow placement");
        assert_eq!(ma.kind, Kind::Shadow);
        let up = g.committed(2, 0x1_0000, 0xac3);
        *g.mapped(up).add(1) = 0x600d;
        vcall!(g.alloc, 8, fn() -> i32);
        vcall!(g.list, 10, fn(*mut c_void, *mut c_void) -> i32, g.alloc, core::ptr::null_mut());
        copy(g.list, a, 4, up, 4, 4);
        assert!(vcall!(g.list, 9, fn() -> i32) >= 0);
        let l = [g.list];
        vcall!(g.queue, 10, fn(u32, *const *mut c_void) -> (), 1, l.as_ptr());
        vcall!(g.queue, 14, fn(*mut c_void, u64) -> i32, g.fence, 1);
        while vcall!(g.fence, 8, fn() -> u64) < 1 {
            std::thread::yield_now();
        }
        assert_eq!(read(va + 4, 4), Some(0x600du32.to_le_bytes().to_vec()), "a poll sees the shader write in the shadow");
        *cpu.add(2) = 0xbeef;
        assert_eq!(read(va + 8, 4), Some(0xbeefu32.to_le_bytes().to_vec()), "a poll sees a CPU write not yet merged");
        assert_eq!(read(va + 0x1_0000, 4), None, "outside every live range the title memory is read directly");
        vcall!(a, 2, fn() -> u32);
        VirtualFree(base, 0, 0x8000);
    }
}

#[test]
fn texture_then_buffer_in_one_reservation_share_the_local_heap() {
    unsafe {
        let g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 0x40_0000, 0x3000, 4);
        let va = base as u64 + 0x10_0000;
        let mut t = buf(0);
        t[0..4].copy_from_slice(&3u32.to_le_bytes());
        t[0x10..0x18].copy_from_slice(&256u64.to_le_bytes());
        t[0x18..0x1c].copy_from_slice(&256u32.to_le_bytes());
        t[0x20..0x24].copy_from_slice(&28u32.to_le_bytes());
        t[0x2c..0x30].copy_from_slice(&0u32.to_le_bytes());
        t[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        let (mut x, mut y) = (core::ptr::null_mut(), core::ptr::null_mut());
        let mt = create_placed_x(g.dev, va, &t, 0x4_0000, 4, core::ptr::null(), &mut x).expect("texture placed");
        let mb = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut y).expect("buffer placed");
        assert_eq!(mt.kind, Kind::Local);
        assert_eq!((mb.kind, mb.heap, mb.offset), (Kind::Local, mt.heap, mt.offset), "a buffer over an existing GPU-local span joins it");
        vcall!(x, 2, fn() -> u32);
        vcall!(y, 2, fn() -> u32);
        VirtualFree(base, 0, 0x8000);
    }
}

#[test]
fn buffer_then_texture_migrates_to_one_local_heap_and_keeps_bytes() {
    unsafe {
        let mut g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 0x40_0000, 0x3000, 4);
        let va = base as u64 + 0x20_0000;
        let cpu = va as *mut u32;
        *cpu.add(3) = 0x3333_0003;
        let mut t = buf(0);
        t[0..4].copy_from_slice(&3u32.to_le_bytes());
        t[0x10..0x18].copy_from_slice(&256u64.to_le_bytes());
        t[0x18..0x1c].copy_from_slice(&256u32.to_le_bytes());
        t[0x20..0x24].copy_from_slice(&28u32.to_le_bytes());
        t[0x2c..0x30].copy_from_slice(&0u32.to_le_bytes());
        t[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        let (mut p, mut q, mut s) = (core::ptr::null_mut(), core::ptr::null_mut(), core::ptr::null_mut());
        let mp = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut p).expect("buffer first");
        assert_eq!(mp.kind, Kind::Shadow);
        let gva = vcall!(p, 11, fn() -> u64);
        let up = g.committed(2, 0x1_0000, 0xac3);
        *g.mapped(up).add(5) = 0x5555_0005;
        g.run(|l| copy(l, p, 20, up, 20, 4), || {});
        *cpu.add(7) = 0x7777_0007;
        let mut tinfo = [0u64; 2];
        vcall!(g.dev, 25, fn(*mut u64, u32, u32, *const u8) -> *mut u64, tinfo.as_mut_ptr(), 0, 1, t.as_ptr());
        let mq = create_placed_x(g.dev, va, &t, tinfo[0].max(0x4_0000), 4, core::ptr::null(), &mut q).expect("texture after");
        assert_eq!(mq.kind, Kind::Local);
        assert_eq!(mq.offset, mp.offset, "the texture lands at the buffer's offset in the migrated reservation");
        assert!(!shadows(|m| m.contains_key(&mp.heap)), "the shadow is retired by the migration");
        assert_eq!(map_va(p), None, "the migrated buffer uses the GPU-local coherence path");
        assert_eq!(vcall!(p, 11, fn() -> u64), gva, "the buffer keeps its object and GPU address");
        let ms = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut s).expect("later buffer");
        assert_eq!((ms.kind, ms.heap, ms.offset), (Kind::Local, mq.heap, mq.offset), "later placements alias the same heap and offset");
        let rb = g.committed(3, 0x1_0000, 0x400);
        g.run(|l| copy(l, rb, 0, p, 0, 0x100), || {});
        let r = g.mapped(rb);
        assert_eq!((*r.add(3), *r.add(5), *r.add(7)), (0x3333_0003, 0x5555_0005, 0x7777_0007), "CPU bytes, GPU-written bytes and unsynced CPU bytes survive the migration");
        g.run(|l| copy(l, rb, 0x100, s, 0, 0x100), || {});
        assert_eq!((*r.add(0x40 + 3), *r.add(0x40 + 5)), (0x3333_0003, 0x5555_0005), "a later buffer at the same VA sees the same bytes");
        vcall!(p, 2, fn() -> u32);
        vcall!(q, 2, fn() -> u32);
        vcall!(s, 2, fn() -> u32);
        VirtualFree(base, 0, 0x8000);
    }
}

#[test]
fn recommitted_title_pages_reach_the_gpu_as_zero() {
    unsafe {
        let mut g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 0x4_0000, 0x3000, 4);
        let va = base as u64;
        let cpu = va as *mut u32;
        *cpu.add(2) = 0x4242;
        let mut a: *mut c_void = core::ptr::null_mut();
        let ma = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut a).expect("shadow placement");
        assert_eq!(ma.kind, Kind::Shadow);
        assert_eq!(VirtualFree(base, 0x1_0000, 0x4000), 1);
        assert!(!VirtualAlloc(base, 0x1_0000, 0x1000, 4).is_null());
        let rb = g.committed(3, 0x1_0000, 0x400);
        g.run(|l| copy(l, rb, 0, a, 0, 0x100), || {});
        assert_eq!(*g.mapped(rb).add(2), 0, "decommit and recommit gives the GPU the fresh zero page");
        vcall!(a, 2, fn() -> u32);
        VirtualFree(base, 0, 0x8000);
    }
}

#[test]
fn recommitted_write_watched_pages_reach_the_gpu_as_zero_without_a_cpu_write() {
    unsafe {
        let mut g = Gpu::new();
        let base = VirtualAlloc(core::ptr::null_mut(), 0x4_0000, 0x3000 | 0x20_0000, 4);
        assert!(!base.is_null());
        let va = base as u64;
        let cpu = va as *mut u32;
        *cpu.add(2) = 0x4242;
        *cpu.add(0x8000 / 4) = 0x5353;
        let mut a: *mut c_void = core::ptr::null_mut();
        let ma = create_placed_x(g.dev, va, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut a).expect("shadow placement");
        assert_eq!(ma.kind, Kind::Shadow);
        let rb = g.committed(3, 0x1_0000, 0x400);
        g.run(|l| copy(l, rb, 0, a, 0, 0x1_0000), || {});
        assert_eq!((*g.mapped(rb).add(2), *g.mapped(rb).add(0x8000 / 4)), (0x4242, 0x5353));
        g.run(|l| copy(l, rb, 0, a, 0, 0x1_0000), || {});
        assert_eq!(VirtualFree(base, 0x1000, 0x4000), 1);
        assert!(!VirtualAlloc(base, 0x1000, 0x1000, 4).is_null());
        assert_eq!(*cpu.add(2), 0, "the recommitted page reads zero on the CPU");
        g.run(|l| copy(l, rb, 0, a, 0, 0x1_0000), || {});
        let r = g.mapped(rb);
        assert_eq!(*r.add(2), 0, "a write-watched page decommitted and recommitted with no CPU write must reach the GPU as zero");
        assert_eq!(*r.add(0x8000 / 4), 0x5353, "untouched pages keep their bytes");
        vcall!(a, 2, fn() -> u32);
        VirtualFree(base, 0, 0x8000);
    }
}

#[test]
fn mapped_views_are_shadowed_and_still_unmap() {
    extern "system" {
        fn CreateFileMappingW(file: isize, attrs: *const c_void, protect: u32, hi: u32, lo: u32, name: *const u16) -> *mut c_void;
        fn MapViewOfFileEx(h: *mut c_void, access: u32, hi: u32, lo: u32, n: usize, at: *mut c_void) -> *mut c_void;
        fn UnmapViewOfFile(p: *mut c_void) -> i32;
    }
    unsafe {
        let mut g = Gpu::new();
        let fm = CreateFileMappingW(-1, core::ptr::null(), 0x04 | 0x800_0000, 0, 0x4_0000, core::ptr::null());
        assert!(!fm.is_null());
        let v = MapViewOfFileEx(fm, 0x6, 0, 0, 0x4_0000, core::ptr::null_mut());
        assert!(!VirtualAlloc(v, 0x4_0000, 0x1000, 4).is_null());
        *(v as *mut u32) = 0xc0ffee;
        let mut a: *mut c_void = core::ptr::null_mut();
        let ma = create_placed_x(g.dev, v as u64, &buf(0x1_0000), 0x1_0000, 0, core::ptr::null(), &mut a).expect("view placement");
        assert_eq!(ma.kind, Kind::Shadow);
        let rb = g.committed(3, 0x1_0000, 0x400);
        g.run(|l| copy(l, rb, 0, a, 0, 0x10), || {});
        assert_eq!(*g.mapped(rb), 0xc0ffee);
        assert_eq!(UnmapViewOfFile(v), 1, "a shadowed view is not pinned");
        g.run(|l| copy(l, rb, 0, a, 0, 0x10), || {});
        vcall!(a, 2, fn() -> u32);
    }
}
