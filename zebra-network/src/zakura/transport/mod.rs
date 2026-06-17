//! Transport-facing Zakura service types.
//!
//! This package owns the base types between bounded QUIC stream handling and
//! protocol services.

mod clock;
mod frame;
mod io;
mod registry;
mod service;

pub use clock::{Clock, RealClock};
pub use frame::{Frame, StreamPrelude, ZakuraTrace};
pub use io::{framed_channel, FramedRecv, FramedSend};
pub use registry::{RegistryError, ServiceRegistry};
pub use service::{BoxRunFuture, Peer, Service, Sink, SinkReject, Source, Stream, StreamMode};
