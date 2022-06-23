//! ![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)
//!
//! Zebra is a Zcash node written in Rust.
//!
//! The `zebrad` binary uses a collection of `zebra-*` crates,
//! which implement the different components of a Zcash node
//! (networking, chain structures, validation, rpc, etc).
//!
//! [Rendered docs from the `main` branch](https://doc.zebra.zfnd.org).
//! [Join us on the Zcash Foundation Engineering Discord](https://discord.gg/na6QZNd).
//!
//! ## Zebra Feature Flags
//!
//! The following `zebrad` feature flags are available at compile time:
//!
//! ### Metrics
//!
//! * `prometheus`: export metrics to prometheus.
//!
//! Read the [metrics](https://zebra.zfnd.org/user/metrics.html) section of the book
//! for more details.
//!
//! ### Tracing
//!
//! Sending traces to different subscribers:
//! * `journald`: send tracing spans and events to `systemd-journald`.
//! * `sentry`: send crash and panic events to sentry.io.
//! * `flamegraph`: generate a flamegraph of tracing spans.
//!
//! Changing the traces that are collected:
//! * `filter-reload`: dynamically reload tracing filters at runtime.
//! * `error-debug`: enable extra debugging in release builds.
//! * `tokio-console`: enable tokio's `console-subscriber`.
//! * A set of features that [skip verbose tracing].
//!   The default features ignore `debug` and `trace` logs in release builds.
//!
//! Read the [tracing](https://zebra.zfnd.org/user/tracing.html) section of the book
//! for more details.
//!
//! [ignore verbose tracing]: https://docs.rs/tracing/0.1.35/tracing/level_filters/index.html#compile-time-filters
//!
//! ### Testing
//!
//! * `proptest-impl`: enable randomised test data generation.
//! * `lightwalletd-grpc-tests`: enable Zebra JSON-RPC tests that query `lightwalletd` using gRPC.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebrad")]
// Tracing causes false positives on this lint:
// https://github.com/tokio-rs/tracing/issues/553
#![allow(clippy::cognitive_complexity)]

#[macro_use]
extern crate tracing;

/// Error type alias to make working with tower traits easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub mod application;
pub mod commands;
pub mod components;
pub mod config;
pub mod prelude;

#[cfg(feature = "sentry")]
pub mod sentry;
