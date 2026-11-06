//! The [`zebra_test::transcript`] error diagnostic, checked with a fixed color-eyre hook.
//!
//! This file must hold only this test. It installs the process-wide color-eyre hook, which can
//! only be installed once, and `cargo test` runs all tests of one file in a single process, where
//! `zebra_test::init` in another test may install it first.

use color_eyre::{
    config::{HookBuilder, Theme},
    Section as _,
};
use insta::assert_snapshot;
use regex::Regex;
use tower::Service;

use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

/// Wrong requests retain their expected and actual values in the error diagnostic.
#[tokio::test]
async fn transcript_rejects_wrong_request_for_error_entry() {
    // Keep machine-specific context and terminal styling out of the snapshot.
    HookBuilder::default()
        .theme(Theme::new())
        .display_env_section(false)
        .capture_span_trace_by_default(false)
        .install()
        .expect("this test is the only one in its process");

    let _init_guard = zebra_test::init();

    let mut svc = Transcript::from([("expected", Err::<(), _>(ExpectedTranscriptError::Any))]);
    let error = svc
        .call("wrong")
        .await
        .unwrap_err()
        .suppress_backtrace(true);
    let diagnostic = format!("{error:?}");
    // The `track-caller` feature of color-eyre, which `zebrad/error-debug` turns on, adds the
    // caller's file and line. The hook can only hide it when that feature is on, so drop it here.
    let diagnostic = Regex::new(r"\n\nLocation:\n[^\n]*")
        .expect("the pattern is valid")
        .replace(&diagnostic, "");

    assert_snapshot!(diagnostic.trim(), @r#"
    0: received unexpected request

    Expected Request:
       "expected"

    Found Request:
       "wrong"
    "#);
}
