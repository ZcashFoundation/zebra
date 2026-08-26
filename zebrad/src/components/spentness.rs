//! The spentness-hint artifact fetch task.
//!
//! A spentness-hint artifact is captured by a node only at the moment its
//! own sync frontier passes the release-pinned checkpoint height. A node
//! whose tip is already past that height never reconstructs the artifact —
//! that would need per-output spending heights the state does not keep.
//! Instead, it downloads the artifact from v2 peers over `get-object`,
//! verifies it whole against the pinned content hash, and stores it, so it
//! can serve the artifact onward to other nodes.
//!
//! The task is dormant until release tooling pins a [`MaxCheckpoint`] for
//! the network: with no pin, there is nothing to fetch.

use std::time::Duration;

use tokio::task::JoinHandle;
use tower::{Service, ServiceExt};
use tracing::{debug, info, warn};
use tracing_futures::Instrument as _;

use zebra_chain::parameters::{
    spentness_hints::{self, MaxCheckpoint, SpentnessHints},
    Network,
};
use zebra_network::{self as zn, constants::MAX_OBJECT_TOTAL_SIZE, types::ObjectHash};
use zebra_state as zs;

use crate::BoxError;

/// How long to wait between artifact fetch rounds.
///
/// The artifact only becomes widely available once some nodes have captured
/// or downloaded it, so retries are unhurried.
const FETCH_RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// The number of bytes requested per `get-object` request (4 MiB).
///
/// The whole exchange is bounded by the per-request timeout, so pieces are
/// small enough that a slow peer still completes one within the timeout.
const OBJECT_PIECE_LENGTH: u64 = 4 * 1024 * 1024;

/// The maximum number of `get-object` requests issued per fetch round.
///
/// A peer serving dribble-sized pieces would otherwise keep the task
/// issuing requests at network speed; the cap falls the round out to the
/// retry interval.
const MAX_REQUESTS_PER_ROUND: usize = 64;

/// Spawns the spentness-hint artifact fetch task.
///
/// The task polls the `spentness_hint` column family; while the pinned
/// artifact is missing, it downloads the artifact range-by-range from v2
/// peers, verifies the whole artifact against the pinned hash, and persists
/// it. It exits once the artifact is stored, or immediately when the
/// network has no pinned artifact.
pub fn start<PS, SS, RS>(
    network: Network,
    peer_set: PS,
    state: SS,
    read_state: RS,
) -> JoinHandle<Result<(), BoxError>>
where
    PS: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone + Send + 'static,
    PS::Future: Send + 'static,
    SS: Service<zs::Request, Response = zs::Response, Error = BoxError> + Clone + Send + 'static,
    SS::Future: Send + 'static,
    RS: Service<zs::ReadRequest, Response = zs::ReadResponse, Error = BoxError>
        + Clone
        + Send
        + 'static,
    RS::Future: Send + 'static,
{
    tokio::spawn(
        async move {
            let Some(max_checkpoint) = spentness_hints::max_checkpoint(&network) else {
                debug!(%network, "no pinned spentness-hint artifact for this network");
                return Ok(());
            };
            let max_checkpoint = *max_checkpoint;

            info!(
                height = ?max_checkpoint.height,
                "spawning spentness-hint artifact fetch task",
            );

            let height = max_checkpoint.height;
            let hash = ObjectHash(max_checkpoint.spentness_hash);
            let mut bytes = Vec::new();

            loop {
                // Already stored? This node serves the artifact onward, so
                // the task is done.
                let stored = read_state
                    .clone()
                    .oneshot(zs::ReadRequest::SpentnessHint(height))
                    .await;
                if let Ok(zs::ReadResponse::SpentnessHint(Some(_))) = stored {
                    debug!(?height, "spentness-hint artifact already stored");
                    return Ok(());
                }

                match fetch_artifact(&mut peer_set.clone(), hash, &mut bytes).await {
                    Ok(()) => match verify_artifact(&network, &max_checkpoint, bytes.clone()) {
                        Ok(bytes) => {
                            // The artifact is opportunistic: a state write
                            // failure retries the next round rather than
                            // shutting the node down.
                            if let Err(error) = state
                                .clone()
                                .oneshot(zs::Request::WriteSpentnessHint { height, bytes })
                                .await
                            {
                                warn!(?error, "failed to store the spentness-hint artifact");
                            } else {
                                info!(
                                    ?height,
                                    artifact_hash = %hex::encode(max_checkpoint.spentness_hash),
                                    "fetched, verified, and stored the pinned spentness-hint artifact",
                                );
                                return Ok(());
                            }
                        }
                        Err(error) => {
                            // Wrong artifacts produce failure, not fraud: the
                            // next round retries against another peer, from
                            // scratch, since none of the bytes are trusted.
                            warn!(?error, "a peer served an invalid spentness-hint artifact");
                            bytes = Vec::new();
                        }
                    },
                    Err(error) => {
                        // Received bytes are kept: the next round resumes at
                        // the last received byte.
                        debug!(?error, "spentness-hint artifact fetch failed");
                    }
                }

                tokio::time::sleep(FETCH_RETRY_INTERVAL).await;
            }
        }
        .in_current_span(),
    )
}

