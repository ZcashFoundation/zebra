//! Swap point between `tokio` and `loom` synchronization primitives.
//!
//! Only this crate's own unit tests built with
//! `RUSTFLAGS="--cfg zebra_loom -C debug-assertions"` see the loom types. Every other build,
//! including dependents and integration tests, sees `tokio`, so release code is unchanged.
//! See `book/src/dev/loom-testing.md`.
//!
//! The loom channel stands in for tokio's unbounded channel only where it is used
//! synchronously: sent from any thread, drained with `try_recv` by its single owner. Unlike
//! tokio's `UnboundedReceiver`, loom's `Receiver` is not `Sync`, so a type holding one is
//! `Send` but not `Sync` in the loom build.

// The models rely on overflow checks (§2.6): refuse a build that sets the cfg without them.
#[cfg(all(test, zebra_loom, not(debug_assertions)))]
compile_error!("loom models need RUSTFLAGS=\"--cfg zebra_loom -C debug-assertions\"");

#[cfg(all(test, zebra_loom))]
pub(crate) mod sync {
    pub(crate) mod mpsc {
        pub(crate) use ::loom::sync::mpsc::{
            channel as unbounded_channel, Receiver as UnboundedReceiver, Sender as UnboundedSender,
        };
    }
}

#[cfg(not(all(test, zebra_loom)))]
pub(crate) mod sync {
    pub(crate) mod mpsc {
        pub(crate) use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
    }
}
