//! Build script for zebrad.
//!
//! Turns Zebra version information into build-time environmental variables,
//! so that it can be compiled into `zebrad`, and used in diagnostics.
//!
//! When compiling the `lightwalletd` gRPC tests, also builds a gRPC client
//! Rust API for `lightwalletd`.

use std::process::Command;
use vergen::EmitBuilder;

/// Returns a new `vergen` builder, configured for everything except for `git` env vars.
/// This builder fails the build on error.
fn base_vergen_builder() -> EmitBuilder {
    let mut vergen = EmitBuilder::builder();

    vergen.all_cargo().all_rustc();

    vergen
}

/// Run a git command and return the output, or a fallback value if it fails
fn run_git_command(args: &[&str], fallback: &str) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// Process entry point for `zebrad`'s build script
#[allow(clippy::print_stderr)]
fn main() {
    let mut vergen = base_vergen_builder();

    vergen.all_git().git_sha(true);
    // git adds a "-dirty" flag if there are uncommitted changes.
    // This doesn't quite match the  SemVer 2.0 format, which uses dot separators.
    vergen.git_describe(true, true, Some("v*.*.*"));

    // Disable git if we're building with an invalid `zebra/.git`
    match vergen.fail_on_error().emit() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("git error in vergen build script: skipping git env vars: {e:?}",);
            base_vergen_builder()
                .emit()
                .expect("non-git vergen should succeed");
        }
    }

    #[cfg(feature = "lightwalletd-grpc-tests")]
    tonic_build::configure()
        .build_client(true)
        .build_server(false)
        // The lightwalletd gRPC types don't use floats or complex collections,
        // so we can derive `Eq` as well as the default generated `PartialEq` derive.
        // This fixes `clippy::derive_partial_eq_without_eq` warnings.
        .message_attribute(".", "#[derive(Eq)]")
        .compile_protos(
            &["tests/common/lightwalletd/proto/service.proto"],
            &["tests/common/lightwalletd/proto"],
        )
        .expect("Failed to generate lightwalletd gRPC files");

    // Add custom git tag and commit information
    // Use environment variables if available (from CI/CD), otherwise try git commands (Can be problematic as the docker image shouldn't have the .git folder in it)
    let git_tag = std::env::var("GIT_TAG")
        .unwrap_or_else(|_| run_git_command(&["describe", "--exact-match", "--tags"], "none"));
    let git_commit = std::env::var("GIT_COMMIT_FULL")
        .unwrap_or_else(|_| run_git_command(&["rev-parse", "HEAD"], "none"));

    println!("cargo:rustc-env=GIT_TAG={}", git_tag);
    println!("cargo:rustc-env=GIT_COMMIT_FULL={}", git_commit);
}
