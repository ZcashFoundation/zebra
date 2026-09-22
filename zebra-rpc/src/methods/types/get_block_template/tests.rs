//! Tests for types and functions for the `getblocktemplate` RPC.

use anyhow::anyhow;
use std::iter;
use zebra_chain::amount::Amount;

use strum::IntoEnumIterator;
use zcash_keys::address::Address;

use zebra_chain::parameters::testnet::ConfiguredFundingStreamRecipient;

use zebra_chain::{
    block::Height,
    parameters::{
        subsidy::FundingStreamReceiver::{Deferred, Ecc, MajorGrants, ZcashFoundation},
        testnet::{self, ConfiguredActivationHeights, ConfiguredFundingStreams},
        Network, NetworkUpgrade,
    },
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
};

use crate::client::TransactionTemplate;
use crate::config::mining::{default_miner_address, MinerAddressType};

use super::MinerParams;

/// Tests that coinbase transactions can be generated.
///
/// This test needs to be run with the `--release` flag so that it runs for ~ 30 seconds instead of
/// ~ 90.
#[test]
#[ignore]
fn coinbase() -> anyhow::Result<()> {
    let regtest = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            overwinter: Some(1),
            sapling: Some(2),
            blossom: Some(3),
            heartwood: Some(4),
            canopy: Some(5),
            nu5: Some(6),
            nu6: Some(7),
            nu6_1: Some(8),
            nu7: Some(9),
            ..Default::default()
        })?
        .with_funding_streams(vec![
            ConfiguredFundingStreams {
                height_range: Some(Height(1)..Height(100)),
                recipients: Some(vec![
                    ConfiguredFundingStreamRecipient::new_for(Ecc),
                    ConfiguredFundingStreamRecipient::new_for(ZcashFoundation),
                    ConfiguredFundingStreamRecipient::new_for(MajorGrants),
                ]),
            },
            ConfiguredFundingStreams {
                height_range: Some(Height(1)..Height(100)),
                recipients: Some(vec![
                    ConfiguredFundingStreamRecipient::new_for(MajorGrants),
                    ConfiguredFundingStreamRecipient {
                        receiver: Deferred,
                        numerator: 12,
                        addresses: None,
                    },
                ]),
            },
        ])
        .to_network()?;

    for net in Network::iter().chain(iter::once(regtest)) {
        for nu in NetworkUpgrade::iter().filter(|nu| nu >= &NetworkUpgrade::Sapling) {
            if let Some(height) = nu.activation_height(&net) {
                for addr_type in MinerAddressType::iter() {
                    TransactionTemplate::new_coinbase(
                        &net,
                        height,
                        &MinerParams::from(
                            Address::decode(&net, default_miner_address(net.kind(), &addr_type))
                                .ok_or(anyhow!("hard-coded addr must be valid"))?,
                        ),
                        Amount::zero(),
                    )?
                    .data()
                    .as_ref()
                    // Deserialization contains checks for elementary consensus rules, which must
                    // pass.
                    .zcash_deserialize_into::<Transaction>()?;
                }
            }
        }
    }

    Ok(())
}

