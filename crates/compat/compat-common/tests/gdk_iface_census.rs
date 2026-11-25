use compat_common::gdk::{self, Guid};
use core::ffi::c_void;

const NOT_EMULATED: &[(&str, &str, &str)] = &[
    ("XGameSave", "704c3f58-e629-4cc2-b197-30511b996fe2", "1bfff3af-f14a-40a3-8e35-9ada906593f9"),
    ("XGameSave", "704c3f58-e629-4cc2-b197-30511b996fe2", "ab4ae4fb-6508-4950-a032-45fd4bf8c43b"),
    ("XGameUi", "dfcd4649-4ff8-4043-ba07-35d607df98b0", "eaf669df-5542-4590-99a3-8dc061f837cc"),
    ("XSystem", "e349bd1a-fc20-4e40-b99c-4178cc6b409f", "1861cf2e-e18b-4834-a9f5-b4a4e6efb4cf"),
    ("XUser", "01acd177-91f9-4763-a38e-ccbb55ce32e0", "26f3c674-a2fe-44fa-b6c4-a323bc94ff53"),
    ("XPackage", "af406016-e850-4aa8-a88d-2f3dcb9dac7e", "e2a4734b-2f4a-456d-aa8f-d065e04fb209"),
    ("XStore", "0dd112ac-7c24-448c-b92b-3960fb5bd30c", "b09d803c-2414-4a05-82c6-66dfdc9e9a44"),
    ("XStore", "0dd112ac-7c24-448c-b92b-3960fb5bd30c", "5c48dedf-0b67-4492-a4b5-6829b8e796e1"),
    ("XAppCapture", "a4f1aee2-4bf1-4485-b008-a7c26d52ac27", "22e672d7-b4e3-406c-bd50-8f0d25236f9e"),
    ("XGameStreaming", "0a2192aa-b2d5-4d58-83be-383b6d80799e", "57786622-6605-46d0-b917-0f22bbcd9c52"),
    ("XGameStreaming", "0a2192aa-b2d5-4d58-83be-383b6d80799e", "aebdfe78-4394-49ff-ba3a-001956457044"),
    ("unnamed provider", "95fd18d2-74dd-4d7c-aa1b-0b51827665d6", "026b010c-06c3-4cdd-bbcb-43f229db1cff"),
];

#[test]
fn interface_ids_the_pc_runtime_serves_and_the_emulator_does_not_are_refused() {
    assert!(gdk::real_query().is_none());
    let served = gdk::implemented();
    for (family, provider, iid) in NOT_EMULATED {
        let (c, i) = (Guid::parse(provider), Guid::parse(iid));
        assert!(!served.iter().any(|e| e.0 == c && e.1 == i), "{family} {iid} is emulated now: remove its row");
        let mut obj = 1usize as *mut c_void;
        let hr = unsafe { gdk::query_api_impl(&c, &i, &mut obj) };
        assert_eq!(hr, gdk::E_NOINTERFACE, "{family} {iid}");
        assert!(obj.is_null(), "{family} {iid}");
    }
}

#[test]
fn every_refused_family_but_the_unnamed_one_is_emulated_under_another_id() {
    let served = gdk::implemented();
    for (family, provider, _) in NOT_EMULATED.iter().filter(|r| r.0 != "unnamed provider") {
        let c = Guid::parse(provider);
        assert!(served.iter().any(|e| e.0 == c), "{family}");
    }
}
