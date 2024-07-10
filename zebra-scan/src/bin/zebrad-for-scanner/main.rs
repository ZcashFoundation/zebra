//! Main entry point for Zebrad, to be used in zebra-scan tests

use zebrad::application::{boot, APPLICATION};

/// Process entry point for `zebrad`
fn main() {
    boot(&APPLICATION);
}
