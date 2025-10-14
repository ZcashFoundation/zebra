//! Build script for zebrad.
//!
//! Turns Zebra version information into build-time environmental variables,
//! so that it can be compiled into `zebrad`, and used in diagnostics.
//!
//! When compiling the `lightwalletd` gRPC tests, also builds a gRPC client
//! Rust API for `lightwalletd`.

use vergen_git2::{CargoBuilder, Emitter, Git2Builder, RustcBuilder};

/// Configures an [`Emitter`] for everything except for `git` env vars.
/// This builder fails the build on error.
fn add_base_emitter_instructions(emitter: &mut Emitter) {
    emitter
        .add_instructions(
            &CargoBuilder::all_cargo().expect("all_cargo() should build successfully"),
        )
        .expect("adding all_cargo() instructions should succeed")
        .add_instructions(
            &RustcBuilder::all_rustc().expect("all_rustc() should build successfully"),
        )
        .expect("adding all_rustc() instructions should succeed");
}

/// Process entry point for `zebrad`'s build script
#[allow(clippy::print_stderr)]
fn main() {
    let mut emitter = Emitter::default();
    add_base_emitter_instructions(&mut emitter);

    let all_git = Git2Builder::default()
        .branch(true)
        .commit_author_email(true)
        .commit_author_name(true)
        .commit_count(true)
        .commit_date(true)
        .commit_message(true)
        .commit_timestamp(true)
        .describe(false, false, None)
        .sha(true)
        .dirty(false)
        .describe(true, true, Some("v*.*.*"))
        .build()
        .expect("all_git + describe + sha should build successfully");

    emitter
        .add_instructions(&all_git)
        .expect("adding all_git + describe + sha instructions should succeed");

    // Disable git if we're building with an invalid `zebra/.git`
    match emitter.fail_on_error().emit() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("git error in vergen build script: skipping git env vars: {e:?}",);
            let mut emitter = Emitter::default();
            add_base_emitter_instructions(&mut emitter);
            emitter.emit().expect("base emit should succeed");
        }
    }

    #[cfg(feature = "lightwalletd-grpc-tests")]
    tonic_prost_build::configure()
        .build_client(true)
        .build_server(false)
        .compile_protos(
            &["tests/common/lightwalletd/proto/service.proto"],
            &["tests/common/lightwalletd/proto"],
        )
        .expect("Failed to generate lightwalletd gRPC files");
}
