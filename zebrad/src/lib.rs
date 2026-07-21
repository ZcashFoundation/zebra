//! ![Zebra logotype](https://zfnd.org/wp-content/uploads/2022/03/zebra-logotype.png)
//!
//! Zebra is a Zcash full node written in Rust. Follow the [introductory
//! page](https://zebra.zfnd.org/index.html#documentation) in the Zebra Book to learn more.
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
//! The first Zcash node, `zcashd`, was originally created as a fork of Bitcoin
//! Core and is no longer maintained. Zebra is an independent Zcash node
//! implementation, written from scratch, and is the actively maintained node
//! for the network. Other implementations built on or alongside Zebra also
//! exist. Because they implement the same protocol, conforming nodes
//! interoperate and maintain the Zcash network together.
//!
//! ## Zebra Advantages
//!
//! These are some of the advantages or benefits of Zebra:
//!
//! - **Performance**: Zebra is built from scratch in an async, parallelized
//!   design, giving it high throughput for block validation and syncing.
//! - **Security**: Zebra is written in Rust, a memory-safe language, which
//!   reduces the risk of memory-safety and correctness bugs that could
//!   compromise the node or the environment it runs in.
//! - **Modularity**: Zebra is organized as a set of reusable crates
//!   (`zebra-chain`, `zebra-consensus`, `zebra-network`, `zebra-state`,
//!   `zebra-rpc`, and more). Wallets, indexers, and other tools can build on
//!   these directly instead of reimplementing core Zcash logic.
//! - **Broader deployment targets**: Its modular design makes it possible to run
//!   Zcash consensus code in a wider range of environments, including mobile and
//!   WebAssembly.
//! - **Open contribution**: A modern, well-documented Rust codebase lowers the
//!   barrier for new contributors, widening the pool of developers who can
//!   review, maintain, and extend the Zcash protocol.
//! - **Ecosystem foundation**: Because Zebra is independent and openly developed,
//!   other teams can build implementations, forks, and services on top of it,
//!   supporting a healthy and decentralized network.
//!
//! ## Configuration
//!
//! The command below places the generated `zebrad.toml` config file in the default preferences directory of Linux:
//!
//! ```console
//! zebrad generate -o ~/.config/zebrad.toml
//! ```
//!
//! See [`config::ZebradConfig`] for other OSes default locations or more information about how to configure Zebra.
//!
//! ## Zebra Feature Flags
//!
//! The following [Cargo
//! features](https://doc.rust-lang.org/cargo/reference/features.html#command-line-feature-options)
//! are available at compile time:
//!
//! ### Metrics
//!
//! * configuring a `tracing.progress_bar`: shows key metrics in the terminal using progress bars,
//!   and automatically configures Zebra to send logs to a file.
//!   (The `progress-bar` feature is activated by default.)
//! * `prometheus`: export metrics to prometheus.
//!
//! Read the [metrics](https://zebra.zfnd.org/user/metrics.html) section of the book
//! for more details.
//!
//! ### Tracing
//!
//! Sending traces to different subscribers:
//! * configuring a `tracing.log_file`: appends traces to a file on disk.
//! * `journald`: send tracing spans and events to `systemd-journald`.
//! * `sentry`: send crash and panic events to sentry.io.
//! * `flamegraph`: generate a flamegraph of tracing spans.
//!
//! Changing the traces that are collected:
//! * `filter-reload`: dynamically reload tracing filters at runtime.
//! * `error-debug`: enable extra debugging in release builds.
//! * `tokio-console`: enable tokio's `console-subscriber` (needs [specific compiler flags])
//! * A set of features that [skip verbose tracing].
//!   The default features ignore `debug` and `trace` logs in release builds.
//!
//! Read the [tracing](https://zebra.zfnd.org/user/tracing.html) section of the book
//! for more details.
//!
//! [skip verbose tracing]: https://docs.rs/tracing/0.1.35/tracing/level_filters/index.html#compile-time-filters
//! [specific compiler flags]: https://zebra.zfnd.org/dev/tokio-console.html#setup
//!
//! ### Testing
//!
//! * `proptest-impl`: enable randomised test data generation.
//! * `lightwalletd-grpc-tests`: enable Zebra JSON-RPC tests that query `lightwalletd` using gRPC.
//!
//! ### Experimental
//!
//! * `elasticsearch`: save block data into elasticsearch database. Read the [elasticsearch](https://zebra.zfnd.org/user/elasticsearch.html)
//!   section of the book for more details.
//! * `internal-miner`: enable experimental support for mining inside Zebra, without an external
//!   mining pool. This feature is only supported on testnet. Use a GPU or ASIC on mainnet for
//!   efficient mining.
//!
//! ## Zebra crates
//!
//! [The Zebra monorepo](https://github.com/ZcashFoundation/zebra) is a collection of the following
//! crates:
//!
//! - [tower-batch-control](https://docs.rs/tower-batch-control/latest/tower_batch_control/)
//! - [tower-fallback](https://docs.rs/tower-fallback/latest/tower_fallback/)
//! - [zebra-chain](https://docs.rs/zebra-chain/latest/zebra_chain/)
//! - [zebra-consensus](https://docs.rs/zebra-consensus/latest/zebra_consensus/)
//! - [zebra-network](https://docs.rs/zebra-network/latest/zebra_network/)
//! - [zebra-node-services](https://docs.rs/zebra-node-services/latest/zebra_node_services/)
//! - [zebra-rpc](https://docs.rs/zebra-rpc/latest/zebra_rpc/)
//! - [zebra-script](https://docs.rs/zebra-script/latest/zebra_script/)
//! - [zebra-state](https://docs.rs/zebra-state/latest/zebra_state/)
//! - [zebra-test](https://docs.rs/zebra-test/latest/zebra_test/)
//! - [zebra-utils](https://docs.rs/zebra-utils/latest/zebra_utils/)
//! - [zebrad](https://docs.rs/zebrad/latest/zebrad/)
//!
//! The links in the list above point to the documentation of the public APIs of the crates. For
//! the documentation of the internal APIs, follow <https://zebra.zfnd.org/internal> that lists
//! all Zebra crates as well in the left sidebar.

#![doc(html_favicon_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://zfnd.org/wp-content/uploads/2022/03/zebra-icon.png")]
#![doc(html_root_url = "https://docs.rs/zebrad")]
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
pub(crate) mod sentry;
