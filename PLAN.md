# Plan: Move zebra-crosslink into other modules

## Summary

Remove the `zebra-crosslink` crate by distributing its code to appropriate modules, removing the tenderlink/malachite/protobuf stack, replacing all network encoding with `ZcashSerialize`/`ZcashDeserialize`, and integrating BFT consensus networking into `zebra-network`.

## Key Findings

- `malctx.rs` is dead code (not declared as a module in `lib.rs`). It contains Malachite trait implementations that are no longer compiled. Delete entirely.
- `FatPointerToBftBlock2` (in `lib.rs`) is a duplicate of `FatPointerToBftBlock` (in `zebra-chain`). Unify them.
- The types in `lib.rs` that DON'T depend on tenderlink/malachite: `MalVote`, `MalPublicKey2`, `MalValidator`, `FatPointerToBftBlock2`, `FatPointerSignature2`, `TFLServiceInternal`, and all the service logic functions. These already use `ZcashSerialize` or raw byte formats.
- The types that DO depend on tenderlink: `From<tenderlink::FatPointerToBftBlock3>`, `tenderlink::TMStatus`, `tenderlink::SortedRosterMember`, `tenderlink::entry_point()`, and the `tenderlink::Closure*` callbacks. These need replacement.
- The protobuf messages (consensus.proto, sync.proto, liveness.proto) are only used by the dead `malctx.rs` Codec and `build.rs`. They will be replaced by new `ZcashSerialize` message types.

## Phase 1: Move chain data types to `zebra-chain`

**Target**: `zebra-chain/src/crosslink/` (new module)

Move from `zebra-crosslink/src/chain.rs`:
- `BftBlock` (already has `ZcashSerialize`/`ZcashDeserialize`)
- `Blake3Hash` (already has `ZcashSerialize`/`ZcashDeserialize`)
- `ZcashCrosslinkParameters`, `PROTOTYPE_PARAMETERS`
- `InvalidBftBlock`
- `BftBlockAndFatPointerToIt`

Move from `zebra-crosslink/src/lib.rs`:
- `MalVote` → rename to `BftVote` (no longer tied to Malachite)
- `MalPublicKey2` → rename to `BftValidatorAddress`
- `MalValidator` → rename to `BftValidator`
- `FatPointerToBftBlock2` → unify with existing `zebra-chain::block::FatPointerToBftBlock`
- `FatPointerSignature2` → unify with existing `zebra-chain::block::FatPointerSignature`

**Key changes**:
- Merge `FatPointerToBftBlock2` methods (`validate_signatures()`, `inflate()`, `points_at_block_hash()`, `get_vote_template()`) into the existing `zebra-chain::block::FatPointerToBftBlock`
- Remove all `to_non_two()` conversion methods
- Update `BftBlock` to use `FatPointerToBftBlock` directly instead of `FatPointerToBftBlock2`
- Replace `blake3::Hasher::new_keyed(&tenderlink::HashKeys::default().value_id.0)` with a locally-defined constant key
- Add `ed25519-zebra` and `blake3` dependencies to `zebra-chain`

## Phase 2: Define crosslink network messages in `zebra-network`

**Target**: `zebra-network/src/protocol/external/crosslink.rs` (new file)

Replace protobuf definitions with `ZcashSerialize`/`ZcashDeserialize` types:

From `consensus.proto`:
- `CrosslinkSignedMessage` — contains either a Proposal or Vote with signature
- `CrosslinkProposal` — height, round, value, pol_round, validator_address, signature
- `CrosslinkStreamedProposal` — streamed proposal part
- `CrosslinkStreamMessage` — stream segment (data or fin)

From `sync.proto`:
- `CrosslinkStatus` — peer_id, height, earliest_height
- `CrosslinkValueRequest` / `CrosslinkValueResponse` — sync by height
- `CrosslinkSyncedValue` — value with commit certificate
- `CrosslinkCommitCertificate` — height, round, value_id, signatures
- `CrosslinkCommitSignature` — validator address + signature

From `liveness.proto`:
- `CrosslinkPolkaCertificate` — polka voting certificate
- `CrosslinkRoundCertificate` — round voting certificate
- `CrosslinkLivenessMessage` — union of vote/polka/round certificate

All types get `ZcashSerialize` and `ZcashDeserialize` impls using the same patterns as existing zebra-chain types (little-endian, length-prefixed vectors, etc.).

Add new variants to `zebra-network::protocol::external::Message`:
```rust
CrosslinkConsensus(CrosslinkSignedMessage),
CrosslinkSync(CrosslinkSyncMessage),
CrosslinkLiveness(CrosslinkLivenessMessage),
CrosslinkStatus(CrosslinkStatus),
```

Add corresponding variants to `zebra-network::Request` and `zebra-network::Response`.

## Phase 3: Add BFT peer management to `zebra-network`

**Target**: `zebra-network/src/peer_set/`

### 3a: BFT Peer Transport

