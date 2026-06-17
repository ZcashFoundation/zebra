//! Bounded inbound sink used by Zakura tests.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use tracing::debug;

use crate::zakura::{
    legacy_gossip_streams, Frame, Peer, Service, SinkReject, Stream, ZakuraPeerId,
};

/// One frame delivered to an [`InboundRecorder`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedInbound {
    /// Authenticated peer that sent the frame.
    pub peer_id: ZakuraPeerId,
    /// Application stream kind.
    pub stream_kind: u16,
    /// Decoded stream frame.
    pub frame: Frame,
}

/// Bounded, inspectable inbound sink for integration tests.
///
/// This is a lossy observation tap. Production backpressure is enforced by the
/// bounded channel feeding the sink; once a frame reaches the recorder, old
/// observations are dropped rather than blocking the handler on test inspection.
#[derive(Clone, Debug)]
pub struct InboundRecorder {
    capacity: usize,
    messages: Arc<Mutex<VecDeque<RecordedInbound>>>,
    dropped: Arc<AtomicUsize>,
}

impl InboundRecorder {
    /// Create a recorder with a fixed capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            messages: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.max(1)))),
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Drain all currently recorded messages.
    pub fn drain(&self) -> Vec<RecordedInbound> {
        self.messages
            .lock()
            .expect("recorder mutex should not be poisoned")
            .drain(..)
            .collect()
    }

    /// Returns true if a recorded frame has the given payload.
    pub fn contains_payload(&self, stream_kind: u16, payload: &[u8]) -> bool {
        self.messages
            .lock()
            .expect("recorder mutex should not be poisoned")
            .iter()
            .any(|message| {
                message.stream_kind == stream_kind && message.frame.payload.as_slice() == payload
            })
    }

    /// Current number of retained messages.
    pub fn len(&self) -> usize {
        self.messages
            .lock()
            .expect("recorder mutex should not be poisoned")
            .len()
    }

    /// Returns true if no messages are retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of observations dropped because the recorder was full.
    pub fn dropped_count(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Record one decoded frame.
    pub fn deliver(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        let mut messages = self
            .messages
            .lock()
            .map_err(|_| SinkReject::local("recorder mutex should not be poisoned"))?;
        if messages.len() == self.capacity {
            messages.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        messages.push_back(RecordedInbound {
            peer_id,
            stream_kind,
            frame,
        });
        Ok(())
    }
}

impl Service for InboundRecorder {
    fn name(&self) -> &'static str {
        "inbound-recorder"
    }

    fn streams(&self) -> &[Stream] {
        legacy_gossip_streams()
    }

    fn add_peer(&self, mut peer: Peer) {
        for stream in self
            .streams()
            .iter()
            .filter(|stream| matches!(stream.mode, crate::zakura::StreamMode::Ordered))
        {
            let Some((mut recv, _send)) = peer.take_stream(stream.kind) else {
                continue;
            };
            // The recorder observes inbound frames only; it has no source side.
            let recorder = self.clone();
            let peer_id = peer.id.clone();
            let stream_kind = stream.kind;
            let cancel_token = peer.cancel_token();
            tokio::spawn(async move {
                loop {
                    let frame = tokio::select! {
                        _ = cancel_token.cancelled() => return,
                        frame = recv.recv() => {
                            let Some(frame) = frame else {
                                return;
                            };
                            frame
                        }
                    };

                    if let Err(error) = recorder.deliver(peer_id.clone(), stream_kind, frame) {
                        debug!(?error, ?peer_id, "inbound recorder could not record frame");
                    }
                }
            });
        }
    }

    fn remove_peer(&self, _peer: &ZakuraPeerId) {}

    fn deliver_frame(
        &self,
        peer_id: ZakuraPeerId,
        stream_kind: u16,
        frame: Frame,
    ) -> Result<(), SinkReject> {
        self.deliver(peer_id, stream_kind, frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_is_bounded_and_reports_drops() {
        let recorder = InboundRecorder::new(2);

        for payload in [1, 2, 3] {
            recorder
                .deliver(
                    ZakuraPeerId::new(vec![7; 32]).expect("test peer id is within bounds"),
                    1,
                    Frame {
                        message_type: 0,
                        flags: 0,
                        payload: vec![payload],
                    },
                )
                .expect("recorder accepts frames");
        }

        let messages = recorder.drain();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].frame.payload, vec![2]);
        assert_eq!(messages[1].frame.payload, vec![3]);
        assert_eq!(recorder.dropped_count(), 1);
    }
}
