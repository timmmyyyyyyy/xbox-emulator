use core::ffi::c_void;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetCurrentProcess() -> *mut c_void;
}

type Ws = unsafe extern "system" fn(u32, *mut u8) -> i32;

fn xmem() -> *mut c_void {
    let exe = std::env::current_exe().unwrap();
    let p = exe.ancestors().skip(1).flat_map(|a| [a.join("xmem.dll"), a.join("debug").join("xmem.dll")]).find(|p| p.is_file()).expect("build xmem first");
    let w: Vec<u16> = p.to_str().unwrap().encode_utf16().chain(Some(0)).collect();
    let h = unsafe { LoadLibraryW(w.as_ptr()) };
    assert!(!h.is_null());
    h
}

fn export(module: *mut c_void, name: &std::ffi::CStr) -> *mut c_void {
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) };
    assert!(!address.is_null(), "missing xmem export {name:?}");
    address
}

fn ws() -> Ws {
    unsafe { core::mem::transmute(export(xmem(), c"XMemGetWorkingSetStatistics")) }
}

#[test]
fn layout_19041_reports_the_first_title_allocation_in_4k_frames() {
    if std::env::var_os("XMEM_LAYOUT_TEST_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "layout_19041_reports_the_first_title_allocation_in_4k_frames"])
            .env("XMEM_LAYOUT_TEST_CHILD", "1")
            .env("TOMBOX_CONSOLE_OSBUILD", "19041")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    let module = xmem();
    unsafe {
        let query: extern "system" fn(*mut u64) -> i32 = core::mem::transmute(export(module, c"XMemGetPhysicalMemoryLayout"));
        let allocate: extern "system" fn(u64, *mut usize, *mut u64) -> i32 = core::mem::transmute(export(module, c"XMemAllocatePhysicalPages"));
        let free: extern "system" fn(usize, *mut u64) -> i32 = core::mem::transmute(export(module, c"XMemFreePhysicalPages"));
        let mut words = [u64::MAX; 4];
        assert_eq!(query(words.as_mut_ptr().add(1)), 1);
        let mut count = 2;
        let mut pages = [0; 2];
        assert_eq!(allocate(0x2040_0000, &mut count, pages.as_mut_ptr()), 1);
        assert_eq!(free(count, pages.as_mut_ptr()), 1);
        let first_frame = pages[0] * 16;
        assert_eq!(words, [u64::MAX, first_frame, first_frame, u64::MAX]);
        assert_eq!(pages[1], pages[0] + 1);
        assert_eq!(query(words.as_mut_ptr().add(1)), 1);
        assert_eq!(words[1..3], [first_frame; 2]);
    }
}

#[test]
fn allocation_is_queryable_through_the_dll_exports() {
    const SIZE: usize = 64;
    const MEM_COMMIT: u32 = 0x1000;
    const WS_V2_TITLE: u32 = 0x1000_0001;
    let module = xmem();
    unsafe {
        let alloc: extern "system" fn(usize, u64) -> *mut c_void =
            core::mem::transmute(export(module, c"XMemAlloc"));
        let free: extern "system" fn(*mut c_void, u64) =
            core::mem::transmute(export(module, c"XMemFree"));
        let query: unsafe extern "system" fn(*mut c_void, *const c_void, *mut u64) -> i32 =
            core::mem::transmute(export(module, c"XMemVirtualQuery"));
        let stats: Ws = core::mem::transmute(export(module, c"XMemGetWorkingSetStatistics"));

        let address = alloc(SIZE, 0);
        assert!(!address.is_null());
        let mut region = [0u64; 8];
        let result = query(GetCurrentProcess(), address, region.as_mut_ptr());
        let mut working_set = [0xcc; 0x58];
        let stats_result = stats(WS_V2_TITLE, working_set.as_mut_ptr());
        free(address, 0);

        assert_eq!(result, 1);
        assert_eq!(region[4] as u32, MEM_COMMIT);
        let offset = (address as u64).checked_sub(region[0]).expect("address is inside the region");
        assert!(offset + SIZE as u64 <= region[3]);
        assert_eq!(stats_result, 1);
        let total = u64::from_le_bytes(working_set[..8].try_into().unwrap());
        let used = u64::from_le_bytes(working_set[8..16].try_into().unwrap());
        assert!(used > 0 && used <= total);
        assert_eq!(working_set[0x50..], [0xcc; 8]);
    }
}

fn d(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn near(a: u32, b: u32) {
    assert!(a.abs_diff(b) <= 0x100_0000, "{a:#x} vs {b:#x}: live pool bytes drifted too far");
}

#[test]
fn v2_copies_internal_pool_and_commit_limit_like_console_2a50() {
    let f = ws();
    let mut int = [0u8; 0x78];
    let mut v2 = [0xccu8; 0x50];
    unsafe {
        assert_eq!(f(0x2000_0009, int.as_mut_ptr()), 1);
        assert_eq!(f(0x1000_0009, v2.as_mut_ptr()), 1);
    }
    assert_ne!(d(&int, 0x48), 0);
    near(d(&v2, 0x48), d(&int, 0x48));
    assert_eq!(d(&v2, 0x4c), d(&int, 0x4c));
}

#[test]
fn legacy_puts_internal_pool_bytes_at_0x24_like_console_2b44() {
    let f = ws();
    let mut int = [0u8; 0x78];
    let mut old = [0xccu8; 0x48];
    unsafe {
        assert_eq!(f(0x2000_0009, int.as_mut_ptr()), 1);
        assert_eq!(f(0x0000_0009, old.as_mut_ptr()), 1);
    }
    assert_ne!(d(&int, 0x48), 0);
    near(d(&old, 0x24), d(&int, 0x48));
}

#[test]
fn legacy_free_page_query_writes_two_triples_through_the_dll_export() {
    if std::env::var_os("XMEM_FREE_PAGE_TEST_CHILD").is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "legacy_free_page_query_writes_two_triples_through_the_dll_export"])
            .env("XMEM_FREE_PAGE_TEST_CHILD", "1")
            .env("TOMBOX_CONSOLE_OSBUILD", "19041")
            .status().unwrap();
        assert!(status.success());
        return;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetNumaHighestNodeNumber(highest: *mut u32) -> i32;
    }
    let mut highest = 0;
    assert_ne!(unsafe { GetNumaHighestNodeNumber(&mut highest) }, 0);
    let query: unsafe extern "system" fn(*mut u64) -> i32 =
        unsafe { core::mem::transmute(export(xmem(), c"XMemGetFreePageStatistics")) };
    let mut words = [u64::MAX; 8];
    let result = unsafe { query(words.as_mut_ptr().add(1)) };
    if highest > 1 {
        assert_eq!(result, 0);
        assert_eq!(words, [u64::MAX; 8]);
    } else {
        assert_eq!(result, 1);
        assert_eq!((words[0], words[7]), (u64::MAX, u64::MAX));
        assert!(!words[1..7].contains(&u64::MAX));
        if highest == 0 {
            assert_eq!(words[1..4], words[4..7]);
        }
    }
}