Create `zebra-network/src/peer/crosslink_transport.rs`:
- QUIC/Noise (IK_25519_ChaChaPoly_BLAKE2s) transport for BFT peers
- Uses `snow` crate for Noise protocol (move dependency from zebra-crosslink)
- Connection lifecycle: DH key exchange → encrypted bidirectional channel
- Move key derivation logic (`addr_string_to_stuff`, `parse_to_ipv6_bytes`) from lib.rs

### 3b: Peer Set Integration

Modify `zebra-network/src/peer_set/set.rs`:
- Add a separate collection of BFT validator peers (distinct from PoW peers)
- Route crosslink requests to BFT peers, regular requests to PoW peers
- Broadcast crosslink messages to all connected BFT validators

Modify `zebra-network/src/peer_set/initialize.rs`:
- Accept crosslink config (listen address, public address, validator peers)
- Spawn BFT peer connection tasks alongside existing TCP peer tasks
- Manage BFT peer handshakes and connection tracking

Modify `zebra-network/src/config.rs`:
- Add crosslink network config fields (listen_address, public_address, validator_peers, etc.)

## Phase 4: Move TFL service logic to `zebrad`

**Target**: `zebrad/src/components/crosslink/` (new module)

### 4a: Service Handle & Types

Move from `zebra-crosslink/src/service.rs` and `lib.rs`:
- `TFLServiceHandle`, `TFLServiceCalls`, `TFLServiceInternal`
- `spawn_new_tfl_service()`
- `tfl_service_incoming_request()`
- `TFL_ACTIVATION_HEIGHT`

### 4b: Consensus Logic

Move from `zebra-crosslink/src/lib.rs`:
- `propose_new_bft_block()` — proposes BFT blocks
- `validate_bft_block_from_malachite()` → rename to `validate_bft_block()`
- `new_decided_bft_block_from_malachite()` → rename to `process_decided_bft_block()`
- `update_roster_for_block()`, `update_roster_for_cmd()` — staking/roster management
- `push_staking_action_from_cmd_str()` — staking command handler
- `tfl_block_sequence()`, utility functions

### 4c: Main Loop Adaptation

Rewrite `tfl_service_main_loop()`:
- Remove `tenderlink::entry_point()` call and all closure callbacks
- Instead, use `zebra-network` service to send/receive BFT consensus messages
- The BFT consensus state machine (propose/prevote/precommit/decide) becomes a local state machine that processes messages from zebra-network
- Preserve: activation check, diagnostic logging, tip polling, 125ms interval

### 4d: Config & Supporting

Move from `zebra-crosslink/src/lib.rs`:
- `config::Config` → `zebrad/src/components/crosslink/config.rs`
- `rng_private_public_key_from_address()` — key derivation
- Test/debug utilities (`TEST_MODE`, `TEST_FAILED`, `dump_test_instrs`, etc.)

Move from `zebra-crosslink/src/test_format.rs`:
- Test instruction format → `zebrad/src/components/crosslink/test_format.rs`

Move from `zebra-crosslink/src/viz.rs`:
- Visualization → `zebrad/src/components/crosslink/viz.rs` (behind `viz_gui` feature)

## Phase 5: Remove zebra-crosslink crate

1. Delete `zebra-crosslink/` directory entirely
2. Remove from workspace `Cargo.toml` members list
3. Remove `zebra-crosslink` dependency from `zebrad/Cargo.toml`
4. Update `zebrad/src/config.rs` to use new config path
5. Update `zebrad/src/commands/start.rs` to use new service path
6. Update `zebrad/src/application.rs` references
7. Update `zebrad/tests/crosslink.rs` imports
8. Update `zebra-rpc` references if any
9. Remove `tenderlink`, `prost`, `prost-build`, `malachitebft_*` from dependency tree
10. Remove `libp2p-identity`, `multiaddr` dependencies (no longer needed)

## Phase 6: Validation & Cleanup

1. `cargo check` the entire workspace
2. Fix all compilation errors from moved types and changed paths
3. Run existing tests (`zebrad/tests/crosslink.rs`)
4. Verify the `viz_gui` feature still compiles
5. Verify RPC methods still work for finality queries

## Dependencies Added/Removed

**Added to zebra-chain**: `blake3`, `ed25519-zebra` (may already be there)
**Added to zebra-network**: `snow` (Noise protocol)
**Removed from workspace**: `tenderlink`, `prost`, `prost-build`, `libp2p-identity`, `multiaddr`, all `malachitebft_*` crates

## Risk Areas

- The `tenderlink::HashKeys::default().value_id.0` constant used for BLAKE3 keyed hashing needs to be extracted or replicated
- The BFT consensus state machine currently lives inside tenderlink — without it, nodes cannot participate in consensus until a replacement is implemented
- The `tenderlink::TMStatus` enum (Pass/Fail/Indeterminate) needs a local replacement
- Audio features in viz.rs depend on macroquad which was a zebra-crosslink dependency
