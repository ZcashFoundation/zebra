//! Build script for zebra-state.
//!
//! Turns the environmental variable `$TEST_FAKE_ACTIVATION_HEIGHTS`
//! into the Rust configuration `cfg(test_fake_activation_heights)`.

use std::env;

fn main() {
    let use_fake_heights = env::var_os("TEST_FAKE_ACTIVATION_HEIGHTS").is_some();
    println!("cargo:rerun-if-env-changed=TEST_FAKE_ACTIVATION_HEIGHTS");
    if use_fake_heights {
        println!("cargo:rustc-cfg=test_fake_activation_heights");
    }
}
