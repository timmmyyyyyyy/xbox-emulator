use core::ffi::c_void;
use pixrt10::driver::*;
use pixrt10::runtime::{self, *};
use std::sync::atomic::Ordering;

fn setup() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("pixrt10_initialize_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("TOMBOX_LOG", dir.join("tombox_compat.log"));
    std::env::set_var("TOMBOX_GDK_ROOT", dir.join("data"));
    std::env::set_var("TOMBOX_CONSOLE_OSBUILD", "22000");
    dir
}

unsafe fn init(flags: u32, a5: u32, drv: &[u8], drv_bytes: u32, out: *mut c_void, out_bytes: u32) -> i32 {
    initialize(0x55f0_114d, 0x35dbc, flags, 2, a5, 0x20004, drv.as_ptr(), drv_bytes, out, out_bytes)
}

fn idle(s: &Singleton) {
    assert_eq!(s.instrument.owner.load(Ordering::SeqCst), u64::MAX);
}

#[test]
fn pix_dll_initialize_without_a_tool_follows_the_22000_console() {
    let dir = setup();
    assert!(bind_xbtp(XbtpFns {
        session_io: || panic!("22000 must close an accepted link before resolving session I/O"),
        init_system: xbtp::link::XbtpLinkInitializeSystem,
        create: xbtp::link::XbtpLinkCreateHandle,
        listen: unsafe { core::mem::transmute::<usize, unsafe extern "C" fn(*mut u8, *mut c_void, u16, *mut usize) -> i32>(xbtp::link::XbtpLinkListen as *const () as usize) },
        accept: xbtp::link::XbtpLinkAccept,
        close: xbtp::link::XbtpLinkCloseHandle,
    }));
    let drv: Vec<u8> = (0..0x100u32).map(|i| (i as u8) ^ 0x5a).collect();
    unsafe {
        assert_eq!(pixrt10::driver::slot8(), E_UNEXPECTED);
        assert_eq!(slot9(), E_UNEXPECTED);
        assert_eq!(slot41(), 1);
        assert_eq!(slot6(0, 0), E_UNEXPECTED);
        teardown();
        assert_eq!(refs(), 0);

        let host = gate_host_path().unwrap();
        assert!(host.starts_with(dir.join("data")), "{}", host.display());
        let exe = std::env::current_exe().unwrap();
        assert_eq!(host.file_name(), exe.file_name());
        assert!(gate_path().unwrap().starts_with(PIX_GATE_DIR));
        assert!(!gate());
        let mut out = [0xaaaa_aaaa_aaaa_aaaau64; 44];
        assert_eq!(init(1, 0, &drv, 0xf8, out.as_mut_ptr() as *mut c_void, 0x150), 0x8000_4001u32 as i32);
        assert!(singleton().is_none());
        assert_eq!(refs(), 0);
        assert!(out.iter().all(|&v| v == 0xaaaa_aaaa_aaaa_aaaa));

        let mut t = [0u64; 16];
        assert_eq!(from_title(0x55f0_114d, 0x35dbc, 0, t.as_mut_ptr() as *mut c_void, 0x78), 0);
        let s = singleton().unwrap();
        let rt = runtime::runtime().unwrap();
        assert!(!pixrt10::state().overflow_alloc.load(Ordering::SeqCst));
        idle(s);

        assert_eq!(init(0, 0, &drv, 0x100, out.as_mut_ptr() as *mut c_void, 0x150), 0);
        assert_eq!(refs(), 1);
        assert!(pixrt10::state().overflow_alloc.load(Ordering::SeqCst));
        assert!(core::ptr::eq(singleton().unwrap(), s));
        assert!(core::ptr::eq(runtime::runtime().unwrap(), rt));
        assert_eq!(s.driver_table.lock().unwrap()[..], drv[..DRIVER_TABLE_BYTES]);
        assert_eq!(rt.driver_table.load(Ordering::SeqCst), s.driver_table.lock().unwrap().as_ptr() as usize);
        assert_eq!(rt.instrument_mode.load(Ordering::SeqCst), 0);
        assert_eq!(s.magic.load(Ordering::SeqCst), 0x55f0_114d);
        assert_eq!((s.arg4.load(Ordering::SeqCst), s.arg5.load(Ordering::SeqCst), s.arg6.load(Ordering::SeqCst)), (2, 0, 0x20004));
        let want = table(&INIT_22000, false);
        assert_eq!(want.len(), 42);
        assert_eq!(out[..42], want.iter().map(|&v| v as u64).collect::<Vec<_>>()[..]);
        assert_eq!(out[42..], [0xaaaa_aaaa_aaaa_aaaa; 2]);
        let nulls: Vec<usize> = (0..42).filter(|&i| out[i] == 0).collect();
        assert_eq!(nulls, vec![9]);
        assert_eq!(out[20], thread_event_buffer as *const () as usize as u64);
        assert_eq!(out[21], capture_flags as *const () as usize as u64);
        let cap = table(&INIT_22000, true);
        assert_eq!((0..42).filter(|&i| cap[i] == 0).collect::<Vec<_>>(), vec![12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 37, 38, 39, 40]);
        assert_eq!(cap[9], slot9 as *const () as usize);

        let mut part = [0x11u8; 24];
        assert_eq!(init(0, 0, &drv, 0, part.as_mut_ptr() as *mut c_void, 13), 0);
        assert_eq!(refs(), 2);
        assert_eq!(part[..13], want[0].to_le_bytes().iter().chain(want[1].to_le_bytes().iter()).copied().take(13).collect::<Vec<u8>>()[..]);
        assert_eq!(part[13..], [0x11u8; 11]);
        assert_eq!(s.driver_table.lock().unwrap()[..], drv[..DRIVER_TABLE_BYTES]);
        assert_eq!(init(0, 0, &drv, 0x10, core::ptr::null_mut(), 0x150), 0);
        assert_eq!(refs(), 3);

        std::fs::create_dir_all(host.parent().unwrap()).unwrap();
        std::fs::write(&host, b"").unwrap();
        assert!(gate());
        let mut big = [0x77u64; 44];
        assert_eq!(init(1, 0, &drv, 0xf8, big.as_mut_ptr() as *mut c_void, 0x200), 0);
        assert_eq!(refs(), 4);
        assert_eq!(big[..42], out[..42]);
        assert_eq!(big[42..], [0x77; 2]);

        let found = pixie_entry_points();
        let first = init(0, 1, &drv, 0xf8, core::ptr::null_mut(), 0);
        assert_eq!(first, if found { 0x8000_4001u32 as i32 } else { E_UNEXPECTED });
        assert_eq!(refs(), 5);
        assert_eq!(s.instrument.started.load(Ordering::SeqCst), found);
        assert_eq!(s.arg5.load(Ordering::SeqCst), 1);
        assert_eq!(rt.instrument_mode.load(Ordering::SeqCst), 1);
        assert_eq!(init(0, 1, &drv, 0xf8, core::ptr::null_mut(), 0), first);
        assert_eq!(s.instrument.started.load(Ordering::SeqCst), found);
        assert_eq!(init(0, 0, &drv, 0xf8, core::ptr::null_mut(), 0), 0);
        assert_eq!(refs(), 7);
        idle(s);

        assert_eq!(pixrt10::driver::slot8(), E_UNEXPECTED);
        assert_eq!(slot9(), 0x8000_4001u32 as i32);
        assert_eq!(slot7(0, 0, 0, 0), E_UNEXPECTED);
        assert_eq!(slot12(), 0);
        slot18();
        assert_eq!(slot6(0, 1), S_FALSE);
        assert_eq!(slot24(0, 0), S_FALSE);
        assert_eq!(slot34(0), 0);
        assert_eq!(slot5(2, 0), E_FAIL);
        assert_eq!(slot5(0, 0), E_FAIL);
        idle(s);
        assert_eq!(slot11(0, 0), S_FALSE);
        assert_eq!(slot35(0, 0), S_FALSE);
        rt.capture_session.store(1, Ordering::SeqCst);
        assert_eq!(slot11(0, 0), S_FALSE);
        rt.capture_session.store(0, Ordering::SeqCst);

        s.instrument.active.store(true, Ordering::SeqCst);
        assert_eq!(pixrt10::driver::slot8(), E_FAIL);
        s.instrument.level.store(-1, Ordering::SeqCst);
        assert_eq!(slot6(0, 0), S_FALSE);
        assert_eq!(slot34(0), 0);
        s.instrument.level.store(0, Ordering::SeqCst);
        s.instrument.active.store(false, Ordering::SeqCst);
        idle(s);

        assert_eq!(slot41(), 0);
        rt.device_5bc.store(1, Ordering::SeqCst);
        assert_eq!(slot41(), 1);
        rt.device_5b8.store(1, Ordering::SeqCst);
        assert_eq!(slot41(), 0);
        rt.device_5bc.store(0, Ordering::SeqCst);
        rt.device_5b8.store(0, Ordering::SeqCst);

        let ext: Vec<u8> = (0..0x40u8).collect();
        slot40(ext.as_ptr());
        assert_eq!(s.ext_block.lock().unwrap()[..], ext[..]);

        assert_eq!(slot22(1), 0);
        assert_eq!(capture_flags(), 2);
        assert_eq!(slot23(0x8001), 0);
        assert_eq!(capture_flags(), 0x8003);
        assert_eq!(slot22(0), 0);
        assert_eq!(slot23(0), 0);
        assert_eq!(capture_flags(), 0);
        assert_eq!(thread_event_buffer(0), 0);

        let cfg = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut o = [0u8; 0x18];
        assert_eq!(slot13(core::ptr::null(), o.as_mut_ptr()), E_POINTER);
        assert_eq!(slot13(cfg.as_ptr(), core::ptr::null_mut()), E_POINTER);
        assert_eq!(timing_config(), [0; 12]);
        assert_eq!(slot13(cfg.as_ptr(), o.as_mut_ptr()), 0x8000_4001u32 as i32);
        assert_eq!(timing_config(), cfg);
        slot14();
        assert_eq!(timing_config(), [0; 12]);

        slot17(0x1000);
        let words = [1u64, 2];
        slot19(core::ptr::null(), 2);
        slot19(words.as_ptr(), 2);
        slot38(0);
        slot38(0x1234);
        let hdr = [8u16, 0, 0, 0];
        slot39(core::ptr::null());
        slot39(hdr.as_ptr());
        let kind = 3u32;
        slot37(core::ptr::null(), core::ptr::null(), 0);
        slot37(&kind, core::ptr::null(), 0);

        for _ in 0..6 {
            teardown();
        }
        assert_eq!(refs(), 1);
        assert_eq!(s.driver_table.lock().unwrap()[..], drv[..DRIVER_TABLE_BYTES]);
        teardown();
        assert_eq!(refs(), 0);
        assert_eq!(s.driver_table.lock().unwrap()[..], [0u8; DRIVER_TABLE_BYTES]);
        assert_eq!((s.arg4.load(Ordering::SeqCst), s.arg5.load(Ordering::SeqCst), s.arg6.load(Ordering::SeqCst)), (0, 0, 0));
        assert!(core::ptr::eq(singleton().unwrap(), s));
        teardown();
        assert_eq!(refs(), -1);
        assert_eq!(from_title(1, 2, 0, core::ptr::null_mut(), 0), 0);
        assert_eq!(refs(), -1);
    }
}

