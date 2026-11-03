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

/// `parking_lot::Mutex::lock()` is infallible, while loom's returns a `LockResult`, so the loom
/// half wraps loom's mutex and call sites are the same in both builds.
#[cfg(all(test, zebra_loom))]
pub(crate) mod parking_lot {
    /// A loom mutex with `parking_lot`'s infallible `lock()`.
    #[derive(Debug)]
    pub(crate) struct Mutex<T>(::loom::sync::Mutex<T>);

    impl<T> Mutex<T> {
        pub(crate) fn new(value: T) -> Self {
            Self(::loom::sync::Mutex::new(value))
        }

        #[track_caller]
        pub(crate) fn lock(&self) -> ::loom::sync::MutexGuard<'_, T> {
            self.0
                .lock()
                .expect("loom's lock() always returns Ok: it panics itself on a poisoned lock")
        }
    }
}

#[cfg(not(all(test, zebra_loom)))]
pub(crate) mod parking_lot {
    pub(crate) use ::parking_lot::Mutex;
}
