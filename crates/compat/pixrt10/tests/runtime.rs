use core::ffi::c_void;
use core::ptr::null;
use pixrt10::runtime::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use xbtp::link_dev;

extern "system" {
    fn QueryThreadCycleTime(h: usize, c: *mut u64) -> i32;
    fn GetThreadDescription(h: usize, out: *mut *mut u16) -> i32;
    fn LocalFree(p: *mut c_void) -> *mut c_void;
    fn WaitForSingleObject(h: usize, ms: u32) -> u32;
}

fn setup() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("pixrt10_runtime_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("TOMBOX_LOG", dir.join("tombox_compat.log"));
    std::env::set_var("TOMBOX_GDK_ROOT", dir.join("data"));
    std::env::set_var("TOMBOX_CONSOLE_OSBUILD", "22000");
    dir
}

fn pool_is_none() -> bool {
    context().pool.lock().unwrap().is_none()
}

fn wait_until(f: impl Fn() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn cycles(h: usize) -> u64 {
    let mut c = 0u64;
    assert_ne!(unsafe { QueryThreadCycleTime(h, &mut c) }, 0);
    c
}

fn description(h: usize) -> String {
    let mut p: *mut u16 = core::ptr::null_mut();
    assert!(unsafe { GetThreadDescription(h, &mut p) } >= 0);
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    let s = String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(p, n) });
    unsafe { LocalFree(p as *mut c_void) };
    s
}

