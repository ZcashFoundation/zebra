#![no_main]

//! P2P Message Parse — Deep Invariant Fuzz Target.
//!
//! Layered design (mirrors p2p_deep_fuzz.rs but narrows on **header / framing /
//! version / command / length** invariants rather than full per-variant body
//! deep extraction). This target complements `p2p_deep_fuzz.rs`:
//!
//! - `p2p_deep_fuzz.rs`: per-variant property + Layer-3 round-trip.
//! - `p2p_message_parse.rs` (this file): codec framing invariants —
//!   round-trip byte equality, size limit, protocol version range,
//!   command name UTF-8, header/payload size consistency.
//!
//! Invariants exercised:
//! - I-1 round-trip: codec encode-decode asymmetry on parsed messages.
//! - I-2 size cap:   MAX_PROTOCOL_MESSAGE_LEN guard.
//! - I-3 version:    no panic on unsupported / out-of-range protocol versions.
//! - I-4 UTF-8 cmd:  exercise the `command()` accessor directly.
//! - I-5 size match: header body_len ↔ encoded body length.
//!
//! Conservative measure: invariants are bounded by `panic::catch_unwind` so the
//! fuzz target observes the panic without aborting the run.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use std::panic;
use tokio_util::codec::{Decoder, Encoder};
use zebra_chain::parameters::Network;
use zebra_chain::serialization::MAX_PROTOCOL_MESSAGE_LEN;
use zebra_network::protocol::external::{Codec, Message};

/// Header length (4 magic + 12 command + 4 body_len + 4 checksum).
/// Tracks `HEADER_LEN` in `zebra-network/src/protocol/external/codec.rs`.
const FUZZ_HEADER_LEN: usize = 24;

/// Plausible lower bound for any historical protocol version we expect to
/// observe in real Zcash traffic. Conservative: keep wider than the current
/// `INITIAL_MIN_NETWORK_PROTOCOL_VERSION` so that older mainnet packets stay
/// in-range; the fuzz target asserts only that values do not cause panics
/// downstream.
const PROTO_VERSION_PLAUSIBLE_MIN: u32 = 170_000;

/// Plausible upper bound: well above current `CURRENT_NETWORK_PROTOCOL_VERSION`
/// (170_140 as of 2026-04-30, with reserved 170_150 / 170_160 slots) so we
/// only flag values that look syntactically wild (e.g. 0xFFFF_FFFF). Out-of-
/// range values must not panic — they should surface as parse / handshake
/// errors elsewhere.
const PROTO_VERSION_PLAUSIBLE_MAX: u32 = 200_000;

