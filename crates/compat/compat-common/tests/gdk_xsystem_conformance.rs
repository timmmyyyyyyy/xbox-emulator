use compat_common::gdk;
use core::ffi::c_void;
use std::sync::OnceLock;

#[macro_use]
#[path = "support/xsys.rs"]
mod xsys;

const DIFFERS: &[(&str, &str)] = &[];

unsafe extern "system" fn query(provider: *const xsys::Guid, iid: *const xsys::Guid, out: *mut *mut c_void) -> i32 {
    gdk::query_api_impl(provider as *const gdk::Guid, iid as *const gdk::Guid, out)
}

fn setup(scenario: &str) -> bool {
    if let Some((_, what)) = DIFFERS.iter().find(|(n, _)| *n == scenario) {
        if std::env::var("TOMBOX_XSYS_ALL").as_deref() != Ok("1") {
            eprintln!("SKIP: {scenario}: {what}");
            return false;
        }
    }
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        std::env::set_var("TOMBOX_GDK_ROOT", std::env::temp_dir().join(format!("tombox_gdk_xsys_{}", std::process::id())));
        if gdk::title().title_id.is_some() {
            eprintln!("SKIP: a MicrosoftGame.config with a TitleId was found above the test executable");
            return false;
        }
        xsys::install(query);
        true
    })
}

xsys_tests!(setup);
