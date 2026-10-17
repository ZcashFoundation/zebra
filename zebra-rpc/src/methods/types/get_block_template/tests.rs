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
    let template = BlockTemplateResponse::new_internal(
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

/// Differential sizing against real proofs, including funding streams and lockbox activation.
/// Run in release mode because shielded coinbases are expensive to prove.
#[test]
#[ignore]
fn coinbase_resources_match_built_transactions() -> anyhow::Result<()> {
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
            nu6_2: Some(9),
            nu6_3: Some(10),
            nu7: Some(11),
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
                    let mut miner = MinerParams::from(
                        Address::decode(&net, default_miner_address(net.kind(), &addr_type))
                            .ok_or(anyhow!("hard-coded addr must be valid"))?,
                    );
                    for tag_len in [None, Some(75), Some(76), Some(92)] {
                        miner.data =
                            tag_len.and_then(|len| zcash_script::pv::push_value(&vec![b'x'; len]));
                        assert_coinbase_resources(&net, height, &miner)?;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Compares the analytic reservation with an independently serialized, fee-bearing coinbase.
fn assert_coinbase_resources(
    net: &Network,
    height: Height,
    miner: &MinerParams,
) -> anyhow::Result<()> {
    use zebra_chain::serialization::ZcashSerialize;
    use zebra_consensus::ShieldedActionCounts;
    use zebra_script::Sigops;

    let parent_balance = Some(1_000_000_000u64.try_into()?);
    let resources =
        TransactionTemplate::coinbase_resource_usage(net, height, miner, parent_balance)?;
    let template = TransactionTemplate::new_coinbase_with_parent_pools(
        net,
        height,
        miner,
        10_000u64.try_into()?,
        parent_balance,
    )?;
    let transaction: Transaction = template.data().as_ref().zcash_deserialize_into()?;
    assert_eq!(
        resources.serialized_size,
        transaction.zcash_serialized_size()
    );
    assert_eq!(resources.sigops, transaction.sigops()?);
    assert_eq!(
        resources.shielded_actions,
        ShieldedActionCounts::from_transaction(&transaction)
    );
    Ok(())
}

#[test]
fn transparent_coinbase_resources_cover_encoding_boundaries() -> anyhow::Result<()> {
    use zcash_script::opcode::Evaluable;
    use zcash_transparent::address::TransparentAddress;

    // No funding outputs, except at NU6.1, where the total crosses CompactSize's 253 boundary.
    for lockbox_outputs in [251, 252, 253] {
        let net = testnet::Parameters::build()
            .with_slow_start_interval(Height::MIN)
            .with_activation_heights(ConfiguredActivationHeights {
                sapling: Some(1),
                nu5: Some(2),
                nu6_1: Some(3),
                nu6_3: Some(4),
                ..Default::default()
            })?
            .clear_funding_streams()
            .with_lockbox_disbursements(vec![
                testnet::ConfiguredLockboxDisbursement {
                    address: default_miner_address(
                        zebra_chain::parameters::NetworkKind::Testnet,
                        &MinerAddressType::Transparent,
                    )
                    .to_string(),
                    amount: Amount::zero(),
                };
                lockbox_outputs
            ])
            .to_network()?;
        for address in [
            TransparentAddress::PublicKeyHash([7; 20]),
            TransparentAddress::ScriptHash([7; 20]),
        ] {
            let mut miner = MinerParams::from(Address::from(address));
            for height in [
                1, 2, 3, 4, 16, 17, 127, 128, 32_767, 32_768, 8_388_607, 8_388_608,
            ] {
                let max_tag = zcash_transparent::coinbase::MAX_COINBASE_SCRIPT_LEN
                    - zcash_script::pattern::push_num(i64::from(height)).byte_len()
                    - 2;
                for tag_len in [None, Some(0), Some(75), Some(76), Some(92), Some(max_tag)] {
                    miner.data =
                        tag_len.and_then(|len| zcash_script::pv::push_value(&vec![b'x'; len]));
                    assert_coinbase_resources(&net, Height(height), &miner)?;
                }
            }
            // Before NU5 an Orchard-only shielded receiver falls back to transparent.
            let Address::Unified(unified) = Address::decode(
                &net,
                default_miner_address(net.kind(), &MinerAddressType::Unified),
            )
            .ok_or(anyhow!("hard-coded unified address must be valid"))?
            else {
                panic!("the unified address fixture contains a unified address");
            };
            let unified = zcash_keys::address::UnifiedAddress::from_receivers(
                unified.orchard().copied(),
                None,
                Some(address),
                None,
                None,
            )
            .expect("the Orchard receiver makes the unified address valid");
            assert_coinbase_resources(
                &net,
                Height(1),
                &MinerParams::from(Address::Unified(Box::new(unified))),
            )?;
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

/// Coinbase-only and fee-bearing templates must coexist without rebuilding their proofs.
#[test]
fn coinbase_cache_retains_empty_and_fee_bearing_templates() {
    use super::CoinbaseCache;

    let height = Height(1_000_000);
    let zero_fee = Amount::zero();
    let real_fee: Amount<zebra_chain::amount::NonNegative> =
        Amount::try_from(10_000).expect("valid amount");

    let cache = CoinbaseCache::default();

    // Publish a coinbase-only template, followed by one including mempool transactions.
    let empty_coinbase = TransactionTemplate::new_coinbase(
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

    cache.store(height, zero_fee, None, empty_coinbase.clone());
    cache.store(height, real_fee, None, real_coinbase.clone());

    // Publishing fee-bearing work does not evict the coinbase-only template.
    assert_eq!(
        cache.get(height, zero_fee, None),
        Some(empty_coinbase),
        "coinbase-only work should remain cached after storing a fee-bearing coinbase"
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

/// Fee churn beyond the cache cap retains the proof needed for coinbase-only work.
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

    // Store the coinbase-only template's proof first.
    let empty_coinbase = make_coinbase(zero_fee);
    cache.store(height, zero_fee, None, empty_coinbase.clone());

    // Fill to capacity with distinct fee values (simulating mempool fee churn).
    for i in 1..=5u64 {
        let fee = Amount::try_from(i * 1_000).expect("valid amount");
        cache.store(height, fee, None, make_coinbase(fee));
    }

    // The zero-fee entry must survive eviction at capacity.
    assert_eq!(
        cache.get(height, zero_fee, None),
        Some(empty_coinbase.clone()),
        "coinbase-only work must survive fee churn at capacity"
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
        Some(empty_coinbase),
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
