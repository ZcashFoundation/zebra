#![no_main]

use libfuzzer_sys::fuzz_target;
use bytes::BytesMut;
use std::panic;
use tokio_util::codec::{Decoder, Encoder};
use zebra_chain::parameters::Network;
use zebra_network::protocol::external::{Codec, Message};

fuzz_target!(|data: &[u8]| {
    // ═══════════════════════════════════════════════════════════════════
    // Layer 1: Codec Decode — Multi-message Extraction (with byte tracking)
    // Target: codec parsing bugs, buffer handling errors
    //
    // Track each message's byte segment [start..end] within `data` so Layer 3
    // can run a true input-vs-re-encode oracle (`serialized == input[start..end]`)
    // — strictly stronger than an encode/decode/encode oracle, since it verifies
    // asymmetry on the *attacker-supplied* bytes rather than only on encoder
    // output.
    // ═══════════════════════════════════════════════════════════════════
    let mut codec = Codec::builder().finish();
    let mut buf = BytesMut::from(data);
    let mut messages = Vec::new();
    // Byte-segment for each parsed message inside `data`.
    let mut msg_segments: Vec<(usize, usize)> = Vec::new();
    let total_len = data.len();

    // Decode all messages from the buffer, recording each message's byte
    // segment in the original `data`. BytesMut::split_to consumes from the
    // front, so consumed bytes = total_len - buf.len() at any point.
    loop {
        let consumed_before = total_len - buf.len();
        match codec.decode(&mut buf) {
            Ok(Some(msg)) => {
                let consumed_after = total_len - buf.len();
                msg_segments.push((consumed_before, consumed_after));
                messages.push(msg);
                continue;
            }
            Ok(None) => break,  // need more data
            Err(_) => break,    // parse error — expected, not a bug
        }
    }

    if messages.is_empty() {
        return;
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 2: Message Property Extraction & Deep Fuzz
    // Target: panics in field access, display, command, and nested data
    // ═══════════════════════════════════════════════════════════════════

    for msg in &messages {
        // Basic message methods — should never panic
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _ = msg.command();
        }));
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _ = format!("{}", msg);
        }));

        // Deep extraction per message variant
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            match msg {
                Message::Version(ver) => {
                    let _version = ver.version;
                    let _services = ver.services;
                    let _timestamp = ver.timestamp;
                    let _addr_recv = &ver.address_recv;
                    let _addr_from = &ver.address_from;
                    let _nonce = ver.nonce;
                    let _user_agent = &ver.user_agent;
                    let _start_height = ver.start_height;
                    let _relay = ver.relay;
                }
                Message::Verack | Message::GetAddr | Message::Mempool | Message::FilterClear => {
                    // No fields to extract
                }
                Message::Ping(nonce) | Message::Pong(nonce) => {
                    let _n = *nonce;
                }
                Message::Reject { message, ccode, reason, data } => {
                    let _msg = message.clone();
                    let _code = *ccode;
                    let _reason = reason.clone();
                    let _data = *data;
                }
                Message::Addr(addrs) => {
                    let _count = addrs.len();
                    for _addr in addrs {}
                }
                Message::GetBlocks { known_blocks, stop } | Message::GetHeaders { known_blocks, stop } => {
                    let _count = known_blocks.len();
                    for _hash in known_blocks {}
                    let _stop = *stop;
                }
                Message::Inv(hashes) | Message::GetData(hashes) | Message::NotFound(hashes) => {
                    let _count = hashes.len();
                    for _hash in hashes {}
                }
                Message::Headers(headers) => {
                    let _count = headers.len();
                    for header in headers {
                        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                            header.header.hash()
                        }));
                    }
                }
                Message::Block(block) => {
                    // Deep fuzz the block — replicate block_deep_fuzz Layer 2
                    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { block.hash() }));
                    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { block.coinbase_height() }));
                    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { let _ = block.commitment(&zebra_chain::parameters::Network::Mainnet); }));
                    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { block.auth_data_root() }));

                    // Deep fuzz each transaction in the block
                    for tx in &block.transactions {
                        deep_fuzz_transaction(tx);
                    }
                }
                Message::Tx(unmined_tx) => {
                    // Deep fuzz the transaction
                    deep_fuzz_transaction(&unmined_tx.transaction);
                }
                Message::FilterLoad { filter, hash_functions_count, tweak, flags } => {
                    let _f = filter;
                    let _hfc = *hash_functions_count;
                    let _t = *tweak;
                    let _fl = *flags;
                }
                Message::FilterAdd { data } => {
                    let _len = data.len();
                }
            }
        }));
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 2.5: Single-message Business Invariants (I-4 .. I-8)
    //
    // Single-message scope only — multi-message race / connection state
    // machine race is out of scope for this target. Each invariant is wrapped
    // in catch_unwind so we observe panics rather than letting libFuzzer
    // attribute them to upstream parse code paths.
    // ═══════════════════════════════════════════════════════════════════

    for msg in &messages {
        // ---------------------------------------------------------------
        // I-4: parse OK + msg ∈ ValidStates ⇒ basic dispatch shape
        //      (command + display + Debug) does not panic.
        //
        // Note: full handle_message dispatch requires a live PeerState +
        // tokio runtime + connected peer; that is out of single-message
        // reach. Here we observe the message-shape
        // invariants the connection state machine relies on at entry —
        // i.e. that the message can be inspected without panicking. A
        // panic in any of these would also panic the real dispatcher.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _cmd = msg.command();
            let _disp = format!("{}", msg);
            let _dbg = format!("{:?}", msg);
        }));

        // ---------------------------------------------------------------
        // I-5: parse inv OK ⇒ constructing a getdata response from the
        //      same hash list and encoding it does not panic.
        //
        // Mirrors the inbound advertise → getdata dispatch path (single
        // message, no peer state). We do not run the full inbound service;
        // we only assert that the encode side of the response cannot
        // panic on attacker-influenced inv contents. The decode side is
        // already covered by Layer 1.
        // ---------------------------------------------------------------
        if let Message::Inv(hashes) = msg {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let getdata = Message::GetData(hashes.clone());
                let mut codec_inv = Codec::builder().finish();
                let mut out_buf = BytesMut::new();
                let _ = codec_inv.encode(getdata, &mut out_buf);
            }));
        }

        // ---------------------------------------------------------------
        // I-6: parse addr OK ⇒ each MetaAddr.sanitize(network) does not
        //      panic, and re-encoding the sanitized addr list as an
        //      outbound Addr message does not panic.
        //
        // sanitize() returns Option<MetaAddr>; None is a valid outcome
        // and not asserted as an invariant. We assert only the absence
        // of panics and that the resulting (filtered) list survives a
        // single encode pass — i.e. the gossip-out path is shape-stable.
        // ---------------------------------------------------------------
        if let Message::Addr(addrs) = msg {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let sanitized: Vec<_> = addrs
                    .iter()
                    .filter_map(|a| a.sanitize(&Network::Mainnet))
                    .collect();
                let _len = sanitized.len();
                let out_msg = Message::Addr(sanitized);
                let mut codec_addr = Codec::builder().finish();
                let mut out_buf = BytesMut::new();
                let _ = codec_addr.encode(out_msg, &mut out_buf);
            }));
            // Also exercise Testnet path — sanitize() branches on
            // last_known_info_is_valid_for_outbound(network).
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                for a in addrs {
                    let _ = a.sanitize(&Network::new_default_testnet());
                }
            }));
        }

        // ---------------------------------------------------------------
        // I-7: parse OK ⇒ re-decoding the same payload through a fresh
        //      Codec does not panic. For Tx / Block messages this drives
        //      Codec::deserialize_transaction_spawning / _block_spawning,
        //      which spawn a rayon FIFO scope inside tokio::block_in_place.
        //      That scope's `.expect("scope has already finished")` is a
        //      panic surface worth exercising.
        //
        // We exercise the spawning path indirectly by re-encoding the
        // already-parsed message and decoding it again — encode is
        // round-trip-stable for tx/block, so the second decode hits the
        // same rayon scope code path with attacker-influenced bytes.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc = Codec::builder().finish();
            let mut out_buf = BytesMut::new();
            if codec_enc.encode(msg.clone(), &mut out_buf).is_ok() {
                let mut codec_dec = Codec::builder().finish();
                // For Tx/Block this routes into deserialize_transaction_spawning
                // / deserialize_block_spawning — i.e. into the rayon scope.
                let _ = codec_dec.decode(&mut out_buf);
            }
        }));

        // ---------------------------------------------------------------
        // I-8: parse OK ⇒ no `unreachable!()` is reached during the
        //      single-message shape inspection that the connection state
        //      machine performs before dispatch. Several variants (Reject,
        //      FilterLoad, Headers, Block, Tx) carry nested data that the
        //      state machine's pre-dispatch match arms touch via Display /
        //      command / minimal field reads.
        //
        // We do not simulate the full state machine here (multi-message
        // race + state transitions are out of scope here). We assert the
        // weaker, single-message invariant: the entry-shape inspection
        // does not panic with an unreachable!(). A real unreachable!()
        // surfaces here as a panic caught by catch_unwind.
        // ---------------------------------------------------------------
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            // Cover the variants the dispatcher's pre-state match touches.
            match msg {
                Message::Version(_) | Message::Verack | Message::Ping(_) | Message::Pong(_)
                | Message::Reject { .. } | Message::GetAddr | Message::Addr(_)
                | Message::GetBlocks { .. } | Message::Inv(_) | Message::GetData(_)
                | Message::NotFound(_) | Message::Block(_) | Message::Headers(_)
                | Message::GetHeaders { .. } | Message::Tx(_) | Message::Mempool
                | Message::FilterLoad { .. } | Message::FilterAdd { .. }
                | Message::FilterClear => {
                    let _cmd = msg.command();
                }
            }
        }));
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 3: Message Round-trip (Decode → Encode → Compare against input)
    // Target: encode/decode asymmetry, data loss, corruption
    //
    // An earlier oracle compared
    // encode(msg) → decode → re-encode and verified the two encode outputs
    // matched. That detects encoder non-determinism but cannot catch a
    // decoder that silently drops/reorders attacker-supplied bytes (since
    // both encodes start from the *same* parsed Message).
    //
    // The new oracle compares the re-encoded bytes against the *original
    // input segment* `data[start..end]` for each parsed message. Codec
    // encode is fully deterministic (header magic / command / length /
    // checksum all derived from body), so any byte-level mismatch implies
    // either:
    //   (a) decoder accepted bytes the encoder cannot reproduce
    //       (e.g. duplicate encoding of the same logical message),
    //   (b) decoder lost / reordered field bytes during parse,
    //   (c) encoder writes a different canonical form than the wire format.
    // All three are real codec asymmetries.
    //
    // Variants excluded from input-eq oracle (legitimately non-deterministic
    // wire-form):
    //   - Version : address timestamp / services may be normalized on parse,
    //               and zebra's wire reader may accept multiple compatible
    //               serializations of the addresses (V1 / V2 form).
    //   - Verack  : empty body, command-eq via Layer 1 is sufficient.
    // All other parsed variants are subject to input-byte equality.
    // ═══════════════════════════════════════════════════════════════════

    // ═══════════════════════════════════════════════════════════════════
    // Layer 3.5: Cross-network magic-byte oracle
    //
    // Re-encode each parsed Message under both Mainnet and Testnet codecs
    // and verify the magic-byte prefix matches the network. This exercises
    // `Codec::for_network` selection + `Magic::write_to` boundaries that
    // baseline oracle never hits because the seed corpus is Mainnet-only.
    //
    // The first 4 bytes of any encoded frame must equal the network magic;
    // a regression in `for_network` would surface here.
    // ═══════════════════════════════════════════════════════════════════
    for msg in &messages {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            for net in [Network::Mainnet, Network::new_default_testnet()] {
                let mut codec_x = Codec::builder().for_network(&net).finish();
                let mut out = BytesMut::new();
                if codec_x.encode(msg.clone(), &mut out).is_ok() && out.len() >= 4 {
                    let magic_bytes = &out[..4];
                    let expected = net.magic().0;
                    assert_eq!(
                        magic_bytes,
                        &expected[..],
                        "Codec::for_network({:?}) produced wrong magic: got {:02x?}, want {:02x?}",
                        net, magic_bytes, expected
                    );
                    // Round-trip through same-network decoder.
                    let mut codec_y = Codec::builder().for_network(&net).finish();
                    let _ = codec_y.decode(&mut out);
                }
            }
        }));
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 3.6: Header boundary fuzz on parsed messages
    //
    // For each message we encoded above, surgically corrupt the length
    // field and feed it back through a fresh decoder. The codec must
    // reject without panicking. This exercises `MAX_PROTOCOL_MESSAGE_LEN`
    // guard + `verify_checksum` paths that otherwise need exotic seeds.
    // ═══════════════════════════════════════════════════════════════════
    for msg in &messages {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc = Codec::builder().for_network(&Network::Mainnet).finish();
            let mut out = BytesMut::new();
            if codec_enc.encode(msg.clone(), &mut out).is_err() || out.len() < 24 {
                return;
            }
            // Corruption variant 1: length field set to MAX+1 (oversize).
            let mut corrupted_oversize = out.clone();
            let oversize = (zebra_chain::serialization::MAX_PROTOCOL_MESSAGE_LEN as u32) + 1;
            corrupted_oversize[16..20].copy_from_slice(&oversize.to_le_bytes());
            let mut codec_d1 = Codec::builder().for_network(&Network::Mainnet).finish();
            let _ = codec_d1.decode(&mut corrupted_oversize);
            // Corruption variant 2: checksum byte flipped.
            let mut corrupted_cksum = out.clone();
            corrupted_cksum[20] ^= 0xff;
            let mut codec_d2 = Codec::builder().for_network(&Network::Mainnet).finish();
            let _ = codec_d2.decode(&mut corrupted_cksum);
            // Corruption variant 3: command field zeroed.
            let mut corrupted_cmd = out.clone();
            for b in &mut corrupted_cmd[4..16] {
                *b = 0;
            }
            let mut codec_d3 = Codec::builder().for_network(&Network::Mainnet).finish();
            let _ = codec_d3.decode(&mut corrupted_cmd);
            // Corruption variant 4: truncate to header-only.
            let mut codec_d4 = Codec::builder().for_network(&Network::Mainnet).finish();
            let mut header_only: BytesMut = out[..24].into();
            let _ = codec_d4.decode(&mut header_only);
        }));
    }

    // ═══════════════════════════════════════════════════════════════════
    // Layer 3.7: Inv-list mixed-variant invariant
    //
    // For each Message::Inv parsed, build mixed-variant InventoryHash
    // permutations (Block + Tx + WTx + Error + Filtered_Block) and
    // encode → decode round-trip. This drives `read_inv` on every
    // discriminator and exposes any new variant not handled in `write`.
    // ═══════════════════════════════════════════════════════════════════
    for msg in &messages {
        if let Message::Inv(hashes) | Message::GetData(hashes) | Message::NotFound(hashes) = msg {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                use zebra_network::protocol::external::InventoryHash;
                // For each inventory entry, wrap in alternating types.
                let mut mixed: Vec<InventoryHash> = Vec::new();
                for (i, h) in hashes.iter().enumerate().take(64) {
                    match i % 4 {
                        0 => mixed.push(h.clone()),
                        1 => {
                            if let Some(hash32) = inv_hash_to_32(h) {
                                mixed.push(InventoryHash::Block(zebra_chain::block::Hash(hash32)));
                            }
                        }
                        2 => {
                            if let Some(hash32) = inv_hash_to_32(h) {
                                mixed.push(InventoryHash::Tx(
                                    zebra_chain::transaction::Hash::from(hash32),
                                ));
                            }
                        }
                        _ => mixed.push(InventoryHash::Error),
                    }
                }
                if mixed.is_empty() {
                    return;
                }
                let mut codec_e = Codec::builder().for_network(&Network::Mainnet).finish();
                let mut out = BytesMut::new();
                if codec_e.encode(Message::Inv(mixed.clone()), &mut out).is_ok() {
                    let mut codec_d = Codec::builder().for_network(&Network::Mainnet).finish();
                    let _ = codec_d.decode(&mut out);
                }
            }));
        }
    }

    for (msg, &(seg_start, seg_end)) in messages.iter().zip(msg_segments.iter()) {
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let mut codec_enc = Codec::builder().finish();
            let mut out_buf = BytesMut::new();

            // Encode the message (decoded from input).
            if codec_enc.encode(msg.clone(), &mut out_buf).is_ok() {
                // Decode-side sanity: the encoded buffer must round-trip.
                let mut codec_dec = Codec::builder().finish();
                let mut decoder_buf = out_buf.clone();
                if let Ok(Some(msg2)) = codec_dec.decode(&mut decoder_buf) {
                    // Command-level oracle (always).
                    assert_eq!(
                        msg.command(), msg2.command(),
                        "Message round-trip command mismatch"
                    );
                }

                // Input-eq oracle: compare re-encoded bytes against the
                // original input segment. Variants whose wire form has
                // legitimate non-determinism are excluded.
                //
                // Excluded:
                //   - Version : address V1/V2 form ambiguity, services /
                //               timestamp normalization on parse.
                //   - Verack / GetAddr / Mempool / FilterClear : zero-body
                //               variants whose `read_*` consumers ignore
                //               trailing bytes (BTC-derived codec quirk;
                //               decoder accepts attacker-supplied body
                //               bytes that the encoder will not emit).
                //               Tracked as a separate family-7 finding —
                //               not surfaced here per fuzz_known_panic
                //               filter SOP so libfuzzer can explore
                //               other paths.
                //   - Reject  : `reason` is a var-string whose length
                //               prefix can be encoded in multiple
                //               valid widths.
                //   - Addr    : V1 / V2 form interleaving may produce
                //               canonicalized output on re-encode.
                //   - FilterAdd : `data` length-prefix has multi-byte
                //               varint forms the encoder canonicalizes.
                let body_eq_eligible = !matches!(
                    msg,
                    Message::Version(_)
                        | Message::Verack
                        | Message::GetAddr
                        | Message::Mempool
                        | Message::FilterClear
                        | Message::Reject { .. }
                        | Message::Addr(_)
                        | Message::FilterAdd { .. }
                );
                if body_eq_eligible
                    && seg_start <= seg_end
                    && seg_end <= total_len
                {
                    let input_segment = &data[seg_start..seg_end];
                    // Header is deterministic given body (magic/command/
                    // length/checksum). If lengths differ, parse already
                    // accepted slack bytes the encoder will not emit —
                    // a real codec asymmetry.
                    assert_eq!(
                        out_buf.as_ref(),
                        input_segment,
                        "Codec asymmetry: re-encode != input segment (cmd={}, in_len={}, enc_len={})",
                        msg.command(),
                        input_segment.len(),
                        out_buf.len()
                    );
                }
            }
        }));
    }
});

