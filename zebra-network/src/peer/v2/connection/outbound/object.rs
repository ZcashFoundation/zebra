//! Driving this node's `get-object` requests from the public protocol draft.
//!
//! One request is one stream: the request, then the answer's `result` and
//! `size`, then the data until the responder finishes the stream. The data is
//! read as it arrives, never reserved from the peer's `size`, and only up to
//! what the request and the object's own `size` allow.
//!
//! Once the `result` and `size` have arrived, an answer that the request
//! deadline cuts off, or that the responder resets (with `CANCELLED` or any
//! other code), is a short answer with the data read so far, not a failure:
//! object bytes are position-addressed, so the caller keeps them and asks for
//! the rest.
//!
//! # Misbehavior
//!
//! Every violation the connection can see in a `get-object` answer breaks a
//! "MUST" of the draft, so each is a connection error, with no misbehavior
//! score (draft, "Misbehavior and Banning"):
//! - an answer that does not conform to the format: no `result`, an unknown
//!   `result`, a truncated or non-canonical `size`, or data after a not-found
//!   answer: `PROTOCOL_ERROR`;
//! - more data than the request's `length`, or data past the end of the object
//!   as the answer's own `size` gives it: `FLOOD`;
//! - a `size` that differs from the one an earlier answer on the connection
//!   gave for the same hash: `PROTOCOL_ERROR`. The connection remembers the
//!   sizes of the last [`MAX_TRACKED_OBJECT_SIZES`] objects it asked about.
//!
//! The draft's one scored `get-object` violation, a complete object delivered
//! by one peer that does not hash to the requested hash (100 points), needs
//! the assembled object, which the caller holds: it may fetch an object in
//! several ranges, from one peer or several. So the caller verifies objects
//! and blames peers, and the connection does not hash what it delivers.
//!
//! A not-found answer, an answer the responder finishes early, and a request
//! the responder refuses are conformant, so none of them is penalized. A
//! refusal fails the request with [`PeerError::RequestRefused`].

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use quinn::Connection;
use tokio::sync::{mpsc, oneshot};

use crate::{
    peer::PeerError,
    protocol::{
        internal::Response,
        v2::{
            object::ObjectHash,
            request::{GetObject, Request as WireRequest},
            response::{read_object_data, read_object_header, ObjectAnswer},
            types::ErrorCode,
            wire::WireError,
        },
    },
};

use super::{open_request, Failure, MAX_OUTBOUND_REQUEST_STREAMS};

#[cfg(test)]
mod tests;

/// How many objects' sizes one connection remembers: once it
/// remembers this many, a new object's size replaces the size of the object
/// first asked about.
const MAX_TRACKED_OBJECT_SIZES: usize = 1_024;

/// How long a request waits for its connection's size task, which answers at
/// once unless it has exited, so this only bounds a shutdown race.
const SIZE_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// How long before the request deadline an answer's data stops being read,
/// so that the data read so far reaches the caller as a short answer before
/// the request's timeout ends it.
const SHORT_ANSWER_MARGIN: Duration = Duration::from_millis(100);

/// Requests up to `length` bytes of the object `hash` from `offset`, and
/// reads the answer, checking its size against `sizes`. The request's timeout
/// ends it at `deadline`.
///
/// The request is checked against the draft's bounds before anything is
/// sent: one outside them fails with [`PeerError::RequestOutOfBounds`],
/// without a stream, and the connection stays open.
pub(super) async fn fetch_object(
    connection: &Connection,
    sizes: &ObjectSizes,
    deadline: tokio::time::Instant,
    hash: ObjectHash,
    offset: u64,
    length: u32,
) -> Result<Response, Failure> {
    let request = GetObject {
        hash,
        offset,
        length,
    };
    request
        .check()
        .map_err(|error| Failure::Refused(PeerError::RequestOutOfBounds(error.to_string())))?;

    let mut recv = open_request(connection, &WireRequest::Object(request)).await?;
    // One reader for the whole answer: the buffer can hold data bytes read along with the header.
    let mut reader = recv.reader();

    let size = match read_object_header(&mut reader)
        .await
        .map_err(refused_or_failure)?
    {
        ObjectAnswer::NotFound => return Ok(Response::ObjectNotFound),
        ObjectAnswer::Found { size } => size,
    };

    // Draft, "get-object": a size inconsistent with an earlier answer for the same hash is a
    // `PROTOCOL_ERROR`. Objects are immutable, so a conforming peer never gives two sizes.
    if let Some(earlier) = sizes.record(hash, size).await {
        return Err(Failure::Violation {
            reason: format!(
                "get-object answer for {hash} gave size {size}, but an earlier answer gave \
                 {earlier}"
            ),
            code: ErrorCode::ProtocolError,
        });
    }

    let mut data = Vec::new();
    let data_deadline = deadline
        .checked_sub(SHORT_ANSWER_MARGIN)
        .unwrap_or(deadline);
    let read = read_object_data(&mut reader, request.data_limit(size), &mut data);
    match tokio::time::timeout_at(data_deadline, read).await {
        Ok(Ok(())) => {}
        // The data read so far is a prefix of the answer's data: a short answer.
        Ok(Err(error)) if reset_code(&error).is_some() => {
            tracing::debug!(
                %hash,
                offset,
                received = data.len(),
                %error,
                "get-object answer was reset part way, keeping the data received",
            );
        }
        Ok(Err(error)) => return Err(error.into()),
        Err(_elapsed) => {
            tracing::debug!(
                %hash,
                offset,
                received = data.len(),
                "get-object answer reached the request deadline, keeping the data received",
            );
        }
    }

    Ok(Response::Object { size, data })
}

