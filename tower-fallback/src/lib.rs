//! A service combinator that sends requests to a first service, then retries
//! processing on a second fallback service if the first service errors.
//!
//! Fallback designs have [a number of downsides][aws-fallback] but may be useful
//! in some cases. For instance, when using batch verification, the `Fallback`
//! wrapper can be used to fall back to individual verification of each item when
//! a batch fails to verify.
//!
//! TODO: compare with similar code in linkerd.
//!
//! [aws-fallback]: https://aws.amazon.com/builders-library/avoiding-fallback-in-distributed-systems/

// Standard lints
#![warn(missing_docs)]
#![allow(clippy::try_err)]
#![deny(clippy::await_holding_lock)]
#![forbid(unsafe_code)]

pub mod future;
mod service;

pub use self::service::Fallback;

/// A boxed type-erased `std::error::Error` that can be sent between threads.
pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;
