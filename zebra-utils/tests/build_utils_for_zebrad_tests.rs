//! # Dependency Workaround
//!
//! This empty integration test makes `cargo` build the `zebra-checkpoints` binary for the `zebrad`
//! integration tests:
//!
//! > Binary targets are automatically built if there is an integration test or benchmark being
//! > selected to test.
//!
//! <https://doc.rust-lang.org/cargo/commands/cargo-test.html#target-selection>
//!
//! Each utility binary will only be built if its corresponding Rust feature is activated.
//! <https://doc.rust-lang.org/cargo/reference/cargo-targets.html#the-required-features-field>
//!
//! # Unstable `cargo` Feature
//!
//! When `cargo -Z bindeps` is stabilised, add a binary dependency to `zebrad/Cargo.toml` instead:
//! https://github.com/rust-lang/cargo/issues/9096
