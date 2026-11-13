//! Reading records from a stream the peer writes, with a bound on how long
//! an incomplete record is held.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures::Stream;
use tokio::{io::AsyncRead, time::Sleep};
use tokio_util::codec::FramedRead;

use crate::{
    constants::REQUEST_TIMEOUT,
    protocol::v2::{record::RecordCodec, wire::WireError},
};

/// How long a record may take to arrive once its first byte has: as long as
/// a whole request and its response may take.
///
/// # Security
///
/// A stream holds the part of a record that has arrived until the rest
/// does, so a peer could otherwise keep that much buffered on every stream
/// it may open for as long as it likes, by never completing a record. The
/// connection is closed with `FLOOD` instead (draft, "Announcement
/// Streams": a receiver SHOULD bound the time it holds an incomplete record,
/// and MAY treat exceeding it as `FLOOD`). The time runs while the stream is
/// read: [`RecordReader::pause`] stops it while the reader does other work.
pub(super) const INCOMPLETE_RECORD_TIMEOUT: Duration = REQUEST_TIMEOUT;

/// The records of a stream, which fails with [`WireError::Stalled`] if a
/// record is still incomplete [`INCOMPLETE_RECORD_TIMEOUT`] after its first
/// byte was read.
pub(super) struct RecordReader<R> {
    records: FramedRead<R, RecordCodec>,
    /// Ends when the incomplete record being read has been held for too
    /// long; `None` while no record is part way through.
    stalled: Option<Pin<Box<Sleep>>>,
}

impl<R: AsyncRead + Unpin> RecordReader<R> {
    /// Reads the records of `records`.
    pub(super) fn new(records: FramedRead<R, RecordCodec>) -> Self {
        RecordReader {
            records,
            stalled: None,
        }
    }

    /// Stops timing the incomplete record, if there is one, because the
    /// caller is about to stop reading for a while: the rest of the record
    /// may have arrived by the time it reads again, and the time the caller
    /// spent elsewhere is not the peer's delay. The timing restarts with the
    /// next read.
    pub(super) fn pause(&mut self) {
        self.stalled = None;
    }
}

impl<R: AsyncRead + Unpin> Stream for RecordReader<R> {
    type Item = Result<Bytes, WireError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Poll::Ready(record) = Pin::new(&mut this.records).poll_next(cx) {
            this.stalled = None;
            return Poll::Ready(record);
        }

        // The decoder has taken every complete record, so whatever is left is part of one.
        let incomplete =
            !this.records.read_buffer().is_empty() || this.records.decoder().is_skipping();
        if !incomplete {
            this.stalled = None;
            return Poll::Pending;
        }
        let stalled = this
            .stalled
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(INCOMPLETE_RECORD_TIMEOUT)));
        match stalled.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Some(Err(WireError::Stalled { what: "record" }))),
            Poll::Pending => Poll::Pending,
        }
    }
}
