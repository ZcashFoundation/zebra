//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use abscissa_core::testing::prelude::*;
use once_cell::sync::Lazy;

use std::time::Duration;

/// Executes zebrad binary via `cargo run`.
pub static RUNNER: Lazy<CmdRunner> = Lazy::new(CmdRunner::default);

#[test]
fn generate_no_args() {
    let mut runner = RUNNER.clone();
    let mut cmd = runner.arg("generate").capture_stdout().run();
    cmd.stdout()
        .expect_line("# Default configuration values for zebrad.");
    cmd.wait().unwrap().expect_success();
}

#[test]
fn generate_args() {
    let mut runner = RUNNER.clone();
    let cmd = runner
        .args(&["generate", "argument"])
        .capture_stdout()
        .run();
    cmd.wait().unwrap().expect_code(1);

    // Invalid flag
    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["generate", "-f"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    // Valid flag but missing argument
    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["generate", "-o"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    let mut runner = RUNNER.clone();
    let cmd = runner
        .args(&["generate", "-o", "file"])
        .capture_stdout()
        .run();
    cmd.wait().unwrap().expect_success();

    // Todo: Check if the file was created
}

#[test]
fn help_no_args() {
    let mut runner = RUNNER.clone();
    let cmd = runner.arg("help").capture_stdout().run();
    cmd.wait().unwrap().expect_success();
}

#[test]
fn help_args() {
    // Invalid argument
    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["help", "argument"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    // Invalid flag
    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["help", "-f"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);
}

#[test]
fn revhex_no_args() {
    let mut runner = RUNNER.clone();
    let _cmd = runner
        .timeout(Duration::from_secs(1))
        .arg("revhex")
        .capture_stdout()
        .run();

    // Program is waiting for input, we just exit after 1 second
}

#[test]
fn revhex_args() {
    let mut runner = RUNNER.clone();
    let mut cmd = runner.args(&["revhex", "33eeff55"]).capture_stdout().run();
    cmd.stdout().expect_line("55ffee33");
}

#[test]
fn seed_no_args() {
    let mut runner = RUNNER.clone();
    runner.exclusive();
    let mut cmd = runner
        .timeout(Duration::from_secs(1))
        .arg("seed")
        .capture_stdout()
        .run();

    // Todo: maybe add special info!() to seed command
    // Todo: improve the regex
    cmd.stdout()
        .expect_regex(r"^(.*?)Initializing tracing endpoint");

    // Problem: Zombie child process is not killed
    //cmd.kill();
}

#[test]
fn seed_args() {
    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["seed", "argument"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["seed", "-f"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["seed", "start"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);
}

#[test]
fn start_no_args() {
    let mut runner = RUNNER.clone();
    runner.exclusive();
    let mut cmd = runner
        .timeout(Duration::from_secs(1))
        .arg("start")
        .capture_stdout()
        .run();

    // Todo: maybe add special info!() to start command
    // Todo: improve the regex
    cmd.stdout()
        .expect_regex(r"^(.*?)Initializing tracing endpoint");

    // Problem: Zombie child process is not killed
}

#[test]
fn start_args() {
    // Bug? This should fail but not happening
    //let mut runner = RUNNER.clone();
    //let cmd = runner.args(&["start", "argument"]).capture_stdout().run();
    //cmd.wait().unwrap().expect_code(1);

    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["start", "-f"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    // Start + seed should be the only combination possible
    let mut runner = RUNNER.clone();
    let mut cmd = runner
        .timeout(Duration::from_secs(1))
        .args(&["start", "seed"])
        .capture_stdout()
        .run();
    cmd.stdout()
        .expect_regex(r"^(.*?)Initializing tracing endpoint");
}

#[test]
fn app_no_args() {
    let mut runner = RUNNER.clone();
    let mut cmd = runner.capture_stdout().run();

    // First line haves the version
    cmd.stdout().expect_regex(r"^(.*?)zebrad");
}

#[test]
fn version_no_args() {
    let mut runner = RUNNER.clone();
    let mut cmd = runner.arg("version").capture_stdout().run();
    cmd.stdout().expect_line("zebrad 0.1.0"); // Todo make regex
}

#[test]
fn version_args() {
    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["version", "argument"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);

    let mut runner = RUNNER.clone();
    let cmd = runner.args(&["version", "-f"]).capture_stdout().run();
    cmd.wait().unwrap().expect_code(1);
}
