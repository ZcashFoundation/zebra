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
                        #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
                        None,
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

#[cfg(all(feature = "tx_v6", zcash_unstable = "zip235"))]
#[test]
#[ignore]
fn coinbase_with_fees_balances_zip235() -> anyhow::Result<()> {
    use zebra_chain::amount::NonNegative;
    use zebra_chain::parameters::subsidy::block_subsidy;

    let net = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu6_1: Some(1),
            nu7: Some(2),
            ..Default::default()
        })?
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(10)),
            recipients: None,
        }])
        .to_network()?;

    let height = NetworkUpgrade::Nu7
        .activation_height(&net)
        .expect("Nu7 activation height is configured");

    let txs_fee: Amount<NonNegative> = 100_000.try_into()?;

    let coinbase = TransactionTemplate::new_coinbase(
        &net,
        height,
        &MinerParams::from(
            Address::decode(
                &net,
                default_miner_address(net.kind(), &MinerAddressType::Transparent),
            )
            .ok_or(anyhow!("hard-coded addr must be valid"))?,
        ),
        txs_fee,
        None,
    )?;

    let tx = coinbase
        .data()
        .as_ref()
        .zcash_deserialize_into::<Transaction>()?;

    let transparent: i64 = tx.outputs().iter().map(|o| o.value().zatoshis()).sum();
    let sapling = tx.sapling_value_balance().sapling_amount().zatoshis();
    let orchard = tx.orchard_value_balance().orchard_amount().zatoshis();
    let zip233 = tx.zip233_amount().zatoshis();

    let total_output = transparent - sapling - orchard + zip233;
    let total_input = block_subsidy(height, &net)?.zatoshis() + txs_fee.zatoshis();

    assert_eq!(zip233, txs_fee.zatoshis() * 6 / 10);
    assert!(zip233 > 0);

    assert_eq!(
        total_output, total_input,
        "coinbase total output must equal subsidy + fees without double-counting zip233"
    );

    Ok(())
}
