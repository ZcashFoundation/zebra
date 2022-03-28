//! ![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)
//!
//! Hello! I am Zebra, an ongoing Rust implementation of a Zcash node.
//!
//! Zebra is a work in progress.  It is developed as a collection of `zebra-*`
//! libraries implementing the different components of a Zcash node (networking,
//! chain structures, consensus rules, etc), and a `zebrad` binary which uses them.
//!
//! Most of our work so far has gone into `zebra-network`, building a new
//! networking stack for Zcash, and `zebra-chain`, building foundational data
//! structures.
//!
//! [Rendered docs from the `main` branch](https://doc.zebra.zfnd.org).
//!
//! [Join us on the Zcash Foundation Engineering Discord](https://discord.gg/na6QZNd).

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
pub mod sentry;