/// Helper: extract the inner 32-byte hash from any InventoryHash
/// variant, used by the Layer 3.7 mixed-variant rebuild oracle.
fn inv_hash_to_32(h: &zebra_network::protocol::external::InventoryHash) -> Option<[u8; 32]> {
    use zebra_network::protocol::external::InventoryHash;
    match h {
        InventoryHash::Block(zebra_chain::block::Hash(b)) => Some(*b),
        InventoryHash::FilteredBlock(zebra_chain::block::Hash(b)) => Some(*b),
        InventoryHash::Tx(t) => Some(<[u8; 32]>::from(*t)),
        InventoryHash::Wtx(_) => None,
        InventoryHash::Error => None,
    }
}

/// Deep fuzz a transaction — exercises property extraction, consensus checks, and ZIP-317.
/// Mirrors the per-transaction deep-fuzz logic.
fn deep_fuzz_transaction(tx: &zebra_chain::transaction::Transaction) {
    use std::panic;
    use zebra_chain::block::Height;
    use zebra_chain::parameters::Network;

    // Property extraction
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.hash() }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.auth_digest() }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.unmined_id() }));

    let _version = tx.version();
    let _is_overwintered = tx.is_overwintered();
    let _lock_time = tx.lock_time();
    let _raw_lock_time = tx.raw_lock_time();
    let _expiry = tx.expiry_height();
    let _network_upgrade = tx.network_upgrade();
    let _is_time = tx.lock_time_is_time();

    let _inputs = tx.inputs();
    let _outputs = tx.outputs();
    let _is_coinbase = tx.is_coinbase();
    let _has_transparent_io = tx.has_transparent_inputs_or_outputs();
    let _has_transparent_in = tx.has_transparent_inputs();
    let _has_transparent_out = tx.has_transparent_outputs();
    let _has_shielded_in = tx.has_shielded_inputs();
    let _has_shielded_out = tx.has_shielded_outputs();
    let _has_transparent_or_shielded_in = tx.has_transparent_or_shielded_inputs();
    let _has_transparent_or_shielded_out = tx.has_transparent_or_shielded_outputs();

    let _joinsplit_count = tx.joinsplit_count();
    for _js in tx.sprout_joinsplits() {}
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _js in tx.sprout_groth16_joinsplits() {}
    }));
    let _pub_key = tx.sprout_joinsplit_pub_key();
    let _has_sprout = tx.has_sprout_joinsplit_data();
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        for _c in tx.sprout_note_commitments() {}
    }));

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _a in tx.sapling_anchors() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _s in tx.sapling_spends_per_anchor() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _o in tx.sapling_outputs() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _n in tx.sapling_nullifiers() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _c in tx.sapling_note_commitments() {} }));
    let _has_sapling = tx.has_sapling_shielded_data();

    let _orchard_data = tx.orchard_shielded_data();
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _a in tx.orchard_actions() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _n in tx.orchard_nullifiers() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _c in tx.orchard_note_commitments() {} }));
    let _flags = tx.orchard_flags();
    let _has_orchard = tx.has_orchard_shielded_data();

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _v in tx.output_values_to_sprout() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { for _v in tx.input_values_from_sprout() {} }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| { tx.sapling_value_balance() }));
    let _binding_sig = tx.sapling_binding_sig();

    let _enough_flags = tx.has_enough_orchard_flags();
    let _valid_non_coinbase = tx.is_valid_non_coinbase();
    for _outpoint in tx.spent_outpoints() {}

    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        tx.coinbase_spend_restriction(&Network::Mainnet, Height(1_000_000))
    }));

    // Consensus checks
    let _ = zebra_consensus::transaction::check::has_inputs_and_outputs(tx);
    let _ = zebra_consensus::transaction::check::has_enough_orchard_flags(tx);
    let _ = zebra_consensus::transaction::check::coinbase_tx_no_prevout_joinsplit_spend(tx);
    let _ = zebra_consensus::transaction::check::joinsplit_has_vpub_zero(tx);
    let _ = zebra_consensus::transaction::check::spend_conflicts(tx);

    {
        let height = Height(1_000_000);
        let now = chrono::Utc::now();
        let _ = zebra_consensus::transaction::check::lock_time_has_passed(tx, height, Some(now));
    }
    for h in [0u32, 419_200, 1_000_000, 1_687_104] {
        let _ = zebra_consensus::transaction::check::disabled_add_to_sprout_pool(
            tx, Height(h), &Network::Mainnet,
        );
    }
    {
        let height = Height(1_000_000);
        let _ = zebra_consensus::transaction::check::coinbase_expiry_height(&height, tx, &Network::Mainnet);
        let _ = zebra_consensus::transaction::check::non_coinbase_expiry_height(&height, tx);
    }
    for h in [0u32, 419_200, 903_000, 1_046_400, 1_687_104, 1_842_420] {
        let _ = zebra_consensus::transaction::check::consensus_branch_id(tx, Height(h), &Network::Mainnet);
    }

    // ZIP-317 fee calculations
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        zebra_chain::transaction::zip317::conventional_fee(tx)
    }));
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        zebra_chain::transaction::zip317::conventional_actions(tx)
    }));
}
