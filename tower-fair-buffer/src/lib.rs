//! Tower middleware that buffers requests in per-caller priority order.
//!
//! [`FairBuffer`] wraps an inner `S: Service<R>` and provides a cloneable
//! `Service<Tagged<K, R>>`, where [`Tagged`] pairs each request with an
//! optional caller key `K` (for example, a peer address). A background
//! [`Worker`] task dispatches queued requests to the inner service, one at a
//! time, in priority order:
//!
//! - Requests from the caller with the lowest recent request count are
//!   processed first, with FIFO order between requests of equal priority.
//! - When the buffer is full, the queued request with the *highest* recent
//!   request count is shed: its response future fails with [`error::Shed`].
//! - Requests without a caller key (internal requests) always have priority 0,
//!   so they are processed before caller requests and are never shed.
//!
//! Recent request counts decay using two map generations that rotate on a
//! fixed interval, like Zebra's inventory registry: a caller's count is the
//! sum of its entries in the current and previous generations, so counts
//! expire after at most two rotation intervals. Queued requests keep the
//! priority they were assigned when they were enqueued, even if a rotation
//! happens while they are queued.
//!
//! ## Differences from `tower::buffer`
//!
//! This crate is a fork of [tower 0.4.13's `tower::buffer`], with two
//! architectural changes:
//!
//! - The FIFO mpsc channel is replaced with a [`crossbeam_skiplist::SkipMap`]
//!   ordered by `(priority, FIFO sequence number)`. The skip map provides the
//!   priority ordering; a single small mutex serializes queue mutations,
//!   request counting, and teardown, which keeps capacity enforcement and
//!   shedding exact, and avoids lock-free teardown races. Contention on the
//!   mutex is low when each caller has at most one in-flight request, like
//!   Zebra's peer connections.
//! - Backpressure is replaced with load shedding: `poll_ready` is always ready
//!   (unless the worker has failed), and pushing a request beyond the buffer's
//!   capacity sheds the highest-priority-key queued request instead of making
//!   callers wait. This subsumes `tower::load_shed`, and removes
//!   `tower::buffer`'s reserved-slot hang footgun, where a caller that calls
//!   `poll_ready` but never `call` holds a buffer slot forever.
//!
//! Like `tower::buffer`, the worker hands the inner service's response future
//! back to the caller through a oneshot channel, so responses are driven by
//! caller tasks and the worker only dispatches requests.
//!
//! ## Denial of service resistance
//!
//! - **Bounded queue memory:** the queue holds at most `capacity` requests
//!   (plus the one the worker is dispatching), so queue memory is bounded by
//!   `capacity` times the maximum request size. Internal requests are never
//!   shed and can exceed `capacity`, so internal callers must be finite and
//!   disciplined; attackers can't make internal requests.
//! - **Bounded per-caller state:** the only per-caller state is two
//!   `HashMap<K, u64>` count generations (tens of bytes per distinct key).
//!   Keys expire after at most two rotation intervals, and the key insertion
//!   rate is bounded by the callers' aggregate request rate — in Zebra,
//!   additionally by connection limits and rate limits.
//! - **Shedding is cheap:** a shed removes one skip map entry and resolves an
//!   already-allocated oneshot channel with a boxed unit-struct error. No work
//!   or allocation is proportional to attacker input.
//! - **Counting is attacker-resistant:** counts are recorded at enqueue time,
//!   whether or not the request is eventually shed, so shed requests still
//!   raise the sender's priority key and push it further back in the queue.
//!
//! [tower 0.4.13's `tower::buffer`]:
//!     https://github.com/tower-rs/tower/tree/master/tower/src/buffer

pub mod error;
pub mod future;

mod counts;
mod message;
mod queue;
mod service;
mod tagged;
mod worker;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub use self::{service::FairBuffer, tagged::Tagged, worker::Worker};
