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
//! ## About Zcash
//!
//! Zcash is a cryptocurrency designed to preserve the user's privacy. Like most
//! cryptocurrencies, it works by a collection of software nodes run by members of
//! the Zcash community or any other interested parties. The nodes talk to each
//! other in peer-to-peer fashion in order to maintain the state of the Zcash
//! blockchain. They also communicate with miners who create new blocks. When a
//! Zcash user sends Zcash, their wallet broadcasts transactions to these nodes
//! which will eventually reach miners, and the mined transaction will then go
//! through Zcash nodes until they reach the recipient's wallet which will report
//! the received Zcash to the recipient.
//!
//! ## Alternative Implementations
//!
//! The original Zcash node is named `zcashd` and is developed by the Electric Coin
//! Company as a fork of the original Bitcoin node. Zebra, on the other hand, is
//! an independent Zcash node implementation developed from scratch. Since they
//! implement the same protocol, `zcashd` and Zebra nodes can communicate with each
//! other and maintain the Zcash network together.
//!
//! ## Zebra Advantages
//!
//! These are some of the advantages or benefits of Zebra:
//!
//! - Better performance: since it was implemented from scratch in an async, parallelized way, Zebra
//!   is currently faster than `zcashd`.
//! - Better security: since it is developed in a memory-safe language (Rust), Zebra
//!   is less likely to be affected by memory-safety and correctness security bugs that
//!   could compromise the environment where it is run.
//! - Better governance: with a new node deployment, there will be more developers
//!   who can implement different features for the Zcash network.
//! - Dev accessibility: supports more developers, which gives new developers
//!   options for contributing to Zcash protocol development.
//! - Runtime safety: with an independent implementation, the detection of consensus bugs
//!   can happen quicker, reducing the risk of consensus splits.
//! - Spec safety: with several node implementations, it is much easier to notice
//!   bugs and ambiguity in protocol specification.
//! - User options: different nodes present different features and tradeoffs for
//!   users to decide on their preferred options.
//! - Additional contexts: wider target deployments for people to use a consensus
//!   node in more contexts e.g. mobile, wasm, etc.
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