/// The Zebra marker is always prepended, and `extra_coinbase_data` can't exceed the limit.
#[test]
fn coinbase_tag_and_limit() {
    use zcash_address::ZcashAddress;

    use crate::config::mining::{
        Config, ExtraCoinbaseData, MAX_USER_COINBASE_DATA_LEN, ZEBRA_COINBASE_MARKER,
        ZEBRA_COINBASE_SEPARATOR,
    };

    // `ExtraCoinbaseData` accepts data up to the limit and rejects one byte over. Its `Deserialize`
    // impl delegates here, so an oversized `mining.extra_coinbase_data` makes the config fail to
    // load and the node refuse to start.
    assert!(ExtraCoinbaseData::try_from("x".repeat(MAX_USER_COINBASE_DATA_LEN)).is_ok());
    assert!(ExtraCoinbaseData::try_from("x".repeat(MAX_USER_COINBASE_DATA_LEN + 1)).is_err());

    let net = Network::Mainnet;
    let addr: ZcashAddress = default_miner_address(net.kind(), &MinerAddressType::Transparent)
        .parse()
        .expect("default miner address parses");

    let params = |extra: Option<ExtraCoinbaseData>| {
        MinerParams::new(
            &net,
            Config {
                miner_address: Some(addr.clone()),
                extra_coinbase_data: extra,
                ..Default::default()
            },
        )
    };

    // The marker is prepended whether or not `extra_coinbase_data` is set, so every block Zebra
    // builds is tagged. Without extra data, the coinbase data is exactly the marker.
    let untagged = params(None).expect("valid config");
    let untagged = untagged.data().as_ref().expect("marker is always present");
    assert_eq!(
        untagged.value().as_slice(),
        ZEBRA_COINBASE_MARKER.as_bytes()
    );

    // With extra data, the marker and separator precede it.
    let tag = ExtraCoinbaseData::try_from("/pool/".to_string()).expect("within the limit");
    let tagged = params(Some(tag)).expect("valid config");
    let tagged = tagged.data().as_ref().expect("marker is always present");
    assert_eq!(
        tagged.value().as_slice(),
        [ZEBRA_COINBASE_MARKER, ZEBRA_COINBASE_SEPARATOR, "/pool/"]
            .concat()
            .as_bytes()
    );
}

/// Tests that the coinbase cache reuses a previously built coinbase for the same height and fees,
/// so a short-polling miner doesn't re-run the shielded-coinbase proof on every request.
#[test]
fn coinbase_cache_reuses_built_coinbase() {
    use super::CoinbaseCache;

    let net = Network::Mainnet;
    let height = NetworkUpgrade::Nu5
        .activation_height(&net)
        .expect("Nu5 is active on Mainnet");
    let miner_params = MinerParams::from(
        Address::decode(
            &net,
            default_miner_address(net.kind(), &MinerAddressType::Sapling),
        )
        .expect("hard-coded Sapling address is valid"),
    );
    let fee = Amount::zero();

    let build = || {
        TransactionTemplate::new_coinbase(&net, height, &miner_params, fee)
            .expect("valid coinbase tx")
    };

    // A shielded coinbase carries a randomized proof, so two fresh builds differ. Identical bytes
    // therefore prove the cache returned a reused transaction rather than rebuilding it.
    let coinbase = build();
    assert_ne!(
        build(),
        coinbase,
        "fresh shielded coinbases differ (randomized proof)"
    );

    let cache = CoinbaseCache::default();
    assert!(cache.get(height, fee).is_none(), "an empty cache misses");

    cache.store(height, fee, coinbase.clone());
    assert_eq!(
        cache.get(height, fee),
        Some(coinbase.clone()),
        "a cache hit reuses the stored coinbase",
    );

    // A different height key misses, so the next request rebuilds.
    let next_height = height.next().expect("height is below Height::MAX");
    assert!(
        cache.get(next_height, fee).is_none(),
        "a different height misses"
    );
}

