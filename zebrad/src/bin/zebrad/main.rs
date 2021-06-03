//! Main entry point for Zebrad

// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]

use zebrad::application::APPLICATION;

/// Boot Zebrad
fn main() {
    abscissa_core::boot(&APPLICATION);
}
