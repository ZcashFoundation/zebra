//! Transport-facing Zakura service types.
//!
//! This package owns the base types between bounded QUIC stream handling and
//! protocol services.

mod clock;
mod frame;
mod guard;
mod io;
mod pipe;
mod registry;
mod service;
mod session;

pub use clock::{Clock, RealClock};
pub use frame::{Frame, StreamPrelude, ZakuraTrace};
// `SessionGuard` and `ByteBudget` are imported through this re-export; `Admit`
// and `PeerMeters` are reached directly via the `guard` module path, so they are
// unused *here* until a later service imports them through `crate::zakura`.
#[allow(unused_imports)]
pub(crate) use guard::{Admit, ByteBudget, PeerMeters, SessionGuard};
pub use io::{framed_channel, FramedRecv, FramedSend};
pub(crate) use pipe::{
    handle_pipe_exit, spawn_supervised_peer_task, spawn_supervised_pipe, Edge, Flow, Node,
    NodeKind, Pipe, PipeCx, PipeShape,
};
pub use registry::{RegistryError, ServiceRegistry};
pub(crate) use service::ServiceStream;
pub use service::{
    BoxRunFuture, Peer, RequestResponseService, Service, Sink, SinkReject, Source, Stream,
    StreamMode,
};
pub use session::{OrderedSendError, PeerStreamSession};
