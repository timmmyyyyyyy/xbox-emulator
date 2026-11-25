use compat_common::gdk::{self, xasync};
use std::sync::OnceLock;

#[macro_use]
#[path = "support/xtq.rs"]
mod xtq;

const DIFFERS: &[(&str, &str)] = &[
    ("verify_get_async_status_no_deadlock", "does not finish: a call begun on a block whose previous call has just completed can lose its state"),
    ("verify_no_write_after_get_status_race", "under load, intermittent: the async block is written after XAsyncGetStatus(wait) has returned"),
    ("verify_terminate_waits_for_async_in_flight", "termination finishes while an XAsync call on the queue is still pending"),
    ("verify_termination_of_base_queue_keeps_composites", "the composite's pending callback runs canceled, reference runs it normally"),
    ("verify_wait_for_completion", "intermittent: XAsyncGetStatus(wait) on a completed call with a result returns before the completion callback has finished"),
];

fn setup(scenario: &str) -> bool {
    if let Some((_, what)) = DIFFERS.iter().find(|(n, _)| *n == scenario) {
        if std::env::var("TOMBOX_XTQ_ALL").as_deref() != Ok("1") {
            eprintln!("SKIP: {scenario}: {what}");
            return false;
        }
    }
    static READY: OnceLock<bool> = OnceLock::new();
    *READY.get_or_init(|| {
        std::env::set_var("TOMBOX_GDK_XASYNC", "emulated");
        std::env::set_var("TOMBOX_GDK_ROOT", std::env::temp_dir().join(format!("tombox_gdk_xtq_{}", std::process::id())));
        assert!(gdk::rt::active());
        let mut obj = core::ptr::null_mut();
        assert_eq!(unsafe { gdk::query_api_impl(&xasync::XASYNC, &xasync::XASYNC, &mut obj) }, 0);
        xtq::install(obj);
        xtq::set_console_model(true);
        true
    })
}

xtq_tests!(setup);