/// Verifies the fix for #10907: the multi-entry coinbase cache retains both the zero-fee fake
/// coinbase (used for ZIP-317 weight sizing) and the real-fee coinbase simultaneously, so
/// `getblocktemplate` doesn't rebuild shielded proofs on every short-poll.
#[test]
fn coinbase_cache_retains_both_fake_and_real_fee_entries() {
    use super::CoinbaseCache;

    let height = Height(1_000_000);
    let zero_fee = Amount::zero();
    let real_fee: Amount<zebra_chain::amount::NonNegative> =
        Amount::try_from(10_000).expect("valid amount");

    let cache = CoinbaseCache::default();

    // Simulate what getblocktemplate does: store a fake coinbase at zero fee (ZIP-317 sizing),
    // then store the real coinbase at the actual fee.
    let fake_coinbase = TransactionTemplate::new_coinbase(
        &Network::Mainnet,
        height,
        &MinerParams::from(
            Address::decode(
                &Network::Mainnet,
                default_miner_address(
                    zebra_chain::parameters::NetworkKind::Mainnet,
                    &MinerAddressType::Sapling,
                ),
            )
            .unwrap(),
        ),
        zero_fee,
    )
    .unwrap();

    let real_coinbase = TransactionTemplate::new_coinbase(
        &Network::Mainnet,
        height,
        &MinerParams::from(
            Address::decode(
                &Network::Mainnet,
                default_miner_address(
                    zebra_chain::parameters::NetworkKind::Mainnet,
                    &MinerAddressType::Sapling,
                ),
            )
            .unwrap(),
        ),
        real_fee,
    )
    .unwrap();

    cache.store(height, zero_fee, fake_coinbase.clone());
    cache.store(height, real_fee, real_coinbase.clone());

    // Both entries coexist — the zero-fee sizing coinbase survives the real-fee store.
    assert_eq!(
        cache.get(height, zero_fee),
        Some(fake_coinbase),
        "zero-fee fake coinbase should still be cached after storing real-fee coinbase"
    );
    assert_eq!(
        cache.get(height, real_fee),
        Some(real_coinbase),
        "real-fee coinbase should be cached"
    );

    // Height transition: storing at a new height evicts the stale entries.
    let next_height = Height(height.0 + 1);
    let next_coinbase = TransactionTemplate::new_coinbase(
        &Network::Mainnet,
        next_height,
        &MinerParams::from(
            Address::decode(
                &Network::Mainnet,
                default_miner_address(
                    zebra_chain::parameters::NetworkKind::Mainnet,
                    &MinerAddressType::Sapling,
                ),
            )
            .unwrap(),
        ),
        zero_fee,
    )
    .unwrap();

    cache.store(next_height, zero_fee, next_coinbase.clone());
    assert_eq!(
        cache.get(next_height, zero_fee),
        Some(next_coinbase),
        "new-height entry should be cached"
    );
    assert!(
        cache.get(height, zero_fee).is_none(),
        "old-height entry should be evicted"
    );
}

/// Verifies that fee churn beyond the cache cap (4 entries) evicts stale nonzero-fee entries
/// while preserving the zero-fee sizing coinbase. Without this, the cap would clear the
/// entire map — including the zero-fee entry — recreating the original #10907 churn.
#[test]
fn coinbase_cache_preserves_zero_fee_entry_at_capacity() {
    use super::CoinbaseCache;

    let height = Height(2_000_000);
    let zero_fee = Amount::zero();
    let cache = CoinbaseCache::default();

    let miner_params = MinerParams::from(
        Address::decode(
            &Network::Mainnet,
            default_miner_address(
                zebra_chain::parameters::NetworkKind::Mainnet,
                &MinerAddressType::Sapling,
            ),
        )
        .unwrap(),
    );

    let make_coinbase = |fee: Amount<zebra_chain::amount::NonNegative>| {
        TransactionTemplate::new_coinbase(&Network::Mainnet, height, &miner_params, fee).unwrap()
    };

    // Store the zero-fee sizing coinbase first.
    let fake_coinbase = make_coinbase(zero_fee);
    cache.store(height, zero_fee, fake_coinbase.clone());

    // Fill to capacity with distinct fee values (simulating mempool fee churn).
    for i in 1..=5u64 {
        let fee = Amount::try_from(i * 1_000).expect("valid amount");
        cache.store(height, fee, make_coinbase(fee));
    }

    // The zero-fee entry must survive eviction at capacity.
    assert_eq!(
        cache.get(height, zero_fee),
        Some(fake_coinbase.clone()),
        "zero-fee sizing coinbase must survive fee churn at capacity"
    );

    // Updating an existing key at capacity should not trigger eviction.
    let fee_1k: Amount<zebra_chain::amount::NonNegative> =
        Amount::try_from(1_000).expect("valid amount");
    let updated_coinbase = make_coinbase(fee_1k);
    cache.store(height, fee_1k, updated_coinbase.clone());
    assert_eq!(
        cache.get(height, fee_1k),
        Some(updated_coinbase),
        "updating an existing key should replace in place"
    );
    assert_eq!(
        cache.get(height, zero_fee),
        Some(fake_coinbase),
        "zero-fee entry must still be present after in-place update"
    );
}

