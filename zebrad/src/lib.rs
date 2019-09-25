//! Zebrad
//!
//! Application based on the [Abscissa] framework.
//!
//! [Abscissa]: https://github.com/iqlusioninc/abscissa

//#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]
// async_await is considered stable on nightly releases, but the corresponding
// release (1.39.0) is still in beta. Suppress the warning for now.
#![allow(stable_features)]
#![feature(async_await)]

#[macro_use]
extern crate tracing;
#[macro_use]
extern crate failure;

mod components;

pub mod application;
pub mod commands;
pub mod config;
pub mod error;
pub mod prelude;