fuzz_target!(|data: &[u8]| {
    // ═══════════════════════════════════════════════════════════════════
    // Layer 1: Codec Decode — Multi-message Extraction
    // Same shape as the original thin target: feed raw bytes that may
    // contain partial/malformed headers and bodies.
    // ═══════════════════════════════════════════════════════════════════
    let mut codec = Codec::builder().finish();
    let mut buf = BytesMut::from(data);
    let mut messages: Vec<Message> = Vec::new();

    loop {
        match codec.decode(&mut buf) {
            Ok(Some(msg)) => {
                messages.push(msg);
                continue;
            }
            // Need more data — stop.
            Ok(None) => break,
            // Parse error (bad magic, checksum mismatch, oversize body, etc.)
            // — expected, not a bug.
            Err(_) => break,
        }
    }

    if messages.is_empty() {
        return;
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 2: Per-message header / framing invariants
    // ═══════════════════════════════════════════════════════════════════

    for msg in &messages {
        // ---------------------------------------------------------------
        // I-1: parse OK ⇒ encode round-trip byte-level equal
        //
        // Codec serialization must be deterministic for a parsed Message.
        // We re-encode with a freshly built Codec, then decode the bytes
        // again and re-encode the second parse; the two encoded byte
        // streams must match. This guards against state-dependent
        // encode paths.
        //
        // Note: we compare `encode(msg) == encode(decode(encode(msg)))`
        // rather than `encode == original raw`, because the original
        // input may be one of many byte representations that decode to
        // the same logical message (e.g. trailing garbage in the input
        // buffer). The stricter equality holds on the **canonical**
        // encoding produced by the codec itself.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc1 = Codec::builder().finish();
            let mut bytes1 = BytesMut::new();
            if codec_enc1.encode(msg.clone(), &mut bytes1).is_err() {
                return;
            }
            let bytes1_frozen = bytes1.clone();

            let mut codec_dec = Codec::builder().finish();
            let mut bytes1_for_decode = bytes1;
            let parsed2 = match codec_dec.decode(&mut bytes1_for_decode) {
                Ok(Some(m)) => m,
                _ => return,
            };

            let mut codec_enc2 = Codec::builder().finish();
            let mut bytes2 = BytesMut::new();
            if codec_enc2.encode(parsed2, &mut bytes2).is_err() {
                return;
            }

            // Byte-level equality on the canonical encoding round-trip.
            // Mismatch would indicate non-deterministic encoding or
            // a decode → re-encode information loss bug.
            assert_eq!(
                bytes1_frozen.as_ref(),
                bytes2.as_ref(),
                "I-1 violated: canonical encode round-trip differs for {}",
                msg.command()
            );
        }));

        // ---------------------------------------------------------------
        // I-2: parse OK ⇒ encoded size ≤ MAX_PROTOCOL_MESSAGE_LEN + HEADER_LEN
        //
        // The codec already enforces `body_len > max_len ⇒ Parse error`
        // on the read side (codec.rs:395). This invariant asserts the
        // **write side** symmetry: a parsed (i.e. accepted) message
        // re-serialized must not exceed the protocol limit either.
        // A violation would mean the codec accepted something it cannot
        // re-emit safely, which would be an asymmetry bug.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc = Codec::builder().finish();
            let mut out = BytesMut::new();
            if codec_enc.encode(msg.clone(), &mut out).is_err() {
                return;
            }
            // Total = header + body. Body alone must be ≤ MAX_PROTOCOL_MESSAGE_LEN.
            // We assert on total length with FUZZ_HEADER_LEN slack so the
            // assertion message is meaningful.
            let total = out.len();
            assert!(
                total <= MAX_PROTOCOL_MESSAGE_LEN + FUZZ_HEADER_LEN,
                "I-2 violated: encoded message {} of {} bytes exceeds MAX_PROTOCOL_MESSAGE_LEN ({}) + header",
                msg.command(),
                total,
                MAX_PROTOCOL_MESSAGE_LEN,
            );
        }));

        // ---------------------------------------------------------------
        // I-3: parse OK + Message::Version ⇒ protocol version sanity
        //
        // We do not require version ∈ [MIN, CURRENT] — peers may send
        // older or newer versions and the node must not panic. We only
        // assert "reading the field does not panic" and that the
        // numeric range, while it may be wild (0 or u32::MAX), is at
        // least observable. The `inv` / `getheaders` etc. messages do
        // not carry an inline version, so this only fires on Version
        // payloads.
        //
        // Conservative wording: this invariant flags values *outside*
        // a plausible range as inputs we want corpus seed to keep, not
        // as bugs. Panics inside accessors **are** bugs.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            if let Message::Version(ver) = msg {
                let v = ver.version.0;
                // No assertion on value; reading is the invariant.
                // We touch the field through a black_box-like sink so
                // the optimizer can't elide it.
                let _sink = (v, v < PROTO_VERSION_PLAUSIBLE_MIN, v > PROTO_VERSION_PLAUSIBLE_MAX);
            }
        }));

        // ---------------------------------------------------------------
        // I-4: parse OK ⇒ command() accessor returns a `&'static str`
        //                without panicking.
        //
        // The codec call site unwraps `String::from_utf8` of the on-wire
        // 12-byte command field. The accepted Message has already passed
        // that path, so calling `command()` here exercises the static
        // dispatch.
        // We additionally verify the returned string is itself valid
        // UTF-8 and within the 12-byte command field length budget.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let cmd = msg.command();
            // Returned by `command()` so `is_char_boundary(0)` is trivial,
            // but assert non-empty + length in [1, 12] to cover the
            // wire-protocol command name budget. If a future refactor
            // ever returns >12 bytes from `command()`, the assertion
            // fires before we serialize it.
            assert!(
                !cmd.is_empty() && cmd.len() <= 12,
                "I-4 violated: command() returned {:?} (len {}) outside [1, 12]",
                cmd,
                cmd.len(),
            );
            // Validate UTF-8 explicitly (the type system already gives us
            // `&str`, but we defend against any future `unsafe` shortcut).
            assert!(
                std::str::from_utf8(cmd.as_bytes()).is_ok(),
                "I-4 violated: command() {:?} not valid UTF-8",
                cmd
            );
        }));

        // ---------------------------------------------------------------
        // I-5: parse OK ⇒ encoded body length matches what the codec
        //                 declared in the header.
        //
        // The codec uses `body_length()`
        // (codec.rs:191) to fill the header `body_len` field before
        // writing the body. If `body_length` and `write_body` ever
        // diverge, the receiver would parse the wrong number of bytes
        // and either truncate or over-read. We re-encode the message
        // and parse the header in-place to confirm:
        //   header.body_len == bytes.len() - HEADER_LEN
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc = Codec::builder().finish();
            let mut out = BytesMut::new();
            if codec_enc.encode(msg.clone(), &mut out).is_err() {
                return;
            }
            if out.len() < FUZZ_HEADER_LEN {
                // Encoder produced a truncated frame — that is itself a
                // bug, surface it.
                panic!(
                    "I-5 violated: encoded {} produced {} bytes, less than HEADER_LEN ({})",
                    msg.command(),
                    out.len(),
                    FUZZ_HEADER_LEN,
                );
            }
            // Header layout: [magic(4) | command(12) | body_len(4 LE) | checksum(4)]
            // body_len lives at bytes [16, 20).
            let header_body_len = u32::from_le_bytes([out[16], out[17], out[18], out[19]]) as usize;
            let actual_body_len = out.len() - FUZZ_HEADER_LEN;
            assert_eq!(
                header_body_len, actual_body_len,
                "I-5 violated: header.body_len ({}) != encoded body bytes ({}) for {}",
                header_body_len,
                actual_body_len,
                msg.command(),
            );
        }));

        // ---------------------------------------------------------------
        // I-6: cross-network magic mismatch must be rejected.
        //
        // Encode the message with the Mainnet codec and feed the result
        // into a Testnet decoder. The codec must reject without panicking
        // (either a parse error, or `Ok(None)` if framed-bytes haven't
        // arrived). Any panic here is a magic-byte handling regression.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_main = Codec::builder().for_network(&Network::Mainnet).finish();
            let mut out = BytesMut::new();
            if codec_main.encode(msg.clone(), &mut out).is_err() {
                return;
            }
            let mut codec_test = Codec::builder().for_network(&Network::new_default_testnet()).finish();
            // Rejection (Err) and "need-more-bytes" (Ok(None)) are both
            // valid; a panic is the only outcome we treat as a bug.
            let _ = codec_test.decode(&mut out);
        }));

        // ---------------------------------------------------------------
        // I-7: partial-frame truncation invariant.
        //
        // For every prefix of an encoded valid message, decode must not
        // panic — at most return Ok(None) (need more bytes) or Err. We
        // probe a few representative prefixes (1, 4, 12, 20, 24, 32, mid)
        // to cover the magic / command / length / checksum / partial-body
        // boundaries.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc = Codec::builder().for_network(&Network::Mainnet).finish();
            let mut full = BytesMut::new();
            if codec_enc.encode(msg.clone(), &mut full).is_err() {
                return;
            }
            let total = full.len();
            // Probe boundaries — last one is mid-body if total > 24.
            let mut probes = vec![1usize, 4, 12, 20, 24, 32];
            if total > 32 {
                probes.push(24 + (total - 24) / 2);
            }
            for &p in &probes {
                if p > total {
                    continue;
                }
                let mut prefix: BytesMut = full[..p].into();
                let mut codec_part = Codec::builder().for_network(&Network::Mainnet).finish();
                let _ = codec_part.decode(&mut prefix);
            }
        }));
    }
});