/// Downloads the whole artifact named by `hash` from v2 peers, one
/// `get-object` range per request, appending to `bytes`: each request
/// resumes at the last received byte, so progress survives failed rounds.
/// Returns once `bytes` holds the whole artifact.
///
/// Peers are chosen by the peer set's load-weighted selection on every
/// request, so a peer that stops answering costs at most one range.
async fn fetch_artifact<PS>(
    peer_set: &mut PS,
    hash: ObjectHash,
    bytes: &mut Vec<u8>,
) -> Result<(), BoxError>
where
    PS: Service<zn::Request, Response = zn::Response, Error = BoxError> + Clone + Send + 'static,
    PS::Future: Send + 'static,
{
    let mut requests = 0;

    loop {
        if requests >= MAX_REQUESTS_PER_ROUND {
            return Err("too many small responses in one fetch round".into());
        }
        requests += 1;

        // The cast is a lossless widening: the received length fits u64.
        let offset = bytes.len() as u64;

        let response = peer_set
            .ready()
            .await?
            .call(zn::Request::Object {
                hash,
                offset,
                length: OBJECT_PIECE_LENGTH,
            })
            .await?;

        let zn::Response::Object {
            total_size,
            bytes: piece,
        } = response
        else {
            return Err("unexpected response to a get-object request".into());
        };

        if total_size == 0 {
            return Err("peer does not hold the object".into());
        }
        if total_size > MAX_OBJECT_TOTAL_SIZE {
            return Err(format!("object is implausibly large: {total_size} bytes").into());
        }

        // The cast is a lossless widening: the piece length fits u64.
        let done = piece.is_empty() || offset + piece.len() as u64 >= total_size;
        bytes.extend_from_slice(&piece);
        if done {
            // The cast is a lossless widening, as above.
            if bytes.len() as u64 == total_size {
                return Ok(());
            }

            return Err(format!(
                "object download stalled at {} of {total_size} bytes",
                bytes.len(),
            )
            .into());
        }
    }
}

