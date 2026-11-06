//! Serving the peer's `get-object` requests from this node's synchronization
//! artifacts (draft, "`get-object`").

use std::{fs::File, io, sync::Arc, time::Duration};

use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

use crate::{
    constants::REQUEST_TIMEOUT,
    protocol::v2::{
        object::{ObjectEntry, ObjectHash, ObjectSource},
        request::GetObject,
        response::{encode_object_header, ObjectAnswer},
        types::ErrorCode,
    },
};

use super::{Outcome, ResponseSend};

/// How many of one peer's `get-object` requests are served at once; the rest
/// are refused rather than queued.
///
/// A `get-object` stream holds none of the peer's
/// [`INBOUND_REQUEST_CONCURRENCY`](super::INBOUND_REQUEST_CONCURRENCY) request permits: those are waited for,
/// and an object request that waited behind the peer's other requests could
/// not be refused promptly when this limit is reached. Instead, the peer's
/// object streams are bounded by this limit, and its probes and misses, which
/// take no permit, by the transport's limit on concurrent streams; each of
/// those reads at most a request and writes at most a `result` and a `size`.
pub(in crate::peer::v2::connection) const MAX_OBJECT_STREAMS_PER_PEER: usize = 2;

/// How many `get-object` requests this node serves at once, over all its
/// peers; the rest are refused rather than queued.
///
/// # Security
///
/// Each served request holds one [`OBJECT_READ_CHUNK`] of object data at a
/// time, so this bounds the memory and the disk reads of object serving,
/// whatever the number of peers.
pub(in crate::peer::v2::connection) const MAX_OBJECT_STREAMS_GLOBAL: usize = 8;

/// The most bytes of an object read from its file at once, and written to
/// the response stream before the next read.
pub(in crate::peer::v2::connection) const OBJECT_READ_CHUNK: usize = 1 << 20;

/// How long before its request deadline a `get-object` response is finished
///.
///
/// A request stream that misses its deadline is reset with `REFUSED` by
/// [`serve`](super::serve), but a `get-object` response must never be reset once its
/// `result` and `size` are written: the requester keeps the prefix it
/// received as a short answer. So the response stops writing this long
/// before the deadline, and the stream is finished instead.
pub(in crate::peer::v2::connection) const OBJECT_EARLY_FIN_MARGIN: Duration =
    Duration::from_secs(4);

/// How long one [`OBJECT_READ_CHUNK`] write to a `get-object` response may
/// wait for the peer to read, before the response is finished early with the
/// bytes written so far.
///
/// # Security
///
/// A peer that reads slowly holds its object stream permits, and one of the
/// node's [`MAX_OBJECT_STREAMS_GLOBAL`], for as long as the response runs. So
/// a write that stalls on the peer's flow control ends the response, rather
/// than waiting for the deadline: a peer must read about 200 KiB/s, a little
/// below the rate that Zebra's artifact downloads require of the peers they
/// fetch from, to keep a stream.
pub(in crate::peer::v2::connection) const OBJECT_WRITE_STALL: Duration = Duration::from_secs(5);

// A response must have time to start before the margin; checked at compile time.
const _: () = assert!(OBJECT_EARLY_FIN_MARGIN.as_millis() * 2 < REQUEST_TIMEOUT.as_millis());

/// What serving the peer's `get-object` requests needs from the connection
///.
#[derive(Clone)]
pub(in crate::peer::v2::connection) struct ObjectServing {
    /// The objects this node serves, by hash, or `None` if it holds none.
    pub source: Option<ObjectSource>,

    /// Whether this node advertises `NODE_SYNC_ARTIFACTS`: unless it does,
    /// every `get-object` request is refused, whatever the index holds.
    pub advertised: bool,

    /// The peer's permits, [`MAX_OBJECT_STREAMS_PER_PEER`] of them.
    pub peer_permits: Arc<Semaphore>,

    /// The node's permits, [`MAX_OBJECT_STREAMS_GLOBAL`] of them, shared by
    /// every connection.
    pub global_permits: Arc<Semaphore>,
}