#[test]
fn query_19041_handles_free_noaccess_and_writable_regions() {
    if std::env::var_os("XMEM_QUERY_CHILD").is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "query_19041_handles_free_noaccess_and_writable_regions"])
            .env("XMEM_QUERY_CHILD", "1")
            .env("TOMBOX_CONSOLE_OSBUILD", "19041")
            .status().unwrap();
        assert!(result.success());
        return;
    }
    extern "system" {
        fn VirtualAlloc(base: usize, size: usize, kind: u32, protect: u32) -> usize;
        fn VirtualFree(base: usize, size: usize, kind: u32) -> i32;
    }
    unsafe {
        let query: unsafe extern "system" fn(*mut c_void, usize, *mut u64) -> i32 =
            core::mem::transmute(export(xmem(), c"XMemVirtualQuery"));
        let proc = GetCurrentProcess();
        let mut out = [u64::MAX; 9];
        assert_eq!(query(proc, 0, out.as_mut_ptr()), 1);
        assert_eq!(out[1], 0);
        assert!(out[3] > 0);
        assert_eq!(out[4] as u32, 0x10000);
        assert_eq!(out[4] >> 32, 1);
        assert_eq!(out[7], 0);
        assert_eq!(out[8], u64::MAX);
        for protect in [1, 4] {
            let base = VirtualAlloc(0, 0x10000, 0x3000, protect);
            assert_ne!(base, 0);
            let result = query(proc, base + 17, out.as_mut_ptr());
            assert_ne!(VirtualFree(base, 0, 0x8000), 0);
            assert_eq!(result, 1);
            assert_eq!(out[1] as usize, base);
            assert_eq!(out[3], 0x10000);
            assert_eq!(out[4] as u32, 0x1000);
            assert_eq!(out[4] >> 32, protect as u64);
            assert_eq!(out[8], u64::MAX);
        }
    }
}
