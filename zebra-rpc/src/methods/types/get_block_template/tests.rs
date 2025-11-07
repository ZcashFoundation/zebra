//! Tests for types and functions for the `getblocktemplate` RPC.

use anyhow::anyhow;
use std::iter;

use strum::IntoEnumIterator;
use zcash_keys::address::Address;

use zebra_chain::parameters::subsidy::{
    FUNDING_STREAM_ECC_ADDRESSES_TESTNET, FUNDING_STREAM_MG_ADDRESSES_TESTNET,
    FUNDING_STREAM_ZF_ADDRESSES_TESTNET,
};
use zebra_chain::parameters::testnet::ConfiguredFundingStreamRecipient;
use zebra_chain::{
    block::Height,
    parameters::{
        subsidy::FundingStreamReceiver,
        testnet::{self, ConfiguredActivationHeights, ConfiguredFundingStreams},
        Network, NetworkUpgrade,
    },
    serialization::ZcashDeserializeInto,
    transaction::Transaction,
};

use crate::client::TransactionTemplate;
use crate::config::mining::{MinerAddressType, MINER_ADDRESS};

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
        })
        .with_funding_streams(vec![
            ConfiguredFundingStreams {
                height_range: Some(Height(1)..Height(100)),
                recipients: Some(vec![
                    ConfiguredFundingStreamRecipient {
                        receiver: FundingStreamReceiver::ZcashFoundation,
                        numerator: 5,
                        addresses: Some(
                            FUNDING_STREAM_ZF_ADDRESSES_TESTNET
                                .map(ToString::to_string)
                                .to_vec(),
                        ),
                    },
                    ConfiguredFundingStreamRecipient {
                        receiver: FundingStreamReceiver::Ecc,
                        numerator: 7,
                        addresses: Some(
                            FUNDING_STREAM_ECC_ADDRESSES_TESTNET
                                .map(ToString::to_string)
                                .to_vec(),
                        ),
                    },
                    ConfiguredFundingStreamRecipient {
                        receiver: FundingStreamReceiver::MajorGrants,
                        numerator: 8,
                        addresses: Some(
                            FUNDING_STREAM_MG_ADDRESSES_TESTNET
                                .map(ToString::to_string)
                                .to_vec(),
                        ),
                    },
                ]),
            },
            ConfiguredFundingStreams {
                height_range: Some(Height(1)..Height(100)),
                recipients: Some(vec![
                    ConfiguredFundingStreamRecipient {
                        receiver: FundingStreamReceiver::MajorGrants,
                        numerator: 8,
                        addresses: Some(
                            FUNDING_STREAM_MG_ADDRESSES_TESTNET
                                .map(ToString::to_string)
                                .to_vec(),
                        ),
                    },
                    ConfiguredFundingStreamRecipient {
                        receiver: FundingStreamReceiver::Deferred,
                        numerator: 12,
                        addresses: None,
                    },
                ]),
            },
        ])
        .to_network();

    for net in Network::iter().chain(iter::once(regtest)) {
        for nu in NetworkUpgrade::iter().filter(|nu| nu >= &NetworkUpgrade::Sapling) {
            if let Some(height) = nu.activation_height(&net) {
                for addr_type in MinerAddressType::iter() {
                    TransactionTemplate::new_coinbase(
                        &net,
                        height,
                        &MinerParams::from(
                            Address::decode(&net, MINER_ADDRESS[&net.kind()][&addr_type])
                                .ok_or(anyhow!("hard-coded addr must be valid"))?,
                        ),
                        &[],
                        #[cfg(feature = "tx_v6")]
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