#[test]
fn gate_tail_follows_0x226e8() {
    assert_eq!(image_tail(b"\\Device\\HarddiskVolume3\\t\\Title.exe"), Some(b"Title.exe".to_vec()));
    assert_eq!(image_tail(b"Title.exe"), None);
    assert_eq!(image_tail(b"\\Device\\x\\"), None);
    assert_eq!(image_tail(b""), None);
    let mut long = b"\\".to_vec();
    long.extend(std::iter::repeat(b'a').take(GATE_BUFFER - PIX_GATE_DIR.len() - 1));
    assert_eq!(image_tail(&long).map(|t| t.len()), Some(GATE_BUFFER - PIX_GATE_DIR.len() - 1));
    long.push(b'a');
    assert_eq!(image_tail(&long), None);
    assert_eq!(init_layout(22000), Some(INIT_22000));
    assert_eq!(init_layout(22620), Some(INIT_22000));
    assert_eq!(init_layout(22621), Some(INIT_22621));
    assert_eq!(init_layout(19041), None);
    assert_eq!(INIT_22000.slots.len() * 8, INIT_22000.table_bytes);
    assert_eq!(INIT_22621.slots.len() * 8, INIT_22621.table_bytes);
    assert_eq!(table(&INIT_22621, false).len(), 43);
    assert_eq!(table(&INIT_22621, false)[42], slot42 as *const () as usize);
    assert_eq!(table(&INIT_22621, true)[42], 0);
}

