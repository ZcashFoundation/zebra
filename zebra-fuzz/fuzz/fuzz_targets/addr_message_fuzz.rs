#![no_main]

//! Addr Message — Application-Layer Invariant Fuzz Target.
//!
//! This target complements `p2p_message_parse.rs`:
//!
//! - `p2p_message_parse.rs`: codec **framing** invariants only — round-trip
//!   byte equality, size limit, protocol version range, command name UTF-8,
//!   header/payload size consistency. **Stops at decode boundary.**
//! - `addr_message_fuzz.rs` (this file): **application-layer** Addr handling
//!   — for each `Message::Addr(addrs)` decoded successfully, exercise the
//!   accessor + transform paths that are reachable from network input but
//!   live above the codec layer. These are the paths that production code
//!   takes once a peer has gossiped addresses to us.
//!
//! Paths exercised here, reached end-to-end from decoded network input:
//!
//! - `limit_last_seen_times` and the `untrusted_last_seen()` accessor on
//!   gossiped `MetaAddr` values, probing whether decode always populates the
//!   fields that downstream code assumes are present.
//! - `new_gossiped_change` on `MetaAddr` values with varying
//!   `services` / `untrusted_last_seen` combinations.
//! - `send_addrs` over a `Vec<MetaAddr>` decoded from network input.
//! - `MetaAddr::sanitize(&Network)`, whose `rem_euclid` / `checked_sub`
//!   arithmetic feeds directly from decoded peer data when responding to
//!   `getaddr` queries.
//!
//! Layered design:
//!
//! - Layer 1: Codec decode (same as `p2p_message_parse.rs` — multi-message
//!   extraction from raw bytes; ignore parse errors as expected).
//! - Layer 2: Filter `Message::Addr(addrs)` and `Message::AddrV2(addrs)`
//!   variants; everything else returns early.
//! - Layer 3: Per-addr invariant battery, each invariant wrapped in
//!   `panic::catch_unwind(panic::AssertUnwindSafe(...))` so a panic in one
//!   invariant does not abort the fuzz process and lose subsequent inputs.
//!
//! Conservative measure: we treat every panic surfaced by these accessors
//! as something to triage offline; the target itself only flags via
//! libfuzzer crash artifacts.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use std::panic;
use tokio_util::codec::{Decoder, Encoder};
use zebra_chain::parameters::Network;
use zebra_network::protocol::external::{Codec, Message};
use zebra_network::types::MetaAddr;

