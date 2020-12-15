//! ![Zebra logotype](https://www.zfnd.org/images/zebra-logotype.png)
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

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebrad")]
//#![deny(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]
// Tracing causes false positives on this lint:
// https://github.com/tokio-rs/tracing/issues/553
#![allow(clippy::cognitive_complexity)]

#[macro_use]
extern crate tracing;

mod components;

pub mod application;
pub mod commands;
pub mod config;
pub mod prelude;
pub mod sentry;