/// The sizes the peer gave in its answers to this node's `get-object`
/// requests on one connection, by hash, so that an answer can be checked
/// against the earlier ones.
///
/// One task per connection owns them, and the connection's request tasks
/// reach it over a channel; it exits when the connection task and every
/// request task have dropped their handles.
#[derive(Clone, Debug)]
pub(in crate::peer::v2::connection) struct ObjectSizes {
    checks: mpsc::Sender<SizeCheck>,
}

/// A request to the size task: record `size` for `hash`, and reply with the
/// different size an earlier answer gave, if there was one.
#[derive(Debug)]
struct SizeCheck {
    hash: ObjectHash,
    size: u64,
    reply: oneshot::Sender<Option<u64>>,
}

impl ObjectSizes {
    /// Spawns a size task for a new connection.
    pub(in crate::peer::v2::connection) fn spawn() -> Self {
        // Each in-flight request waits for at most one answer.
        let (checks, receiver) = mpsc::channel(MAX_OUTBOUND_REQUEST_STREAMS);
        tokio::spawn(run(receiver));
        ObjectSizes { checks }
    }

    /// Records that the peer gave `size` for the object `hash`, and returns
    /// the size an earlier answer gave instead, if it differs.
    ///
    /// The first size is kept, so every later answer is checked against it.
    /// If the size task cannot be reached in time, the answer is not checked.
    pub(in crate::peer::v2::connection) async fn record(
        &self,
        hash: ObjectHash,
        size: u64,
    ) -> Option<u64> {
        let (reply, answer) = oneshot::channel();
        let check = async {
            self.checks
                .send(SizeCheck { hash, size, reply })
                .await
                .ok()?;
            answer.await.ok()
        };
        tokio::time::timeout(SIZE_CHECK_TIMEOUT, check)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                tracing::debug!(%hash, "object size task is unavailable");
                None
            })
    }
}

/// The size task: owns one connection's sizes and answers checks in order.
async fn run(mut checks: mpsc::Receiver<SizeCheck>) {
    let mut sizes = SizeMap::default();
    while let Some(SizeCheck { hash, size, reply }) = checks.recv().await {
        // A dropped reply means the request was abandoned; the size is still recorded.
        let _ = reply.send(sizes.record(hash, size));
    }
}

/// At most [`MAX_TRACKED_OBJECT_SIZES`] sizes, by hash, with the order the
/// hashes were first recorded in.
#[derive(Debug, Default)]
struct SizeMap {
    sizes: HashMap<ObjectHash, u64>,
    order: VecDeque<ObjectHash>,
}

impl SizeMap {
    /// Records `size` for `hash`, and returns the size recorded earlier if it
    /// differs. A new hash beyond the bound replaces the oldest one.
    fn record(&mut self, hash: ObjectHash, size: u64) -> Option<u64> {
        if let Some(&earlier) = self.sizes.get(&hash) {
            return (earlier != size).then_some(earlier);
        }

        if self.sizes.len() >= MAX_TRACKED_OBJECT_SIZES {
            if let Some(oldest) = self.order.pop_front() {
                self.sizes.remove(&oldest);
            }
        }
        self.sizes.insert(hash, size);
        self.order.push_back(hash);
        None
    }
}

/// The failure for an answer that ended before its `result` and `size`
/// arrived.
///
/// Only the responder's reset of the stream with `REFUSED` is a refusal, which
/// reaches the caller as its own error, [`PeerError::RequestRefused`], so a
/// caller that backs off from busy peers never has to search
/// error text, which can carry bytes the peer chose, such as the reason it
/// closed the connection with.
fn refused_or_failure(error: WireError) -> Failure {
    if reset_code(&error) == Some(ErrorCode::Refused.code()) {
        Failure::PeerRefused
    } else {
        error.into()
    }
}

/// The application error code the responder reset the stream with, if
/// `error` is such a reset.
fn reset_code(error: &WireError) -> Option<u64> {
    let WireError::Io(error) = error else {
        return None;
    };
    match error.get_ref()?.downcast_ref::<quinn::ReadError>()? {
        quinn::ReadError::Reset(code) => Some(code.into_inner()),
        _ => None,
    }
}
