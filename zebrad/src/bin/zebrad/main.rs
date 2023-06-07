//! Main entry point for Zebrad

use zebrad::application::{boot, APPLICATION};

/// Process entry point for `zebrad`
fn main() {
    boot(&APPLICATION);
}