fn section_offset(img: &[u8], rva: usize) -> usize {
    let pe = u32::from_le_bytes(img[0x3c..0x40].try_into().unwrap()) as usize;
    let nsec = u16::from_le_bytes(img[pe + 6..pe + 8].try_into().unwrap()) as usize;
    let opt = u16::from_le_bytes(img[pe + 20..pe + 22].try_into().unwrap()) as usize;
    let sec = pe + 24 + opt;
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
}

fn tables_from_dump(img: &[u8], l: &InitLayout, flag: u32) -> (Vec<u32>, Vec<u32>) {
    let base = l.entry as usize;
    let off = section_offset(img, base);
    let code = &img[off..off + 0x600];
    let mut ev: Vec<(usize, usize, u32)> = Vec::new();
    let mut pending: Option<u32> = None;
    let mut split = 0usize;
    let mut cmp_at = 0usize;
    let mut join = 0usize;
    let mut end = code.len();
    let mut i = 0;
    while i + 8 <= code.len() {
        let w = &code[i..];
        if w[0] == 0x48 && w[1] == 0x8d && w[2] == 0x05 {
            let d = i32::from_le_bytes(w[3..7].try_into().unwrap());
            pending = Some((base as i64 + i as i64 + 7 + d as i64) as u32);
            i += 7;
            continue;
        }
        if w[0] == 0x48 && w[1] == 0x89 && (w[2] == 0x44 || w[2] == 0x5c) && w[3] == 0x24 {
            let v = if w[2] == 0x44 { pending.take().unwrap_or(u32::MAX) } else { 0 };
            ev.push((i, w[4] as usize, v));
            i += 5;
            continue;
        }
        if w[0] == 0x48 && w[1] == 0x89 && (w[2] == 0x84 || w[2] == 0x9c) && w[3] == 0x24 {
            let v = if w[2] == 0x84 { pending.take().unwrap_or(u32::MAX) } else { 0 };
            ev.push((i, u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, v));
            i += 8;
            continue;
        }
        if w[0] == 0x38 && w[1] == 0x98 && u32::from_le_bytes(w[2..6].try_into().unwrap()) == flag && w[6] == 0x0f && w[7] == 0x85 {
            let d = i32::from_le_bytes(code[i + 8..i + 12].try_into().unwrap());
            split = (i as i64 + 12 + d as i64) as usize;
            cmp_at = i;
            i += 12;
            continue;
        }
        if split != 0 && join == 0 && i + 5 == split && w[0] == 0xe9 {
            let d = i32::from_le_bytes(w[1..5].try_into().unwrap());
            join = (i as i64 + 5 + d as i64) as usize;
        }
        if split != 0 && join == 0 && i + 2 == split && w[0] == 0xeb {
            join = (i as i64 + 2 + w[1] as i8 as i64) as usize;
        }
        if join != 0 && w[0] == 0x48 && w[1] == 0x8d && w[2] == 0x54 && w[3] == 0x24 && w[4] == 0x60 {
            end = i;
            break;
        }
        i += 1;
    }
    assert!(split != 0 && join > split, "{:#x}", l.entry);
    let n = l.table_bytes / 8;
    let build = |ranges: &[(usize, usize)]| {
        let mut t = vec![0u32; n];
        for &(a, b) in ranges {
            for &(at, o, v) in &ev {
                if at >= a && at < b && (0x60..0x60 + l.table_bytes).contains(&o) {
                    t[(o - 0x60) / 8] = v;
                }
            }
        }
        t
    };
    let default = build(&[(0, split), (join, end)]);
    let capture = build(&[(0, cmp_at), (split, join), (join, end)]);
    assert!(code.windows(6).any(|w| w[0] == 0x41 && w[1] == 0xb8 && u32::from_le_bytes(w[2..6].try_into().unwrap()) as usize == l.table_bytes));
    (default, capture)
}

