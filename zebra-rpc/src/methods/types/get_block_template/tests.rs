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

/// PoW test networks can restart a stalled chain, but must still be synchronized.
#[test]
fn mining_requires_sync_but_only_mainnet_requires_a_recent_tip() {
    use zebra_chain::{chain_sync_status::MockSyncStatus, chain_tip::mock::MockChainTip};

    let (tip, sender) = MockChainTip::new();
    let mut sync = MockSyncStatus::default();
    sender.send_best_tip_height(Height(1_000_000));

    let custom_testnet = testnet::Parameters::build()
        .with_network_name("MiningTestnet")
        .expect("custom network name is valid")
        .to_network()
        .expect("custom Testnet parameters are valid");
    for network in [
        Network::Mainnet,
        Network::new_default_testnet(),
        custom_testnet,
    ] {
        assert!(!network.disable_pow());
        for (age, close, valid) in [
            (chrono::Duration::minutes(1), true, true),
            (chrono::Duration::hours(3), true, !network.is_mainnet()),
            (chrono::Duration::days(3650), true, !network.is_mainnet()),
            (chrono::Duration::minutes(1), false, false),
            (chrono::Duration::hours(3), false, false),
        ] {
            sender.send_best_tip_block_time(chrono::Utc::now() - age);
            sync.set_is_close_to_tip(close);
            assert_eq!(
                super::check_synced_to_tip(&network, tip.clone(), sync.clone()).is_ok(),
                valid,
                "{network:?}, age {age}, sync status {close}",
            );
        }
    }

    let regtest = Network::new_regtest(Default::default());
    let pow_disabled = testnet::Parameters::build()
        .with_disable_pow(true)
        .to_network()
        .expect("PoW-disabled Testnet parameters are valid");
    for network in [regtest, pow_disabled] {
        let (empty_tip, _sender) = MockChainTip::new();
        assert!(super::check_synced_to_tip(&network, empty_tip, sync.clone()).is_ok());
    }
}

