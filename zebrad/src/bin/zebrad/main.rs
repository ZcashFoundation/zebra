//! Main entry point for Zebrad

use zebrad::application::APPLICATION;

/// Process entry point for `zebrad`
fn main() {
    abscissa_core::boot(&APPLICATION);
}