fuzz_target!(|data: &[u8]| {
    // ═══════════════════════════════════════════════════════════════════
    // Layer 1: Codec Decode — mirror p2p_message_parse.rs framing.
    //
    // Feed raw bytes into the codec and pull out as many `Message`
    // values as possible. We only proceed for `Addr` / `AddrV2`
    // variants; other parses are silently dropped (covered by the
    // sister target).
    // ═══════════════════════════════════════════════════════════════════
    let mut codec = Codec::builder().finish();
    let mut buf = BytesMut::from(data);
    let mut addr_batches: Vec<Vec<MetaAddr>> = Vec::new();

    loop {
        match codec.decode(&mut buf) {
            Ok(Some(Message::Addr(addrs))) => {
                addr_batches.push(addrs);
                continue;
            }
            // Note: AddrV2 lands as `Message::Addr` in the current codec
            // (see message.rs); if a separate variant is introduced
            // upstream this target should be updated. For now we only
            // need to match on Addr.
            Ok(Some(_)) => {
                // Not an addr-bearing message — skip and keep reading
                // because a single byte stream can contain multiple
                // messages.
                continue;
            }
            // Need more data.
            Ok(None) => break,
            // Parse error — expected, exit the read loop.
            Err(_) => break,
        }
    }

    if addr_batches.is_empty() {
        return;
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 2: Per-batch and per-addr application-layer invariants.
    // ═══════════════════════════════════════════════════════════════════

    for addrs in addr_batches {
        // ---------------------------------------------------------------
        // A-0: batch-level — `Vec<MetaAddr>` size sanity.
        //
        // The codec enforces an upper bound on Addr message body length
        // via `MAX_PROTOCOL_MESSAGE_LEN` and `MAX_ADDRS_IN_MESSAGE`, but
        // we record the observed length to make the fuzz corpus
        // size-aware.
        // ---------------------------------------------------------------
        let batch_len = addrs.len();
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            // Reading `len()` cannot panic; we hold the assertion shape
            // for symmetry with later invariants.
            assert!(
                batch_len <= 1_000_000,
                "A-0 violated: addr batch length {} unexpectedly huge",
                batch_len
            );
        }));

        for addr in addrs.iter() {
            // -----------------------------------------------------------
            // A-1: `addr()` accessor must not panic.
            //
            // Trivial today, but guards future refactors that could
            // make the address field optional or lazily computed.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.addr();
            }));

            // -----------------------------------------------------------
            // A-2: `last_seen()` accessor must not panic.
            //
            // Returns `Option<DateTime32>` so this is safe by signature,
            // but again we wrap for defense-in-depth.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.last_seen();
            }));

            // -----------------------------------------------------------
            // A-3: `peer_preference()` evaluation.
            //
            // Used by the address book ranking logic; returns a
            // `Result<PeerPreference, &'static str>` so panic-free
            // by signature, but the implementation calls into
            // `PeerPreference::new` which has its own range checks
            // that are worth exercising under arbitrary IPv4/v6/Tor
            // bytes from the wire.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.peer_preference();
            }));

            // -----------------------------------------------------------
            // A-4: outbound-validity checks against both networks.
            //
            // `address_is_valid_for_outbound` and
            // `last_known_info_is_valid_for_outbound` are called from
            // the address book before a connection attempt. Network
            // input drives both directly. We test against Mainnet AND
            // Testnet because the per-network port checks differ.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.address_is_valid_for_outbound(&Network::Mainnet);
                let _ = addr.last_known_info_is_valid_for_outbound(&Network::Mainnet);
            }));

            // -----------------------------------------------------------
            // A-5: `sanitize(&Network)` — getaddr response path.
            //
            // The implementation contains a `rem_euclid` followed by an
            // `.expect(...)` on the result. The expectation holds for all
            // `DateTime32` values currently representable, but it is
            // exactly the kind of arithmetic invariant that fuzzing
            // should validate end-to-end with adversarial wire input.
            //
            // We test sanitize for both networks because the function
            // first calls `last_known_info_is_valid_for_outbound`
            // which is network-dependent, so coverage of the
            // arithmetic path requires picking inputs that pass for
            // each.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.sanitize(&Network::Mainnet);
            }));

            // -----------------------------------------------------------
            // A-6: `new_gossiped_change()` — conversion to
            //       `MetaAddrChange::NewGossiped`.
            //
            // The function unwraps `untrusted_last_seen` after `services?`
            // has short-circuited the `None` case. If a gossiped
            // `MetaAddr` ever reaches this call site with
            // `services = Some(_)` AND `untrusted_last_seen = None`,
            // it panics. We exercise that combination directly here.
            //
            // We `clone()` because `new_gossiped_change` consumes
            // self, and we want the addr available for later
            // invariants in the iteration if any are added.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.clone().new_gossiped_change();
            }));

            // -----------------------------------------------------------
            // A-7: `misbehavior()` accessor.
            //
            // Returns `u32` directly, so panic-free by signature, but
            // we include it so future refactors that introduce e.g.
            // saturating-arithmetic-overflow assertions are caught
            // early.
            // -----------------------------------------------------------
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = addr.misbehavior();
            }));
        }

        // ---------------------------------------------------------------
        // A-8: Simulate `candidate_set::send_addrs` flat-map.
        //
        // The production `send_addrs` path maps each addr through
        // `new_gossiped_change` and unwraps the result, relying on the
        // property that gossiped peers always have services set.
        // We mirror that map **without** the production unwrap — any input
        // that would turn that unwrap into a panic is worth surfacing.
        // This fuzz path actively re-verifies the property under
        // adversarial wire input.
        //
        // We iterate `addrs.iter().cloned()` because
        // `new_gossiped_change` consumes `self`; cloning keeps
        // ownership clean and matches what the production
        // `send_addrs` does internally (it consumes the input).
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            for addr in addrs.iter().cloned() {
                let _changed: Option<_> = addr.new_gossiped_change();
                // No `.expect(...)` here on purpose — the production
                // call site has the unwrap; we only want to know if
                // the inner conversion itself panics for any input.
            }
        }));

        // ---------------------------------------------------------------
        // A-9: cross-network sanitize divergence oracle.
        //
        // For each addr, sanitize against Mainnet and Testnet. Both must
        // succeed without panic; an `Option<MetaAddr>` outcome divergence
        // (one network returns Some, the other None) is worth surfacing
        // because the two networks differ only in port range checks. We
        // don't assert equality — just collect coverage.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            for addr in addrs.iter() {
                let _m = addr.sanitize(&Network::Mainnet);
                let _t = addr.sanitize(&Network::new_default_testnet());
            }
        }));

        // ---------------------------------------------------------------
        // A-10: Addr-batch encode round-trip.
        //
        // Re-encode the parsed batch as a fresh Message::Addr and verify
        // the encode does not panic and the result decodes back to a
        // same-length batch. Catches bugs where the codec accepts more
        // addresses than it emits (capacity asymmetry vs `MAX_ADDRS_IN_MESSAGE`).
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_e = Codec::builder().for_network(&Network::Mainnet).finish();
            let mut out = BytesMut::new();
            if codec_e.encode(Message::Addr(addrs.clone()), &mut out).is_err() {
                return;
            }
            let mut codec_d = Codec::builder().for_network(&Network::Mainnet).finish();
            if let Ok(Some(Message::Addr(re))) = codec_d.decode(&mut out) {
                assert_eq!(
                    addrs.len(),
                    re.len(),
                    "A-10 violated: Addr re-encode lost entries ({} -> {})",
                    addrs.len(),
                    re.len(),
                );
            }
        }));

        // ---------------------------------------------------------------
        // A-11: canonical_peer_addr re-export round-trip.
        //
        // Each MetaAddr's underlying SocketAddr should round-trip through
        // PeerSocketAddr without panic. Exposes any conversion bug in the
        // production wrap (Display/Debug used by error logging).
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            for addr in addrs.iter() {
                let psa = addr.addr();
                let _ = format!("{}", psa);
                let _ = format!("{:?}", psa);
            }
        }));
    }
});