/// Dependency metadata uses the final template order and lists each parent only once.
#[test]
fn template_reports_selected_transaction_dependencies() {
    use zebra_chain::{
        block,
        serialization::DateTime32,
        transaction::{self, LockTime, VerifiedUnminedTx},
        transparent::{Input, OutPoint, Output, Script},
        work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
    };
    use zebra_node_services::mempool::TransactionDependencies;
    use zebra_state::GetBlockTemplateChainInfo;

    use super::{zip317::select_mempool_transactions, BlockTemplateResponse, CoinbaseCache};
    use crate::methods::types::long_poll::LongPollInput;

    let net = Network::new_regtest(testnet::RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        },
        ..Default::default()
    });
    let transaction = |outpoints: Vec<OutPoint>, output_value: u64| {
        let tx = Transaction::test_v5(
            NetworkUpgrade::Nu7,
            outpoints
                .into_iter()
                .map(|outpoint| Input::PrevOut {
                    outpoint,
                    unlock_script: Script::new(&[]),
                    sequence: u32::MAX,
                })
                .collect(),
            vec![
                Output {
                    value: output_value.try_into().unwrap(),
                    lock_script: Script::new(&[0x51]),
                };
                2
            ],
            LockTime::unlocked(),
            Height(100),
        );
        VerifiedUnminedTx::new(
            std::sync::Arc::new(tx).into(),
            10_000u64.try_into().unwrap(),
            0,
            0,
            Default::default(),
        )
        .unwrap()
    };
    let parent = transaction(
        vec![OutPoint::from_usize(transaction::Hash([1; 32]), 0)],
        50_000,
    );
    let unrelated = transaction(
        vec![OutPoint::from_usize(transaction::Hash([2; 32]), 0)],
        50_000,
    );
    let parent_hash = parent.transaction.id.mined_id();
    let parent_outputs = vec![
        OutPoint::from_usize(parent_hash, 0),
        OutPoint::from_usize(parent_hash, 1),
    ];
    let child = transaction(parent_outputs.clone(), 45_000);
    let child_hash = child.transaction.id.mined_id();
    let mut dependencies = TransactionDependencies::default();
    dependencies.add(child_hash, parent_outputs);
    let height = Height(11);
    let miner_params = MinerParams::from(
        Address::decode(
            &net,
            default_miner_address(net.kind(), &MinerAddressType::Transparent),
        )
        .unwrap(),
    );
    let selected = select_mempool_transactions(
        &net,
        height,
        &miner_params,
        vec![child, unrelated, parent],
        dependencies,
        None,
        Some(Amount::zero()),
    );
    let chain_info = GetBlockTemplateChainInfo {
        expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
        chain_value_pools: Default::default(),
        tip_height: Height(10),
        tip_hash: block::Hash([3; 32]),
        cur_time: DateTime32::from(1_700_000_000),
        min_time: DateTime32::from(1_699_999_999),
        max_time: DateTime32::from(1_700_000_001),
        chain_history_root: Some([4; 32].into()),
    };
    let long_poll_id = LongPollInput::new(
        chain_info.tip_height,
        chain_info.tip_hash,
        chain_info.max_time,
        iter::empty(),
    )
    .generate_id();
    let template = BlockTemplateResponse::from_transactions(
        &net,
        &CoinbaseCache::default(),
        &miner_params,
        &chain_info,
        long_poll_id,
        selected,
        None,
    );
    let [parent_index, child_index] = [parent_hash, child_hash].map(|hash| {
        template
            .transactions
            .iter()
            .position(|tx| tx.hash == hash)
            .unwrap()
    });
    let json = serde_json::to_value(&template).unwrap();
    assert!(parent_index < child_index);
    assert_eq!(
        json["transactions"][parent_index]["depends"],
        serde_json::json!([])
    );
    assert_eq!(
        json["transactions"][child_index]["depends"],
        serde_json::json!([parent_index + 1])
    );
}

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
                height_range: Some(Height(1)..Height(7)),
                recipients: Some(vec![
                    ConfiguredFundingStreamRecipient::new_for(Ecc),
                    ConfiguredFundingStreamRecipient::new_for(ZcashFoundation),
                    ConfiguredFundingStreamRecipient::new_for(MajorGrants),
                ]),
            },
            ConfiguredFundingStreams {
                // NU6 replaces ECC and ZF payments with the deferred stream.
                height_range: Some(Height(7)..Height(100)),
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
                    TransactionTemplate::new_coinbase_with_parent_pools(
                        &net,
                        height,
                        &MinerParams::from(
                            Address::decode(&net, default_miner_address(net.kind(), &addr_type))
                                .ok_or(anyhow!("hard-coded addr must be valid"))?,
                        ),
                        Amount::zero(),
                        Some(Amount::zero()),
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
    assert!(
        cache.get(height, fee, None).is_none(),
        "an empty cache misses"
    );

    cache.store(height, fee, None, coinbase.clone());
    assert_eq!(
        cache.get(height, fee, None),
        Some(coinbase.clone()),
        "a cache hit reuses the stored coinbase",
    );

    // A different height key misses, so the next request rebuilds.
    let next_height = height.next().expect("height is below Height::MAX");
    assert!(
        cache.get(next_height, fee, None).is_none(),
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

    cache.store(height, zero_fee, None, fake_coinbase.clone());
    cache.store(height, real_fee, None, real_coinbase.clone());

    // Both entries coexist — the zero-fee sizing coinbase survives the real-fee store.
    assert_eq!(
        cache.get(height, zero_fee, None),
        Some(fake_coinbase),
        "zero-fee fake coinbase should still be cached after storing real-fee coinbase"
    );
    assert_eq!(
        cache.get(height, real_fee, None),
        Some(real_coinbase),
        "real-fee coinbase should be cached"
    );

    // Height transition: a request at the new height evicts stale entries before building.
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

    cache.select(next_height, None);
    assert!(cache.get(next_height, zero_fee, None).is_none());
    cache.store(next_height, zero_fee, None, next_coinbase.clone());
    assert_eq!(
        cache.get(next_height, zero_fee, None),
        Some(next_coinbase),
        "new-height entry should be cached"
    );
    assert!(
        cache.get(height, zero_fee, None).is_none(),
        "old-height entry should be evicted"
    );
}

/// A proof started before a same-height reorg must not evict the new parent's coinbases.
#[test]
fn coinbase_cache_discards_late_old_parent_builds() {
    use super::CoinbaseCache;

    let network = Network::new_regtest(testnet::RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        },
        nsm_reissuance_height: Some(Height(1)),
        ..Default::default()
    });
    let height = Height(10);
    let parent = Some(Amount::try_from(1_000_000_000u64).unwrap());
    let other_parent = Some(Amount::try_from(2_000_000_000u64).unwrap());
    let fee = Amount::zero();
    let miner_params = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .unwrap(),
    );
    let build = |parent| {
        TransactionTemplate::new_coinbase_with_parent_pools(
            &network,
            height,
            &miner_params,
            fee,
            parent,
        )
        .unwrap()
    };
    let cache = CoinbaseCache::default();
    cache.select(height, parent);
    let old_coinbase = build(parent);
    cache.select(height, other_parent);
    let current_coinbase = build(other_parent);
    assert_ne!(old_coinbase, current_coinbase);
    cache.store(height, fee, other_parent, current_coinbase.clone());
    cache.store(height, fee, parent, old_coinbase);
    assert!(cache.get(height, fee, parent).is_none());
    assert_eq!(
        cache.get(height, fee, other_parent),
        Some(current_coinbase),
        "finishing an old proof must not evict the active parent's reusable coinbase",
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
    cache.store(height, zero_fee, None, fake_coinbase.clone());

    // Fill to capacity with distinct fee values (simulating mempool fee churn).
    for i in 1..=5u64 {
        let fee = Amount::try_from(i * 1_000).expect("valid amount");
        cache.store(height, fee, None, make_coinbase(fee));
        assert!(
            cache
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .transactions
                .len()
                <= 4
        );
    }

    // The zero-fee entry must survive eviction at capacity.
    assert_eq!(
        cache.get(height, zero_fee, None),
        Some(fake_coinbase.clone()),
        "zero-fee sizing coinbase must survive fee churn at capacity"
    );

    // Updating an existing key at capacity should not trigger eviction.
    let fee_5k: Amount<zebra_chain::amount::NonNegative> =
        Amount::try_from(5_000).expect("valid amount");
    let updated_coinbase = make_coinbase(fee_5k);
    cache.store(height, fee_5k, None, updated_coinbase.clone());
    assert_eq!(
        cache.get(height, fee_5k, None),
        Some(updated_coinbase),
        "updating an existing key should replace in place"
    );
    assert_eq!(
        cache.get(height, zero_fee, None),
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

/// Coinbase fee metadata reports the value collected, with ZIP 235 rounded once per block.
#[test]
fn coinbase_fee_metadata_matches_collected_fees() {
    use zcash_transparent::address::TransparentAddress;
    use zebra_chain::parameters::{subsidy::scheduled_block_subsidy, testnet::RegtestParameters};

    let _init_guard = zebra_test::init();
    let net = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu6_3: Some(1),
            nu7: Some(10),
            ..Default::default()
        },
        nsm_reissuance_height: Some(Height(12)),
        ..Default::default()
    });
    let miner = MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));
    for height in [Height(9), Height(10)] {
        let subsidy = scheduled_block_subsidy(height, &net).unwrap().zatoshis();
        // 20,002 can be two 10,001-zatoshi fees: per-transaction rounding would yield 8,002.
        for (gross, after_nu7) in [(0, 0), (1, 1), (2, 1), (10_000, 4_000), (20_002, 8_001)] {
            let collected = if height == Height(9) {
                gross
            } else {
                after_nu7
            };
            let template =
                TransactionTemplate::new_coinbase(&net, height, &miner, gross.try_into().unwrap())
                    .unwrap();
            let coinbase: Transaction = template.data.as_ref().zcash_deserialize_into().unwrap();
            let paid = coinbase
                .outputs()
                .iter()
                .map(|output| output.value.zatoshis())
                .sum::<i64>();
            assert_eq!(paid - subsidy, collected);
            assert_eq!(template.fee.zatoshis(), -collected);
        }
    }
}