/// Serves a `get-object` request from this node's synchronization artifacts,
/// as specified by the public protocol draft.
///
/// The request is refused with `REFUSED` unless this node advertises
/// `NODE_SYNC_ARTIFACTS` and holds an object index, and the connection stays
/// open. An object the index does not hold is answered as not found, and a
/// probe (`length == 0`, or an `offset` at or past the end of the object)
/// with the object's `size` alone, both from the index, without touching the
/// disk. The draft's order holds: the offset is checked against the size
/// before the length is looked at, and the bytes returned are at most
/// `min(length, size − offset)`, never `offset + length`.
///
/// A request for data takes one of the peer's and one of the node's object
/// stream permits, without waiting: when either is exhausted the request is
/// refused and counted as busy, never as an inbound service overload, so a
/// peer's downloads never close its connection. The object's file is opened
/// before the response starts, so an unreadable file is refused too. The
/// data is then read in positional [`OBJECT_READ_CHUNK`] reads on the
/// blocking pool and written as it arrives, up to the request's `length`.
///
/// Once `result` and `size` are written, the stream is never reset: a read
/// failure, a file shorter than the index says, or a peer that reads slowly,
/// so that one chunk's write waits longer than [`OBJECT_WRITE_STALL`],
/// finishes the response early with the bytes written so far, which the
/// draft allows and the requester re-requests. Every wait here, the header
/// writes of probes and misses included, stops [`OBJECT_EARLY_FIN_MARGIN`]
/// before `deadline`, the stream's request deadline, so that the deadline's
/// `REFUSED` reset in [`serve`](super::serve) never applies to a started response; a
/// request that arrives with less than the margin left is refused.
pub(in crate::peer::v2::connection) async fn serve_object(
    response: &mut ResponseSend,
    request: GetObject,
    objects: &ObjectServing,
    deadline: Instant,
) -> Result<(), Outcome> {
    let Some(source) = objects.source.as_ref().filter(|_| objects.advertised) else {
        return Err(Outcome::Refuse(ErrorCode::Refused));
    };

    // Every wait below stops this long before the deadline. A request that arrives with less time
    // than that left is refused before anything is written.
    let now = Instant::now();
    let Some(time_left) = deadline
        .checked_duration_since(now)
        .and_then(|left| left.checked_sub(OBJECT_EARLY_FIN_MARGIN))
    else {
        return Err(Outcome::Refuse(ErrorCode::Refused));
    };
    let early_deadline = now + time_left;

    // The index is replaced whole by its publisher, so the entry is copied out and the borrow
    // dropped at once, before any wait.
    let entry = source.borrow().get(&request.hash).cloned();
    let Some(ObjectEntry { path, size }) = entry else {
        let header = encode_object_header(ObjectAnswer::NotFound)?;
        return write_header(response, &header, early_deadline).await;
    };
    let header = encode_object_header(ObjectAnswer::Found { size })?;

    // Zero when the offset is at or past the end: the size alone answers that, and a zero length.
    let remaining = size.saturating_sub(request.offset);
    let budget = remaining.min(u64::from(request.length));
    if budget == 0 {
        return write_header(response, &header, early_deadline).await;
    }

    // # Security
    //
    // Only a few object downloads are served at once, per peer and over all peers; the rest are
    // refused at once, not queued, and the requester tries another peer or waits. A refusal
    // for a busy node is not an overload: the overload policy that drops connections is for
    // requests the inbound service could not take.
    let Ok(peer_permit) = objects.peer_permits.clone().try_acquire_owned() else {
        return Err(busy());
    };
    let Ok(global_permit) = objects.global_permits.clone().try_acquire_owned() else {
        return Err(busy());
    };
    let permits = Arc::new((peer_permit, global_permit));

    // Opened before the response starts, so an object the index names but the disk lacks is
    // refused rather than finished early with no data.
    let opened = tokio::time::timeout_at(
        early_deadline,
        spawn_object_disk(&permits, move || File::open(path)),
    )
    .await;
    let mut file = match opened {
        Ok(Ok(Ok(file))) => file,
        Ok(Ok(Err(error))) => {
            // A peer can repeat a request for an object the index names but the disk lacks, so
            // the counter carries the signal, and the log stays quiet.
            unreadable(&request.hash, &error, "could not open a served object");
            return Err(Outcome::Refuse(ErrorCode::Refused));
        }
        // The blocking task panicked, or the margin before the deadline was reached.
        Ok(Err(_)) | Err(_) => return Err(Outcome::Refuse(ErrorCode::Refused)),
    };

    // From here on, the response has started: it is finished, never reset.
    write_header(response, &header, early_deadline).await?;

    let mut sent: u64 = 0;
    while sent < budget {
        // At most `OBJECT_READ_CHUNK`, so the chunk fits in a `usize`.
        let want = usize::try_from((budget - sent).min(OBJECT_READ_CHUNK as u64))
            .expect("the chunk is at most OBJECT_READ_CHUNK, which is a usize");
        // `offset` is below `size`, and `sent` is below `size - offset`, so this never wraps.
        let Some(position) = request.offset.checked_add(sent) else {
            break;
        };

        let read = tokio::time::timeout_at(
            early_deadline,
            spawn_object_disk(&permits, move || {
                let mut chunk = vec![0; want];
                let read = read_exact_at(&file, &mut chunk, position);
                (file, chunk, read)
            }),
        )
        .await;
        let chunk = match read {
            Ok(Ok((returned, mut chunk, Ok(count)))) => {
                file = returned;
                chunk.truncate(count);
                chunk
            }
            Ok(Ok((_, _, Err(error)))) => {
                unreadable(&request.hash, &error, "could not read a served object");
                break;
            }
            // The blocking task panicked, or the margin before the deadline was reached.
            Ok(Err(_)) | Err(_) => break,
        };
        // The file is shorter than the index says: the requester gets what there is.
        if chunk.is_empty() {
            break;
        }

        let write_deadline = early_deadline.min(Instant::now() + OBJECT_WRITE_STALL);
        match tokio::time::timeout_at(write_deadline, response.write(&chunk)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(error),
            // The peer reads slowly: finish with what it has, before the deadline resets, and
            // free the permits for other requests.
            Err(_elapsed) => break,
        }
        // `chunk.len()` is at most `want`, which fits in a `u64` as it did in a `usize`.
        sent = sent.saturating_add(chunk.len() as u64);
    }

    record_sent_object_bytes(sent);
    Ok(())
}

