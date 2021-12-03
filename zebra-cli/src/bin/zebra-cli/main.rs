//! Main entry point for ZebraCli

#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use zebra_cli::application::APPLICATION;

/// Boot ZebraCli
fn main() {
    abscissa_core::boot(&APPLICATION);
}