/// From NU6.3 onward, a shielded coinbase paid to a Unified miner address with an Orchard
/// receiver routes newly minted value into the Ironwood pool, not the Orchard pool, and remains
/// recoverable with the consensus-required all-zero outgoing viewing key.
#[test]
fn coinbase_at_nu6_3_routes_shielded_output_to_ironwood() {
    let net = Network::new_default_testnet();
    let height = NetworkUpgrade::Nu6_3
        .activation_height(&net)
        .expect("Nu6.3 is scheduled on Testnet");
    let miner_params = MinerParams::from(
        Address::decode(
            &net,
            default_miner_address(net.kind(), &MinerAddressType::Unified),
        )
        .expect("hard-coded Unified address is valid"),
    );

    let template = TransactionTemplate::new_coinbase(&net, height, &miner_params, Amount::zero())
        .expect("valid coinbase tx");
    let coinbase: Transaction = template.data.as_ref().zcash_deserialize_into().unwrap();

    // The coinbase is a v6 transaction with Ironwood shielded data and no Orchard shielded data.
    // ZIP-229: from NU6.3, coinbase MUST have an empty Orchard component.
    assert_eq!(coinbase.version(), 6, "coinbase is v6 at NU6.3");
    assert!(
        coinbase.has_ironwood_shielded_data(),
        "coinbase creates an Ironwood output"
    );
    assert!(
        !coinbase.has_orchard_shielded_data(),
        "coinbase must not create Orchard components on NU6.3"
    );
    zebra_consensus::transaction::check::coinbase_outputs_are_decryptable(&coinbase, &net, height)
        .expect("Ironwood coinbase output is recoverable with the zero outgoing viewing key");
}

/// Testnet's standard difficulty expires strictly after an abbreviated timestamp range.
#[test]
fn only_current_work_is_served() {
    use super::{BlockTemplateResponse, CoinbaseCache};
    use crate::methods::{tests::utils::fake_history_tree, types::long_poll::LongPollInput};
    use zebra_chain::{
        block,
        serialization::{DateTime32, Duration32},
        work::difficulty::{CompactDifficulty, ExpandedDifficulty, ParameterDifficulty, U256},
    };
    use zebra_state::GetBlockTemplateChainInfo;

    let _init_guard = zebra_test::init();
    let max_time = DateTime32::from(1654008719);
    let after_max_time = max_time.saturating_add(Duration32::from_seconds(1));
    let tip_hash = block::Hash([0xab; 32]);
    let make_template = |network: &Network| {
        let tip_height = NetworkUpgrade::Nu5.activation_height(network).unwrap();
        let params = MinerParams::from(
            Address::decode(
                network,
                default_miner_address(network.kind(), &MinerAddressType::Transparent),
            )
            .unwrap(),
        );
        let chain_info = GetBlockTemplateChainInfo {
            expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
            tip_height,
            tip_hash,
            cur_time: DateTime32::from(1654008617),
            min_time: DateTime32::from(1654008606),
            max_time,
            chain_history_root: fake_history_tree(network).hash(),
        };
        BlockTemplateResponse::from_transactions(
            network,
            &CoinbaseCache::default(),
            &params,
            &chain_info,
            LongPollInput::new(tip_height, tip_hash, max_time, []).generate_id(),
            vec![],
            None,
        )
    };
    let network = Network::new_default_testnet();
    let current = make_template(&network);
    assert!(
        current.is_valid_for_tip(tip_hash, &network, max_time),
        "max_time is inclusive"
    );
    assert!(!current.is_valid_for_tip(block::Hash([0xff; 32]), &network, max_time));
    assert!(!current.is_valid_for_tip(tip_hash, &network, after_max_time));

    // The first applicable candidate is 299188, not the child of block 299188.
    // Its pre-Blossom spacing is 150 seconds, so standard difficulty lasts 15 minutes.
    let mut activation_template = current.clone();
    activation_template.max_time = activation_template
        .cur_time
        .checked_add(Duration32::from_seconds(6 * 150))
        .unwrap();
    let expired = activation_template
        .max_time
        .checked_add(Duration32::from_seconds(1))
        .unwrap();
    activation_template.height = 299_187;
    assert!(activation_template.is_valid_for_tip(tip_hash, &network, expired));
    activation_template.height = 299_188;
    assert!(activation_template.is_valid_for_tip(tip_hash, &network, activation_template.max_time,));
    assert!(!activation_template.is_valid_for_tip(tip_hash, &network, expired));

    let mut median_capped = current.clone();
    median_capped.min_time = max_time
        .saturating_sub(Duration32::from_minutes(90))
        .saturating_add(Duration32::from_seconds(1));
    assert!(median_capped.is_valid_for_tip(tip_hash, &network, after_max_time));

    let mut minimum_difficulty = current;
    minimum_difficulty.bits = network.target_difficulty_limit().to_compact();
    minimum_difficulty.target = network.target_difficulty_limit();
    assert!(minimum_difficulty.is_valid_for_tip(tip_hash, &network, after_max_time));

    let regtest = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );
    for network in [Network::Mainnet, regtest] {
        assert!(make_template(&network).is_valid_for_tip(tip_hash, &network, after_max_time));
    }
}