/// Writes a `get-object` response `header` (`result`, and the `size` if the object was found),
/// stopping at `early_deadline`. A timed-out write may already have sent a prefix, so it uses the
/// same started-response cancellation semantics as the outer request deadline.
async fn write_header(
    response: &mut ResponseSend,
    header: &[u8],
    early_deadline: Instant,
) -> Result<(), Outcome> {
    tokio::time::timeout_at(early_deadline, response.write(header))
        .await
        .unwrap_or_else(|_| Err(Outcome::Refuse(response.abandon_code())))
}

/// The per-peer and global permits bounding object disk operations.
pub(in crate::peer::v2::connection) type ObjectPermits =
    Arc<(OwnedSemaphorePermit, OwnedSemaphorePermit)>;

/// Runs blocking object disk `work` while retaining a permit reference until the work actually
/// finishes, even if its async wait times out and drops the join handle.
pub(in crate::peer::v2::connection) fn spawn_object_disk<T, F>(
    permits: &ObjectPermits,
    work: F,
) -> tokio::task::JoinHandle<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permits = permits.clone();
    tokio::task::spawn_blocking(move || {
        let _permits = permits;
        work()
    })
}

/// Counts object payload bytes sent by this node.
pub(in crate::peer::v2::connection) fn record_sent_object_bytes(sent: u64) {
    metrics::counter!("zebra.net.out.objects.bytes").increment(sent);
}

/// The outcome of a `get-object` request this node is too busy to serve:
/// refused, and counted as busy.
fn busy() -> Outcome {
    metrics::counter!("zebra.net.in.objects.busy").increment(1);
    Outcome::Refuse(ErrorCode::Refused)
}

/// Counts an object the index names but the disk could not open or read, and
/// logs it at debug: a peer can repeat the request as fast as its permits
/// allow, so the counter carries the signal, and the operator's warning
/// belongs with the index's publisher.
pub(in crate::peer::v2::connection) fn unreadable(
    hash: &ObjectHash,
    error: &io::Error,
    what: &'static str,
) {
    metrics::counter!("zebra.net.in.objects.unreadable").increment(1);
    tracing::debug!(%error, %hash, "{what}");
}

/// Fills `buf` from `file` at `position`, without moving the file's cursor,
/// and returns how many bytes were read: fewer than `buf.len()` only at the
/// end of the file.
fn read_exact_at(file: &File, buf: &mut [u8], position: u64) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        // `filled` is below `buf.len()`, a `usize`, which fits in a `u64`.
        let Some(position) = position.checked_add(filled as u64) else {
            break;
        };
        match read_at(file, &mut buf[filled..], position) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// One positional read from `file` at `position` into `buf`.
#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], position: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, position)
}

/// One positional read from `file` at `position` into `buf`.
///
/// Seeks the file's cursor, which nothing else uses: the file is opened for
/// this request alone.
#[cfg(not(unix))]
fn read_at(mut file: &File, buf: &mut [u8], position: u64) -> io::Result<usize> {
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(position))?;
    file.read(buf)
}
