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
        TransactionTemplate::new_coinbase(
            &net,
            height,
            &miner_params,
            fee,
            #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
            None,
        )
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

    // A different height key or a cleared cache misses, so the next request rebuilds.
    let next_height = height.next().expect("height is below Height::MAX");
    assert!(
        cache.get(next_height, fee).is_none(),
        "a different height misses"
    );
    cache.clear();
    assert!(cache.get(height, fee).is_none(), "a cleared cache misses");
}