/// From the NSM reissuance height, the coinbase pays the block subsidy computed
/// from the parent block's chain value pools, and can't be built without them.
#[test]
fn coinbase_pays_nsm_subsidy() {
    use zcash_transparent::address::TransparentAddress;
    use zebra_chain::{
        amount::NonNegative,
        parameters::{
            subsidy::{additional_block_subsidy, nsm_fee_contribution, scheduled_block_subsidy},
            testnet::RegtestParameters,
        },
    };
    use zebra_consensus::error::TransactionError;

    use super::CoinbaseCache;

    let _init_guard = zebra_test::init();

    let initial_seed = Amount::<NonNegative>::try_from(2_000_000_000u64).unwrap();
    let net = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            nu7: Some(10),
            ..Default::default()
        },
        initial_nsm_value_balance: Some(initial_seed),
        nsm_reissuance_height: Some(Height(10)),
        ..Default::default()
    });
    let miner_params =
        MinerParams::from(Address::from(TransparentAddress::PublicKeyHash([0x7e; 20])));
    let fee = Amount::<NonNegative>::try_from(1_000).unwrap();

    let miner_output = |height, parent_nsm_value_balance| {
        let template = TransactionTemplate::new_coinbase_with_parent_pools(
            &net,
            height,
            &miner_params,
            fee,
            parent_nsm_value_balance,
        )
        .expect("valid coinbase tx");
        let coinbase: Transaction = template.data.as_ref().zcash_deserialize_into().unwrap();
        assert_eq!(coinbase.outputs().len(), 1);
        coinbase.outputs()[0].value()
    };

    let height = Height(12);
    let nsm_value_balance = Amount::<NonNegative>::try_from(1_000_000_000_i64).unwrap();

    // Before activation, the parent's NSM value balance doesn't affect the coinbase.
    let before = Height(9);
    let scheduled = (scheduled_block_subsidy(before, &net).unwrap() + fee).unwrap();
    assert_eq!(miner_output(before, None), scheduled);
    assert_eq!(miner_output(before, Some(nsm_value_balance)), scheduled);
    // The explicit seed is available at activation even though the stored parent balance is zero.
    let activation = Height(10);
    let activation_subsidy = (scheduled_block_subsidy(activation, &net).unwrap()
        + additional_block_subsidy(activation, &net, initial_seed))
    .unwrap();
    assert_eq!(
        miner_output(activation, Some(Amount::zero())),
        (activation_subsidy + Amount::try_from(400).unwrap()).unwrap(),
    );

    let subsidy = (scheduled_block_subsidy(height, &net).unwrap()
        + additional_block_subsidy(height, &net, nsm_value_balance))
    .unwrap();
    // From NU7 activation the miner gets only the fees ZIP 235 leaves in circulation.
    let miner_fees = (fee - nsm_fee_contribution(height, &net, fee)).unwrap();
    assert_eq!(miner_fees, Amount::<NonNegative>::try_from(400).unwrap());
    assert_eq!(
        miner_output(height, Some(nsm_value_balance)),
        (subsidy + miner_fees).unwrap()
    );

    assert!(matches!(
        TransactionTemplate::new_coinbase(&net, height, &miner_params, fee),
        Err(TransactionError::Subsidy(_))
    ));

    // The coinbase cache doesn't reuse a coinbase built after a different parent.
    let coinbase = TransactionTemplate::new_coinbase_with_parent_pools(
        &net,
        height,
        &miner_params,
        fee,
        Some(nsm_value_balance),
    )
    .unwrap();
    let cache = CoinbaseCache::default();
    cache.store(height, fee, Some(nsm_value_balance), coinbase.clone());
    assert_eq!(
        cache.get(height, fee, Some(nsm_value_balance)),
        Some(coinbase)
    );
    assert!(cache.get(height, fee, Some(Amount::zero())).is_none());
}

