//! Swap point between the real and the `loom` synchronization primitives.
//!
//! Only this crate's own unit tests built with
//! `RUSTFLAGS="--cfg zebra_loom -C debug-assertions"` see the loom types. Every other build,
//! including dependents and integration tests, sees the real types, so release code is
//! unchanged. See `book/src/dev/loom-testing.md`.

// The models rely on overflow checks: refuse a build that sets the cfg without them.
#[cfg(all(test, zebra_loom, not(debug_assertions)))]
compile_error!("loom models need RUSTFLAGS=\"--cfg zebra_loom -C debug-assertions\"");

#[cfg(all(test, zebra_loom))]
pub(crate) mod sync {
    pub(crate) mod atomic {
        pub(crate) use ::loom::sync::atomic::{AtomicUsize, Ordering};
    }
}

#[cfg(not(all(test, zebra_loom)))]
pub(crate) mod sync {
    pub(crate) mod atomic {
        pub(crate) use std::sync::atomic::{AtomicUsize, Ordering};
    }
}
