use core::ffi::c_void;

extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(h: *mut c_void, name: *const u8) -> *mut c_void;
}

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

fn guid(s: &str) -> Guid {
    let h: String = s.chars().filter(|c| *c != '-').collect();
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
    Guid(u32::from_str_radix(&h[0..8], 16).unwrap(), u16::from_str_radix(&h[8..12], 16).unwrap(), u16::from_str_radix(&h[12..16], 16).unwrap(), [b(16), b(18), b(20), b(22), b(24), b(26), b(28), b(30)])
}

type Query = unsafe extern "system" fn(*const Guid, *const Guid, *mut *mut c_void) -> i32;

const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

struct Family {
    name: &'static str,
    provider: &'static str,
    emulated: &'static str,
    same_table: &'static [&'static str],
    other_table: &'static [&'static str],
}

const FAMILIES: &[Family] = &[
    Family {
        name: "XGameSave",
        provider: "704c3f58-e629-4cc2-b197-30511b996fe2",
        emulated: "704c3f58-e629-4cc2-b197-30511b996fe2",
        same_table: &["704c3f58-e629-4cc2-b197-30511b996ee2", "1bfff3af-f14a-40a3-8e35-9ada906593f9", "ab4ae4fb-6508-4950-a032-45fd4bf8c43b"],
        other_table: &[],
    },
    Family {
        name: "XGameUi",
        provider: "dfcd4649-4ff8-4043-ba07-35d607df98b0",
        emulated: "72c4e70f-4a67-481b-bb84-aa72eda14fe5",
        same_table: &["6eeaa73e-9669-43ad-a2c7-d0da4e1f50a1", "fc0b9bd0-8898-4643-bee0-4ee6ee034cb6", "eaf669df-5542-4590-99a3-8dc061f837cc"],
        other_table: &[],
    },
    Family {
        name: "XSystem",
        provider: "e349bd1a-fc20-4e40-b99c-4178cc6b409f",
        emulated: "6fd71f09-7513-49f0-89bc-bfaf5df6f852",
        same_table: &["e349bd1a-fc20-4e40-b99c-4178cc6b409f", "67ce4bfc-b1d1-4ac7-bc3a-cb9219a97a85", "1861cf2e-e18b-4834-a9f5-b4a4e6efb4cf"],
        other_table: &[],
    },
    Family {
        name: "XUser",
        provider: "01acd177-91f9-4763-a38e-ccbb55ce32e0",
        emulated: "1bf2f8c5-d507-4e52-bb05-f726d0e71161",
        same_table: &["01acd177-91f9-4763-a38e-ccbb55ce32e0", "eb9bf948-18dc-4d82-bbcc-40e0a809c4c0", "26f3c674-a2fe-44fa-b6c4-a323bc94ff53"],
        other_table: &["cef4fac0-7676-4a94-a119-4c43f9eb5b74"],
    },
    Family {
        name: "XPackage",
        provider: "af406016-e850-4aa8-a88d-2f3dcb9dac7e",
        emulated: "f92d8712-2b27-4d8a-bf01-11a6f8e3eb42",
        same_table: &["be3f3e9f-c4b3-424d-9a95-d1af060559a5", "3720de07-e8e4-44a3-ad32-b359e8adbe55", "e2a4734b-2f4a-456d-aa8f-d065e04fb209"],
        other_table: &[],
    },
    Family {
        name: "XStore",
        provider: "0dd112ac-7c24-448c-b92b-3960fb5bd30c",
        emulated: "60b09f4e-1b85-45b1-826c-169118e230e1",
        same_table: &["de3dbdd4-0b37-4bdb-a10e-acf3a354d06a", "2d42fea5-e71d-4b76-97cd-c50afbb3ae5d", "b09d803c-2414-4a05-82c6-66dfdc9e9a44", "5c48dedf-0b67-4492-a4b5-6829b8e796e1"],
        other_table: &[],
    },
    Family {
        name: "XAppCapture",
        provider: "a4f1aee2-4bf1-4485-b008-a7c26d52ac27",
        emulated: "3a949778-772e-4799-bdea-0a6639e96baa",
        same_table: &["22e672d7-b4e3-406c-bd50-8f0d25236f9e"],
        other_table: &[],
    },
    Family {
        name: "XGameStreaming",
        provider: "0a2192aa-b2d5-4d58-83be-383b6d80799e",
        emulated: "5f5e5169-746c-4001-ad1c-da728d01c9eb",
        same_table: &["57786622-6605-46d0-b917-0f22bbcd9c52"],
        other_table: &["aebdfe78-4394-49ff-ba3a-001956457044"],
    },
];

const UNNAMED_PROVIDER: (&str, &str) = ("95fd18d2-74dd-4d7c-aa1b-0b51827665d6", "026b010c-06c3-4cdd-bbcb-43f229db1cff");

unsafe fn table(query: Query, provider: &str, iid: &str) -> Result<usize, i32> {
    let mut obj: *mut c_void = core::ptr::null_mut();
    let hr = query(&guid(provider), &guid(iid), &mut obj);
    if hr < 0 {
        return Err(hr);
    }
    assert!(!obj.is_null(), "{provider} {iid}");
    Ok(*(obj as *const usize))
}

#[test]
fn the_pc_runtime_serves_every_version_of_a_family_from_one_method_table() {
    unsafe {
        let name: Vec<u16> = "xgameruntime.dll".encode_utf16().chain(Some(0)).collect();
        let h = LoadLibraryExW(name.as_ptr(), core::ptr::null_mut(), 0x800);
        if h.is_null() {
            eprintln!("SKIP: no System32 xgameruntime.dll (Gaming Services not installed)");
            return;
        }
        let init: unsafe extern "system" fn(u32, u32) -> i32 = core::mem::transmute(GetProcAddress(h, b"InitializeApiImpl\0".as_ptr()));
        let hr = init(0x4a61, 0x2712);
        if hr < 0 {
            eprintln!("SKIP: InitializeApiImpl failed hr={hr:#010x}");
            return;
        }
        let query: Query = core::mem::transmute(GetProcAddress(h, b"QueryApiImpl\0".as_ptr()));
        let mut checked = 0;
        for f in FAMILIES {
            let reference = match table(query, f.provider, f.emulated) {
                Ok(t) => t,
                Err(hr) => {
                    eprintln!("SKIP: {} {} hr={hr:#010x}", f.name, f.emulated);
                    continue;
                }
            };
            for iid in f.same_table {
                match table(query, f.provider, iid) {
                    Ok(t) => {
                        assert_eq!(t, reference, "{} {iid}", f.name);
                        checked += 1;
                    }
                    Err(E_NOINTERFACE) => eprintln!("SKIP: {} {iid} is not known to the installed runtime", f.name),
                    Err(hr) => panic!("{} {iid} hr={hr:#010x}", f.name),
                }
            }
            for iid in f.other_table {
                match table(query, f.provider, iid) {
                    Ok(t) => {
                        assert_ne!(t, reference, "{} {iid}", f.name);
                        checked += 1;
                    }
                    Err(E_NOINTERFACE) => eprintln!("SKIP: {} {iid} is not known to the installed runtime", f.name),
                    Err(hr) => panic!("{} {iid} hr={hr:#010x}", f.name),
                }
            }
        }
        match table(query, UNNAMED_PROVIDER.0, UNNAMED_PROVIDER.1) {
            Ok(_) => checked += 1,
            Err(hr) => eprintln!("SKIP: provider {} hr={hr:#010x}", UNNAMED_PROVIDER.0),
        }
        eprintln!("{checked} interface ids checked against the installed runtime");
        assert!(checked > 0);
    }
}
