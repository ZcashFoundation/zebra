//! Serialization round-trip tests for Crosslink BFT network messages.

use zebra_chain::{
    block::{FatPointerToBftBlock, Header as BlockHeader},
    crosslink::{BftBlock, BftBlockAndFatPointerToIt, BftVote, BftValidatorAddress, Blake3Hash},
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

use crate::protocol::external::crosslink::*;

/// Helper: create a minimal BftBlock for testing.
fn test_bft_block() -> BftBlock {
    BftBlock {
        version: 1,
        height: 42,
        previous_block_fat_ptr: FatPointerToBftBlock::null(),
        finalization_candidate_height: 10,
        headers: Vec::new(),
    }
}

/// Helper: create a BftVote for testing.
fn test_bft_vote() -> BftVote {
    BftVote {
        validator_address: BftValidatorAddress([0xab; 32].into()),
        value: Blake3Hash([0xcd; 32]),
        height: 100,
        typ: false,
        round: 3,
    }
}

/// Helper: create a BftBlockAndFatPointerToIt for testing.
fn test_decided() -> BftBlockAndFatPointerToIt {
    BftBlockAndFatPointerToIt {
        block: test_bft_block(),
        fat_ptr: FatPointerToBftBlock::null(),
    }
}

// ---- CrosslinkVote round-trip ----

#[test]
fn crosslink_vote_round_trip() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkVote {
        vote: test_bft_vote(),
        signature: [0x11; 64],
    };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkVote::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn crosslink_vote_fixed_size() {
    let _init_guard = zebra_test::init();

    let vote = CrosslinkVote {
        vote: test_bft_vote(),
        signature: [0x22; 64],
    };

    let bytes = vote.zcash_serialize_to_vec().unwrap();
    // 76 bytes vote + 64 bytes signature = 140 bytes
    assert_eq!(bytes.len(), 76 + 64);
}

#[test]
fn crosslink_vote_preserves_vote_fields() {
    let _init_guard = zebra_test::init();

    let vote = test_bft_vote();
    let cl_vote = CrosslinkVote {
        vote: vote.clone(),
        signature: [0x33; 64],
    };

    let bytes = cl_vote.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkVote::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(deserialized.vote.validator_address, vote.validator_address);
    assert_eq!(deserialized.vote.value, vote.value);
    assert_eq!(deserialized.vote.height, vote.height);
    assert_eq!(deserialized.vote.typ, vote.typ);
    assert_eq!(deserialized.vote.round, vote.round);
}

// ---- CrosslinkStatus round-trip ----

#[test]
fn crosslink_status_round_trip() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkStatus {
        bft_tip_height: 12345,
        earliest_height: 100,
    };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkStatus::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn crosslink_status_fixed_size() {
    let _init_guard = zebra_test::init();

    let status = CrosslinkStatus {
        bft_tip_height: u64::MAX,
        earliest_height: 0,
    };

    let bytes = status.zcash_serialize_to_vec().unwrap();
    // 8 + 8 = 16 bytes
    assert_eq!(bytes.len(), 16);
}

#[test]
fn crosslink_status_boundary_values() {
    let _init_guard = zebra_test::init();

    for (tip, earliest) in [(0, 0), (u64::MAX, u64::MAX), (1, 0), (u64::MAX, 1)] {
        let original = CrosslinkStatus {
            bft_tip_height: tip,
            earliest_height: earliest,
        };

        let bytes = original.zcash_serialize_to_vec().unwrap();
        let deserialized = CrosslinkStatus::zcash_deserialize(&bytes[..]).unwrap();
        assert_eq!(original, deserialized);
    }
}

// ---- CrosslinkSyncRequest round-trip ----

#[test]
fn crosslink_sync_request_round_trip() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkSyncRequest { height: 999 };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkSyncRequest::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn crosslink_sync_request_fixed_size() {
    let _init_guard = zebra_test::init();

    let req = CrosslinkSyncRequest { height: 42 };
    let bytes = req.zcash_serialize_to_vec().unwrap();
    assert_eq!(bytes.len(), 8);
}

// ---- CrosslinkSyncResponse round-trip ----

#[test]
fn crosslink_sync_response_round_trip() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkSyncResponse {
        height: 50,
        decided: test_decided(),
    };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkSyncResponse::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

// ---- CrosslinkDecided round-trip ----

#[test]
fn crosslink_decided_round_trip() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkDecided {
        decided: test_decided(),
    };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkDecided::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

// ---- CrosslinkProposal round-trip ----

#[test]
fn crosslink_proposal_round_trip() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkProposal {
        height: 7,
        round: 2,
        block: test_bft_block(),
        pol_round: -1,
        proposer: [0xaa; 32],
        signature: [0xbb; 64],
    };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkProposal::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

#[test]
fn crosslink_proposal_with_pol_round() {
    let _init_guard = zebra_test::init();

    let original = CrosslinkProposal {
        height: 100,
        round: 5,
        block: test_bft_block(),
        pol_round: 3,
        proposer: [0xcc; 32],
        signature: [0xdd; 64],
    };

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = CrosslinkProposal::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
    assert_eq!(deserialized.pol_round, 3);
}

// ---- BftVote serialization ----

#[test]
fn bft_vote_round_trip_via_bytes() {
    let _init_guard = zebra_test::init();

    let vote = test_bft_vote();
    let bytes = vote.to_bytes();
    assert_eq!(bytes.len(), 76);

    let deserialized = BftVote::from_bytes(&bytes);

    assert_eq!(vote.validator_address, deserialized.validator_address);
    assert_eq!(vote.value, deserialized.value);
    assert_eq!(vote.height, deserialized.height);
    assert_eq!(vote.typ, deserialized.typ);
    assert_eq!(vote.round, deserialized.round);
}

#[test]
fn bft_vote_commit_flag_preserved() {
    let _init_guard = zebra_test::init();

    let prevote = BftVote {
        validator_address: BftValidatorAddress([1; 32].into()),
        value: Blake3Hash([2; 32]),
        height: 50,
        typ: false,
        round: 7,
    };

    let precommit = BftVote {
        typ: true,
        ..prevote.clone()
    };

    let prevote_bytes = prevote.to_bytes();
    let precommit_bytes = precommit.to_bytes();

    // They should differ only in the round/type field
    assert_ne!(prevote_bytes, precommit_bytes);

    let prevote_rt = BftVote::from_bytes(&prevote_bytes);
    let precommit_rt = BftVote::from_bytes(&precommit_bytes);

    assert!(!prevote_rt.typ);
    assert!(precommit_rt.typ);
    assert_eq!(prevote_rt.round, precommit_rt.round);
}

// ---- BftBlock serialization ----

#[test]
fn bft_block_round_trip() {
    let _init_guard = zebra_test::init();

    let block = test_bft_block();

    let bytes = block.zcash_serialize_to_vec().unwrap();
    let deserialized = BftBlock::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(block, deserialized);
}

#[test]
fn bft_block_and_fat_pointer_round_trip() {
    let _init_guard = zebra_test::init();

    let original = test_decided();

    let bytes = original.zcash_serialize_to_vec().unwrap();
    let deserialized = BftBlockAndFatPointerToIt::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(original, deserialized);
}

// ---- FatPointerToBftBlock serialization ----

#[test]
fn fat_pointer_null_round_trip() {
    let _init_guard = zebra_test::init();

    let null_ptr = FatPointerToBftBlock::null();

    let bytes = null_ptr.zcash_serialize_to_vec().unwrap();
    let deserialized = FatPointerToBftBlock::zcash_deserialize(&bytes[..]).unwrap();

    assert_eq!(null_ptr, deserialized);
    // 44 bytes vote data + 2 bytes signature count (0)
    assert_eq!(bytes.len(), 44 + 2);
}

#[test]
fn fat_pointer_null_points_at_zero_hash() {
    let _init_guard = zebra_test::init();

    let null_ptr = FatPointerToBftBlock::null();
    assert_eq!(null_ptr.points_at_block_hash(), [0u8; 32]);
}

// ---- Deserialization error handling ----

#[test]
fn crosslink_vote_too_short_fails() {
    let _init_guard = zebra_test::init();

    let short_bytes = [0u8; 10];
    let result = CrosslinkVote::zcash_deserialize(&short_bytes[..]);
    assert!(result.is_err());
}

#[test]
fn crosslink_status_too_short_fails() {
    let _init_guard = zebra_test::init();

    let short_bytes = [0u8; 4];
    let result = CrosslinkStatus::zcash_deserialize(&short_bytes[..]);
    assert!(result.is_err());
}

#[test]
fn crosslink_sync_request_too_short_fails() {
    let _init_guard = zebra_test::init();

    let short_bytes = [0u8; 3];
    let result = CrosslinkSyncRequest::zcash_deserialize(&short_bytes[..]);
    assert!(result.is_err());
}

#[test]
fn crosslink_proposal_too_short_fails() {
    let _init_guard = zebra_test::init();

    // Only height (8 bytes) - missing everything else
    let short_bytes = [0u8; 8];
    let result = CrosslinkProposal::zcash_deserialize(&short_bytes[..]);
    assert!(result.is_err());
}
