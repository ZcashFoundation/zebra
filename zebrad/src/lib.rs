//! Zebrad
//!
//! Application based on the [Abscissa] framework.
//!
//! [Abscissa]: https://github.com/iqlusioninc/abscissa

//#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

#[macro_use]
extern crate tracing;

mod components;

pub mod application;
pub mod commands;
pub mod config;
pub mod error;
pub mod prelude;