/// Serialized dependencies identify direct in-template parents once, preserving selection order.
#[test]
fn template_serializes_direct_transaction_dependencies() {
    use std::sync::Arc;

    use super::{BlockTemplateResponse, CoinbaseCache};
    use crate::methods::{tests::utils::fake_history_tree, types::long_poll::LongPollInput};
    use zebra_chain::{
        block,
        serialization::DateTime32,
        transaction::{self, LockTime, VerifiedUnminedTx},
        transparent::{Input, OutPoint, Output, Script},
        work::difficulty::ParameterDifficulty,
    };
    use zebra_state::GetBlockTemplateChainInfo;

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let make_tx = |outpoints: Vec<OutPoint>| {
        let tx = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            outpoints
                .into_iter()
                .map(|outpoint| Input::PrevOut {
                    outpoint,
                    unlock_script: Script::new(&[]),
                    sequence: u32::MAX,
                })
                .collect(),
            vec![Output::new(Amount::zero(), Script::new(&[0x51])); 3],
            LockTime::unlocked(),
            Height(0),
        );
        VerifiedUnminedTx::new(
            Arc::new(tx).into(),
            Amount::try_from(100_000).unwrap(),
            0,
            0,
            Arc::new(Vec::new()),
        )
        .unwrap()
    };
    let confirmed = transaction::Hash([0x42; 32]);
    let parent = make_tx(vec![OutPoint::from_usize(confirmed, 0)]);
    let parent_id = parent.transaction.id.mined_id();
    let child = make_tx(vec![
        OutPoint::from_usize(parent_id, 0),
        OutPoint::from_usize(confirmed, 1),
        OutPoint::from_usize(parent_id, 1),
    ]);
    let child_id = child.transaction.id.mined_id();
    let grandchild = make_tx(vec![OutPoint::from_usize(child_id, 0)]);
    let two_parents = make_tx(vec![
        OutPoint::from_usize(child_id, 1),
        OutPoint::from_usize(parent_id, 2),
        OutPoint::from_usize(child_id, 2),
    ]);
    let txs = vec![parent, child, grandchild, two_parents];
    let expected_hashes: Vec<_> = txs
        .iter()
        .map(|tx| tx.transaction.id.mined_id().to_string())
        .collect();
    let tip_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
    let tip_hash = block::Hash([0xab; 32]);
    let time = DateTime32::from(1_654_008_617);
    let chain_info = GetBlockTemplateChainInfo {
        tip_height,
        tip_hash,
        expected_difficulty: network.target_difficulty_limit().to_compact(),
        cur_time: time,
        min_time: time,
        max_time: time,
        chain_history_root: fake_history_tree(&network).hash(),
    };
    let params = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .unwrap(),
    );
    let template = BlockTemplateResponse::from_transactions(
        &network,
        &CoinbaseCache::default(),
        &params,
        &chain_info,
        LongPollInput::new(tip_height, tip_hash, time, []).generate_id(),
        txs,
        None,
    );
    let response = serde_json::to_value(template).unwrap();
    assert_eq!(response["transactions"].as_array().unwrap().len(), 4);
    for ((tx, expected_hash), dependencies) in response["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .zip(expected_hashes)
        .zip([vec![], vec![1], vec![2], vec![1, 2]])
    {
        assert_eq!(tx["hash"], expected_hash);
        assert_eq!(tx["depends"], serde_json::json!(dependencies));
    }
}