/// Constructs time-dependent work without involving the template-owning actor.
fn template_with_max_time(
    network: &Network,
    max_time: zebra_chain::serialization::DateTime32,
) -> super::BlockTemplateResponse {
    use super::{BlockTemplateResponse, CoinbaseCache};
    use crate::methods::{tests::utils::fake_history_tree, types::long_poll::LongPollInput};
    use zebra_chain::{
        block,
        serialization::DateTime32,
        work::difficulty::{CompactDifficulty, ExpandedDifficulty, U256},
    };
    use zebra_state::GetBlockTemplateChainInfo;

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
        tip_hash: block::Hash([0xab; 32]),
        cur_time: DateTime32::from(1654008617),
        min_time: DateTime32::from(1654008606),
        max_time,
        chain_history_root: fake_history_tree(network).hash(),
        chain_value_pools: Default::default(),
    };
    BlockTemplateResponse::from_transactions(
        network,
        &CoinbaseCache::default(),
        &params,
        &chain_info,
        LongPollInput::new(tip_height, chain_info.tip_hash, max_time, []).generate_id(),
        vec![],
        None,
    )
}

/// Testnet's standard difficulty expires strictly after an abbreviated timestamp range.
#[test]
fn only_current_work_is_served() {
    use zebra_chain::{
        block,
        serialization::{DateTime32, Duration32},
        work::difficulty::ParameterDifficulty,
    };

    let _init_guard = zebra_test::init();
    let max_time = DateTime32::from(1654008719);
    let after_max_time = max_time.saturating_add(Duration32::from_seconds(1));
    let tip_hash = block::Hash([0xab; 32]);
    let make_template = |network: &Network| template_with_max_time(network, max_time);
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

/// Before the MTP maximum activates, even a wide range can end at a difficulty boundary.
#[test]
fn wide_testnet_ranges_expire_before_mtp_maximum_activation() {
    use zebra_chain::serialization::{DateTime32, Duration32};

    let _init_guard = zebra_test::init();
    let configured = testnet::Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(299_188),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid");
    let previous_time = DateTime32::from(1654008600);

    for (network, initial_gap) in [(Network::new_default_testnet(), 900), (configured, 450)] {
        for (height, gap, usable) in [
            (299_188, initial_gap, false),
            (653_605, 450, false),
            (653_606, 450, true),
        ] {
            let max_time = previous_time.saturating_add(Duration32::from_seconds(gap));
            let expired = max_time.saturating_add(Duration32::from_seconds(1));
            let mut current = template_with_max_time(&network, max_time);
            current.height = height;
            current.min_time = max_time
                .saturating_sub(Duration32::from_minutes(90))
                .saturating_add(Duration32::from_seconds(1));
            let tip_hash = current.previous_block_hash;
            assert!(
                current.is_valid_for_tip(tip_hash, &network, max_time),
                "{network:?}, candidate height {height}: the difficulty boundary is inclusive",
            );
            assert_eq!(
                current.is_valid_for_tip(tip_hash, &network, expired),
                usable,
                "{network:?}, candidate height {height}",
            );
        }
    }
}

/// A clock rollback must invalidate any template advertising timestamps beyond the new bound.
#[test]
fn clock_rollback_invalidates_cached_timestamp_range() {
    use zebra_chain::serialization::{DateTime32, Duration32};

    let _init_guard = zebra_test::init();
    let regtest = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(100),
            ..Default::default()
        }
        .into(),
    );

    for network in [Network::Mainnet, Network::new_default_testnet(), regtest] {
        let current = template_with_max_time(&network, DateTime32::from(1654008719));
        let tip_hash = current.previous_block_hash;
        let boundary = current.max_time.saturating_sub(Duration32::from_hours(2));
        let rollback = boundary.saturating_sub(Duration32::from_seconds(1));
        // Checking only cur_time would miss the invalid advertised maximum.
        assert!(current.cur_time <= rollback.saturating_add(Duration32::from_hours(2)));
        assert!(current.is_valid_for_tip(tip_hash, &network, boundary));
        assert!(
            !current.is_valid_for_tip(tip_hash, &network, rollback),
            "every advertised timestamp must remain inside the local-clock bound on {network:?}",
        );
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
        chain_value_pools: Default::default(),
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