#[test]
fn table_layouts_match_the_console_dlls() {
    let dumps = [
        (r"D:\xbox-pc-port\games\XDefiant\extracted\EmbeddedXvd\Windows\System32\pixrt10.dll", INIT_22000, LAYOUT_22000, 0x95b28u32),
        (r"D:\xbox-pc-port\games\Trackmania\extracted\Ubisoft.TrackmaniaNext_1.6.2.0_neutral__ngz4m417e0mpw\EmbeddedXvd\Windows\System32\pixrt10.dll", INIT_22621, LAYOUT_22621, 0x95ba0),
    ];
    for (path, l, rl, flag) in dumps {
        let Ok(img) = std::fs::read(path) else { continue };
        let (d, c) = tables_from_dump(&img, &l, flag);
        assert_eq!(d, l.slots, "{path}");
        assert_eq!(c, l.capture_slots, "{path}");
        let e = section_offset(&img, l.entry as usize);
        let code = &img[e..e + 0x100];
        assert!(code.windows(5).any(|w| w[0] == 0xb9 && u32::from_le_bytes(w[1..5].try_into().unwrap()) as usize == rl.singleton_bytes), "{path}");
        let g = section_offset(&img, l.gate as usize);
        assert!(img[g..g + 0x60].windows(8).any(|w| w == b"c:\\pix\\\0"), "{path}");
        let di = section_offset(&img, l.driver_init as usize);
        assert!(img[di..di + 0x80].windows(5).any(|w| w[0] == 0xb9 && u32::from_le_bytes(w[1..5].try_into().unwrap()) as usize == rl.runtime_bytes), "{path}");
        assert_eq!(img[di..di + 0x50].windows(4).any(|w| w == [0xc6, 0x40, 0x0a, 0x01]), l.sets_overflow_alloc, "{path}");
    }
    let Ok(img) = std::fs::read(r"D:\xbox-pc-port\games\XDefiant\extracted\EmbeddedXvd\Windows\System32\pixrt10.dll") else { return };
    let msg: Vec<u8> = MSG_SIMULTANEOUS.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    assert!(img.windows(msg.len()).any(|w| w == msg.as_slice()));
    let pix: Vec<u8> = PIXIE_DLL.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    assert!(img.windows(pix.len()).any(|w| w == pix.as_slice()));
    assert!(img.windows(PIXIE_EXPORT.len()).any(|w| w == PIXIE_EXPORT));
    let c = section_offset(&img, 0x38d3);
    assert_eq!(&img[c..c + 3], &[0x8d, 0x57, 0xdd]);
}