/// Verifies a downloaded artifact against the pinned max checkpoint: the
/// canonical framing, the network, the pinned height, and — the trust gate
/// — the pinned whole-artifact SHA-256.
fn verify_artifact(
    network: &Network,
    max_checkpoint: &MaxCheckpoint,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, BoxError> {
    let hints = SpentnessHints::from_bytes(network, bytes).map_err(|error| -> BoxError {
        format!("malformed spentness-hint artifact: {error}").into()
    })?;

    if hints.max_height() != max_checkpoint.height {
        return Err(format!(
            "artifact covers height {:?}, expected {:?}",
            hints.max_height(),
            max_checkpoint.height,
        )
        .into());
    }

    if hints.artifact_hash() != max_checkpoint.spentness_hash {
        return Err(format!(
            "artifact hash {} does not match the pinned hash",
            hex::encode(hints.artifact_hash()),
        )
        .into());
    }

    Ok(hints.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        task::{Context, Poll},
    };

    use futures::future::BoxFuture;

    use super::*;

    /// A 4-output test network: the default testnet params (any network with
    /// a known magic works for artifact framing).
    fn test_network() -> Network {
        Network::new_default_testnet()
    }

    /// Builds a real artifact for `height` with the given hint bits, and the
    /// max checkpoint pinning it.
    fn test_artifact(
        height: zebra_chain::block::Height,
        bits: &[bool],
    ) -> (MaxCheckpoint, Vec<u8>) {
        let hints = SpentnessHints::from_bits(&test_network(), height, bits.iter().copied());
        let max_checkpoint = MaxCheckpoint {
            height,
            hash: zebra_chain::block::Hash([0x42; 32]),
            spentness_hash: hints.artifact_hash(),
        };
        (max_checkpoint, hints.as_bytes().to_vec())
    }

    /// A well-formed artifact that matches the pin verifies.
    #[test]
    fn verify_artifact_accepts_the_pinned_artifact() {
        let (max_checkpoint, bytes) =
            test_artifact(zebra_chain::block::Height(100), &[true, false]);

        let verified = verify_artifact(&test_network(), &max_checkpoint, bytes.clone())
            .expect("the pinned artifact verifies");

        assert_eq!(verified, bytes);
    }

    /// A tampered artifact, a mismatched height, and malformed bytes all
    /// fail verification.
    #[test]
    fn verify_artifact_rejects_wrong_artifacts() {
        let (max_checkpoint, bytes) =
            test_artifact(zebra_chain::block::Height(100), &[true, false]);

        // A different content hash.
        let (_other_checkpoint, other_bytes) =
            test_artifact(zebra_chain::block::Height(100), &[false, true]);
        assert!(verify_artifact(&test_network(), &max_checkpoint, other_bytes).is_err());

        // The right artifact, but a pin at a different height.
        let wrong_height = MaxCheckpoint {
            height: zebra_chain::block::Height(101),
            ..max_checkpoint
        };
        assert!(verify_artifact(&test_network(), &wrong_height, bytes.clone()).is_err());

        // Malformed bytes.
        assert!(verify_artifact(&test_network(), &max_checkpoint, vec![0xFF; 8]).is_err());

        // The wrong network.
        assert!(verify_artifact(&Network::Mainnet, &max_checkpoint, bytes).is_err());
    }

    /// A scripted peer set: each call pops a canned response (or error) and
    /// records the request's offset.
    #[derive(Clone)]
    struct ScriptedPeerSet {
        responses: Arc<Mutex<VecDeque<Result<zn::Response, BoxError>>>>,
        offsets: Arc<Mutex<Vec<u64>>>,
    }

    impl Service<zn::Request> for ScriptedPeerSet {
        type Response = zn::Response;
        type Error = BoxError;
        type Future = BoxFuture<'static, Result<zn::Response, BoxError>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: zn::Request) -> Self::Future {
            let zn::Request::Object { offset, .. } = request else {
                panic!("the fetch task only issues Object requests");
            };

            self.offsets
                .lock()
                .expect("offsets lock is not poisoned")
                .push(offset);

            let next = self
                .responses
                .lock()
                .expect("script lock is not poisoned")
                .pop_front()
                .expect("the script has a response for every request");

            Box::pin(async move { next })
        }
    }

    /// A fetch round delivers pieces in order, resumes within the round, and
    /// surfaces not-held and oversized answers as errors.
    #[tokio::test]
    async fn fetch_artifact_assembles_pieces() {
        let _init_guard = zebra_test::init();

        let object: Vec<u8> = (0..9_000_000u32).map(|i| (i % 251) as u8).collect();
        // The cast is a lossless widening: the object length fits u64.
        let total_size = object.len() as u64;
        let hash = ObjectHash([0x99; 32]);

        // Pieces of 4 MiB + 4 MiB + the rest: the fetch resumes each request
        // at the last received byte.
        let piece_1 = object[..4_194_304].to_vec();
        let piece_2 = object[4_194_304..8_388_608].to_vec();
        let piece_3 = object[8_388_608..].to_vec();

        let peer_set = ScriptedPeerSet {
            responses: Arc::new(Mutex::new(
                [
                    Ok(zn::Response::Object {
                        total_size,
                        bytes: piece_1,
                    }),
                    Ok(zn::Response::Object {
                        total_size,
                        bytes: piece_2,
                    }),
                    Ok(zn::Response::Object {
                        total_size,
                        bytes: piece_3,
                    }),
                ]
                .into_iter()
                .collect(),
            )),
            offsets: Arc::new(Mutex::new(Vec::new())),
        };

        let offsets = peer_set.offsets.clone();
        let mut bytes = Vec::new();
        fetch_artifact(&mut peer_set.clone(), hash, &mut bytes)
            .await
            .expect("the artifact assembles");
        assert_eq!(bytes, object);
        assert_eq!(
            *offsets.lock().expect("offsets lock is not poisoned"),
            vec![0, 4_194_304, 8_388_608],
            "each request resumes at the last received byte",
        );

        // A not-held object is an error and makes no progress.
        let peer_set = ScriptedPeerSet {
            responses: Arc::new(Mutex::new(
                [Ok(zn::Response::Object {
                    total_size: 0,
                    bytes: Vec::new(),
                })]
                .into_iter()
                .collect(),
            )),
            offsets: Arc::new(Mutex::new(Vec::new())),
        };
        let mut bytes = Vec::new();
        assert!(fetch_artifact(&mut peer_set.clone(), hash, &mut bytes)
            .await
            .is_err());
        assert!(bytes.is_empty());

        // An oversized object is rejected before its bytes are trusted.
        let peer_set = ScriptedPeerSet {
            responses: Arc::new(Mutex::new(
                [Ok(zn::Response::Object {
                    total_size: MAX_OBJECT_TOTAL_SIZE + 1,
                    bytes: vec![0xAB; 16],
                })]
                .into_iter()
                .collect(),
            )),
            offsets: Arc::new(Mutex::new(Vec::new())),
        };
        let mut bytes = Vec::new();
        assert!(fetch_artifact(&mut peer_set.clone(), hash, &mut bytes)
            .await
            .is_err());
    }
}
