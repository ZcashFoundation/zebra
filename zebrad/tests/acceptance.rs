//! Acceptance test: runs the application as a subprocess and asserts its
//! output for given argument combinations matches what is expected.
//!
//! Modify and/or delete these as you see fit to test the specific needs of
//! your application.
//!
//! For more information, see:
//! <https://docs.rs/abscissa_core/latest/abscissa_core/testing/index.html>

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use abscissa_core::testing::prelude::*;
use lazy_static::lazy_static;

lazy_static! {
    /// Executes your application binary via `cargo run`.
    ///
    /// Storing this value in a `lazy_static!` ensures that all instances of
    /// the runner acquire a mutex when executing commands and inspecting
    /// exit statuses, serializing what would otherwise be multithreaded
    /// invocations as `cargo test` executes tests in parallel by default.
    pub static ref RUNNER: CmdRunner = CmdRunner::default();
}

/// Example of a test which matches a regular expression
#[test]
fn version_no_args() {
    let mut runner = RUNNER.clone();
    let mut cmd = runner.arg("version").capture_stdout().run();
    cmd.stdout().expect_regex(r"\A\w+ [\d\.\-]+\z");
}
