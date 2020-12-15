//! Main entry point for Zebrad

#![deny(missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use zebrad::application::APPLICATION;

/// Boot Zebrad
fn main() {
    abscissa_core::boot(&APPLICATION);
}