static BEGINS: AtomicU32 = AtomicU32::new(0);
static ENDS: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn hb(ctx: usize, desc: *const c_void) -> i32 {
    assert_eq!(ctx, 0x1234);
    assert!(!desc.is_null());
    BEGINS.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "system" fn he(ctx: usize) -> i32 {
    assert_eq!(ctx, 0x1234);
    ENDS.fetch_add(1, Ordering::SeqCst);
    0
}

static HANDLER: CaptureHandler = CaptureHandler { kind: 5, reserved: [0; 3], ctx: 0x1234, begin: hb, end: he };

#[test]
fn from_title_builds_the_runtime_once_listens_on_0x1080_and_idles() {
    let dir = setup();
    assert!(bind_xbtp(XbtpFns {
        session_io: || panic!("22000 must close an accepted link before resolving session I/O"),
        init_system: xbtp::link::XbtpLinkInitializeSystem,
        create: xbtp::link::XbtpLinkCreateHandle,
        listen: unsafe { core::mem::transmute::<usize, unsafe extern "C" fn(*mut u8, *mut c_void, u16, *mut usize) -> i32>(xbtp::link::XbtpLinkListen as *const () as usize) },
        accept: xbtp::link::XbtpLinkAccept,
        close: xbtp::link::XbtpLinkCloseHandle,
    }));
    unsafe {
        let mut t = [0xaaaa_aaaa_aaaa_aaaau64; 16];
        assert_eq!(from_title(0x55f0_114d, 0x35dbc, 0, t.as_mut_ptr() as *mut c_void, 0x78), 0);
        assert_eq!(t[..15], title_table().map(|v| v as u64));
        assert_eq!(t[15], 0xaaaa_aaaa_aaaa_aaaa);
        let s = singleton().unwrap();
        assert_eq!(s.layout, LAYOUT_22000);
        assert_ne!(s.block, 0);
        let rt = runtime().unwrap();
        assert_ne!(rt.block, 0);
        assert_eq!((rt.magic, rt.version, rt.flag), (0x55f0_114d, 0x35dbc, false));
        let c = context();
        assert_eq!(c.magic.load(Ordering::SeqCst), 0x55f0_114d);
        assert_eq!(c.version.load(Ordering::SeqCst), 0x35dbc);
        assert_eq!(*c.pix_dir.lock().unwrap(), PIX_DIR);
        assert_eq!(c.capture_if.load(Ordering::SeqCst), rt as *const Runtime as usize + 8);
        assert_eq!(c.register_status, 0);
        assert!(c.scheduler_events.iter().all(|&e| e != 0));
        let pix = pix_host_dir().unwrap();
        assert!(pix.starts_with(dir.join("data")), "{}", pix.display());
        assert!(pix.is_dir());
        assert!(rt.pix_dir_created);
        assert!(wait_until(|| rt.started.iter().all(|s| s.load(Ordering::SeqCst))));
        let hs: Vec<usize> = rt.threads.iter().map(|h| h.load(Ordering::SeqCst)).collect();
        assert!(hs.iter().all(|&h| h != 0));
        for (h, n) in hs.iter().zip(THREAD_NAMES) {
            assert_eq!(description(*h), n);
        }
        for a in &rt.affinity {
            let m = a.load(Ordering::SeqCst);
            assert!(m == AFFINITY_WIDE || m == AFFINITY_NARROW);
        }
        assert!(wait_until(|| link_dev::listening(0x1080) && link_dev::pending_recvs(0x1080) == 1), "device engine is not parked on the in-process xbtplink listener");
        assert_eq!(listener_refs(0x1080), 1);
        assert_ne!(rt.device.cancel_event(), 0);
        std::thread::sleep(Duration::from_millis(50));
        let before: Vec<u64> = hs.iter().map(|&h| cycles(h)).collect();
        std::thread::sleep(Duration::from_millis(500));
        let after: Vec<u64> = hs.iter().map(|&h| cycles(h)).collect();
        for (i, (b, a)) in before.iter().zip(&after).enumerate() {
            assert!(a - b < 2_000_000, "{} used {} cycles in 500 ms", THREAD_NAMES[i], a - b);
        }
        for &h in &hs {
            assert_eq!(WaitForSingleObject(h, 0), 0x102);
        }
        let mut u = [0x5au8; 24];
        assert_eq!(from_title(1, 2, 1, u.as_mut_ptr() as *mut c_void, 13), 0);
        assert_eq!(u[..13], title_table()[0].to_le_bytes().iter().chain(title_table()[1].to_le_bytes().iter()).copied().take(13).collect::<Vec<u8>>()[..]);
        assert_eq!(u[13..], [0x5au8; 11]);
        assert!(core::ptr::eq(runtime().unwrap(), rt));
        assert!(core::ptr::eq(singleton().unwrap(), s));
        assert_eq!(rt.magic, 0x55f0_114d);
        assert_eq!(from_title(1, 2, 0, core::ptr::null_mut(), 0x78), 0);
        assert_eq!(listener_refs(0x1080), 1);

        assert_eq!(capture_flags(), 0);
        assert_eq!(capture_state(), 0);
        assert_eq!(begin_capture(2, null()), E_POINTER);
        let desc = [0u8; 0x40];
        let d = desc.as_ptr() as *const c_void;
        assert_eq!(begin_capture(2, d), S_FALSE);
        assert_eq!(capture_state(), 1);
        assert_eq!(begin_capture(2, d), E_PENDING);
        assert_eq!(end_capture(0), 0x8007_0006u32 as i32);
        assert_eq!(capture_state(), 0);
        assert_eq!(begin_capture(1, d), 0x8000_4001u32 as i32);
        assert_eq!(capture_state(), 0);

        set_capture_handler(&HANDLER);
        assert_eq!(begin_capture(5, d), 0);
        assert_eq!(BEGINS.load(Ordering::SeqCst), 1);
        assert_eq!(end_capture(1), 0x8007_0006u32 as i32);
        assert_eq!(ENDS.load(Ordering::SeqCst), 1);
        assert_eq!(begin_capture(6, d), 0);
        assert_eq!(BEGINS.load(Ordering::SeqCst), 1);
        assert_eq!(capture_state(), 1);
        end_capture(0);
        assert_eq!(ENDS.load(Ordering::SeqCst), 2);
        clear_capture_handler(4);
        assert_eq!(CAPTURE.handler.load(Ordering::SeqCst), &HANDLER as *const CaptureHandler as usize);
        clear_capture_handler(5);
        assert_eq!(CAPTURE.handler.load(Ordering::SeqCst), 0);
        assert_eq!(begin_capture(5, d), S_FALSE);
        end_capture(0);

        assert_eq!(set_word_14(1), 0);
        assert_eq!(capture_flags(), 1 << 4);
        assert_eq!(set_word_16(3), 0);
        assert_eq!(set_word_12(1), 0);
        assert_eq!(set_word_10(2), 0);
        assert_eq!(capture_flags(), (1 << 5) | (1 << 4) | (1 << 3));
        assert_eq!(begin_capture(2, d), E_PENDING);
        for f in [set_word_14, set_word_16, set_word_12, set_word_10] {
            f(0);
        }
        pixrt10::state().words[0].store(0x8001, Ordering::SeqCst);
        assert_eq!(capture_flags(), 0x8001);
        pixrt10::state().words[0].store(0, Ordering::SeqCst);
        pixrt10::state().words[1].store(1, Ordering::SeqCst);
        assert_eq!(capture_flags(), 2);
        pixrt10::state().words[1].store(0, Ordering::SeqCst);
        assert_eq!(capture_flags(), 0);

        assert_eq!(install_df_pmc(1, 2, 3, 4), E_ACCESSDENIED);
        assert_eq!(install_l2i_pmc(0x10001, 2, 3, 4), E_ACCESSDENIED);

        assert_eq!(context().state.load(Ordering::SeqCst), 1);
        assert_eq!(event_buffer(), [0, 0, 0]);
        assert_eq!(thread_event_buffer(0), 0);
        assert_eq!(event_buffer(), [0, 0, usize::MAX]);
        assert_eq!(thread_event_buffer(1), 0);
        assert_eq!(event_buffer(), [0, 0, usize::MAX]);
        assert!(pool_is_none());
        std::thread::spawn(|| assert_eq!(event_buffer(), [0, 0, 0])).join().unwrap();

        assert_eq!(XMON_GEN.load(Ordering::SeqCst), 0x15);
        assert_eq!(slot8(0x1f, 0, 0, 0), E_INVALIDARG);
        assert_eq!(slot8(0, 0, 0, 0x1f), E_INVALIDARG);
        assert_eq!(slot8(0, 1, 2, 0x1e), E_ACCESSDENIED);
        assert_eq!(XMON_GEN.load(Ordering::SeqCst), 0x15);
        assert_eq!(driver_gen(), 0x15);
        XMON_GEN.store(0x18, Ordering::SeqCst);
        assert_eq!(slot8(0x1000, 0, 0, 0), E_ACCESSDENIED);
        assert_eq!(XMON_GEN.load(Ordering::SeqCst), 0x15);
        XMON_GEN.store(0x18, Ordering::SeqCst);
        assert!(!xmon_tables_valid());
        XMON_GEN.store(0x15, Ordering::SeqCst);
        assert!(xmon_tables_valid());

        assert_eq!(PROVIDER_STATE.bits.load(Ordering::SeqCst), 0);
        let name: Vec<u16> = "fps".encode_utf16().chain(Some(0)).collect();
        report_counter(name.as_ptr(), 60.0);
        apply_enable(1, 5, 0, 0);
        assert_eq!(PROVIDER_STATE.bits.load(Ordering::SeqCst), 1);
        report_counter(name.as_ptr(), 60.0);
        report_counter(null(), 1.0);
        apply_enable(2, 0, 0, 0);
        assert_eq!(PROVIDER_STATE.bits.load(Ordering::SeqCst), 1);
        apply_enable(0, 0, 0, 0);
        assert_eq!(PROVIDER_STATE.bits.load(Ordering::SeqCst), 0);
        assert_eq!(PROVIDER_STATE.enabled.load(Ordering::SeqCst), 0);

        assert!(link_dev::listening(0x1080));
        assert_eq!(link_dev::pending_recvs(0x1080), 1);
        for &h in &hs {
            assert_eq!(WaitForSingleObject(h, 0), 0x102);
        }
        let peer = xbtp::link::XbtpLinkCreateHandle();
        assert!(!peer.is_null());
        let address = [2, 0, 0x10, 0x80, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(xbtp::link::XbtpLinkConnect(peer, address.as_ptr(), 16, 5000), 0);
        assert!(wait_until(|| rt.device.cancel_event() == 0));
        let mut byte = 0;
        let mut received = 1;
        assert_eq!(xbtp::link::XbtpLinkRecv(peer, &mut byte, 1, &mut received, 0, core::ptr::null_mut()), 0);
        assert_eq!(received, 0, "the 22000 worker closes the accepted link without a PIX session");
        xbtp::link::XbtpLinkCloseHandle(peer);
        assert_eq!(listener_refs(0x1080), 0);
        assert!(!link_dev::listening(0x1080));
        let replacement = xbtp::link::XbtpLinkCreateHandle();
        assert!(!replacement.is_null());
        let mut ready = core::ptr::null_mut();
        assert_eq!(xbtp::link::XbtpLinkListen(replacement, core::ptr::null_mut(), 0x1080, &mut ready), 0);
        xbtp::link::XbtpLinkCloseHandle(replacement);

    }
}

#[test]
fn worker_affinity_19041_uses_processor_six() {
    for (process, expected) in [(0, 0x30), (0x3f, 0x30), (0x40, 0x60), (0x7f, 0x60), (0x80, 0x30), (0xff, 0x60), (0x10000, 0x30), (0x10040, 0x60)] {
        assert_eq!(affinity_mask_for_build(19041, process), expected);
        for build in [22000, 22621] {
            assert_eq!(affinity_mask_for_build(build, process), affinity_mask(process));
        }
    }
}

#[test]
fn affinity_and_layouts_follow_the_console() {
    assert_eq!(affinity_mask(0xff), AFFINITY_WIDE);
    assert_eq!(affinity_mask(0x80), AFFINITY_WIDE);
    assert_eq!(affinity_mask(0x7f), AFFINITY_NARROW);
    assert_eq!(affinity_mask(0), AFFINITY_NARROW);
    assert_eq!(layout(22000), Some(LAYOUT_22000));
    assert_eq!(layout(22620), Some(LAYOUT_22000));
    assert_eq!(layout(22621), Some(LAYOUT_22621));
    assert_eq!(layout(19041), Some(LAYOUT_19041));
    assert_eq!(TABLE_BYTES, 0x78);
    for l in [LAYOUT_22000, LAYOUT_22621] {
        assert!(l.slots.windows(2).all(|w| w[0] != w[1]));
        assert!(l.slots.iter().all(|&s| s < l.from_title));
    }
    assert_eq!(TRANSPORT[0], 3);
    assert_eq!(TRANSPORT[1], SERVICE as u32);
}

#[test]
fn layouts_match_the_console_dlls() {
    let dumps = [
        (r"D:\xbox-pc-port\games\XDefiant\extracted\EmbeddedXvd\Windows\System32\pixrt10.dll", LAYOUT_22000),
        (r"D:\xbox-pc-port\games\Trackmania\extracted\Ubisoft.TrackmaniaNext_1.6.2.0_neutral__ngz4m417e0mpw\EmbeddedXvd\Windows\System32\pixrt10.dll", LAYOUT_22621),
    ];
    for (path, l) in dumps {
        let Ok(img) = std::fs::read(path) else { continue };
        let pe = u32::from_le_bytes(img[0x3c..0x40].try_into().unwrap()) as usize;
        let nsec = u16::from_le_bytes(img[pe + 6..pe + 8].try_into().unwrap()) as usize;
        let opt = u16::from_le_bytes(img[pe + 20..pe + 22].try_into().unwrap()) as usize;
        let sec = pe + 24 + opt;
        let off = |rva: usize| -> usize {
            for i in 0..nsec {
                let s = sec + i * 40;
                let va = u32::from_le_bytes(img[s + 12..s + 16].try_into().unwrap()) as usize;
                let vs = u32::from_le_bytes(img[s + 8..s + 12].try_into().unwrap()) as usize;
                let raw = u32::from_le_bytes(img[s + 20..s + 24].try_into().unwrap()) as usize;
                if rva >= va && rva < va + vs {
                    return rva - va + raw;
                }
            }
            panic!("{rva:#x}")
        };
        let code = &img[off(l.from_title as usize)..off(l.from_title as usize) + 0x1d0];
        let mut slots = Vec::new();
        let mut i = 0;
        while i + 7 <= code.len() {
            if code[i] == 0x48 && code[i + 1] == 0x8d && code[i + 2] == 0x05 {
                let d = i32::from_le_bytes(code[i + 3..i + 7].try_into().unwrap());
                slots.push((l.from_title as i64 + i as i64 + 7 + d as i64) as u32);
                i += 7;
            } else {
                i += 1;
            }
        }
        assert_eq!(slots[slots.len() - SLOTS..], l.slots, "{path}");
        let size = |imm: u32| code.windows(5).any(|w| w[0] == 0xb9 && u32::from_le_bytes(w[1..5].try_into().unwrap()) == imm);
        assert!(size(l.singleton_bytes as u32), "{path}");
        let pat: Vec<u8> = TRANSPORT.iter().flat_map(|v| v.to_le_bytes()).collect();
        assert!(img.windows(pat.len()).any(|w| w == pat.as_slice()), "{path}");
        let name: Vec<u8> = THREAD_NAMES[1].encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        assert!(img.windows(name.len()).any(|w| w == name.as_slice()), "{path}");
    }
}

#[test]
fn xmon_tables_and_slot8_input_match_the_22000_dll() {
    let Ok(img) = std::fs::read(r"D:\xbox-pc-port\games\XDefiant\extracted\EmbeddedXvd\Windows\System32\pixrt10.dll") else { return };
    let pe = u32::from_le_bytes(img[0x3c..0x40].try_into().unwrap()) as usize;
    let nsec = u16::from_le_bytes(img[pe + 6..pe + 8].try_into().unwrap()) as usize;
    let opt = u16::from_le_bytes(img[pe + 20..pe + 22].try_into().unwrap()) as usize;
    let sec = pe + 24 + opt;
    let off = |rva: usize| -> usize {
        for i in 0..nsec {
            let s = sec + i * 40;
            let va = u32::from_le_bytes(img[s + 12..s + 16].try_into().unwrap()) as usize;
            let vs = u32::from_le_bytes(img[s + 8..s + 12].try_into().unwrap()) as usize;
            let raw = u32::from_le_bytes(img[s + 20..s + 24].try_into().unwrap()) as usize;
            if rva >= va && rva < va + vs {
                return rva - va + raw;
            }
        }
        panic!("{rva:#x}")
    };
    for t in [0x54850usize, 0x54b40, 0x54e30] {
        for i in 0..XMON_TABLE_ROWS as usize {
            let o = off(t + i * 24);
            assert_eq!(u16::from_le_bytes([img[o], img[o + 1]]) as usize, i);
        }
    }
    let g = off(0x55590);
    let guid: Vec<u32> = (0..4).map(|i| u32::from_le_bytes(img[g + i * 4..g + i * 4 + 4].try_into().unwrap())).collect();
    assert_eq!(guid, CORE_PMC_GUID);
    let s = off(0x5f994);
    assert_eq!(u32::from_le_bytes(img[s..s + 4].try_into().unwrap()), 0x15);
    let code = &img[off(0x2cfc7)..off(0x2cfc7) + 7];
    assert_eq!(&code[3..7], &(EVENT_BUFFER_BYTES as u32).to_le_bytes());
    let first = &img[off(0x2cfd6)..off(0x2cfd6) + 7];
    assert_eq!(&first[3..7], &(EVENT_BUFFER_FIRST as u32).to_le_bytes());
    let nc = &img[off(0x2c9f8)..off(0x2c9f8) + 5];
    assert_eq!(&nc[1..5], &(HR_NO_CAPTURE as u32).to_le_bytes());
    let grow = &img[off(0x260e5)..off(0x260e5) + 8];
    assert_eq!(&grow[4..8], &(POOL_GROW as u32).to_le_bytes());
    let q = &img[off(0x44e82)..off(0x44e82) + 5];
    assert_eq!(&q[1..5], &IOCTL_XMON_QUERY.to_le_bytes());
    let c = &img[off(0x4545b)..off(0x4545b) + 5];
    assert_eq!(&c[1..5], &IOCTL_CORE_PMC.to_le_bytes());
}
