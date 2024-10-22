//! Tests for funding streams.

use color_eyre::Report;

use crate::{
    block::subsidy::general::block_subsidy_pre_nsm,
    parameters::{
        subsidy::FundingStreamReceiver,
        testnet::{
            self, ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
            ConfiguredFundingStreams,
        },
        NetworkKind,
    },
};

use super::*;

/// Check mainnet funding stream values are correct for the entire period.
#[test]
fn test_funding_stream_values() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = &Network::Mainnet;

    // funding streams not active
    let canopy_height_minus1 = (Canopy.activation_height(network).unwrap() - 1).unwrap();

    assert!(funding_stream_values(
        canopy_height_minus1,
        network,
        block_subsidy_pre_nsm(canopy_height_minus1, network)?
    )?
    .is_empty());

    // funding stream is active
    let canopy_height = Canopy.activation_height(network).unwrap();
    let canopy_height_plus1 = (Canopy.activation_height(network).unwrap() + 1).unwrap();
    let canopy_height_plus2 = (Canopy.activation_height(network).unwrap() + 2).unwrap();

    let mut hash_map = HashMap::new();
    hash_map.insert(FundingStreamReceiver::Ecc, Amount::try_from(21_875_000)?);
    hash_map.insert(
        FundingStreamReceiver::ZcashFoundation,
        Amount::try_from(15_625_000)?,
    );
    hash_map.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(25_000_000)?,
    );

    assert_eq!(
        funding_stream_values(
            canopy_height,
            network,
            block_subsidy_pre_nsm(canopy_height, network)?
        )
        .unwrap(),
        hash_map
    );

    assert_eq!(
        funding_stream_values(
            canopy_height_plus1,
            network,
            block_subsidy_pre_nsm(canopy_height_plus1, network)?
        )
        .unwrap(),
        hash_map
    );

    assert_eq!(
        funding_stream_values(
            canopy_height_plus2,
            network,
            block_subsidy_pre_nsm(canopy_height_plus2, network)?
        )
        .unwrap(),
        hash_map
    );

    // funding stream period is ending
    let range = network.pre_nu6_funding_streams().height_range();
    let end = range.end;
    let last = (end - 1).unwrap();

    assert_eq!(
        funding_stream_values(last, network, block_subsidy_pre_nsm(last, network)?).unwrap(),
        hash_map
    );

    assert!(funding_stream_values(end, network, block_subsidy_pre_nsm(end, network)?)?.is_empty());

    // TODO: Replace this with Mainnet once there's an NU6 activation height defined for Mainnet
    let network = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(Blossom.activation_height(network).unwrap().0),
            nu6: Some(POST_NU6_FUNDING_STREAMS_MAINNET.height_range().start.0),
            ..Default::default()
        })
        .with_post_nu6_funding_streams(ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(POST_NU6_FUNDING_STREAMS_MAINNET.height_range().clone()),
            // Use default post-NU6 recipients
            recipients: Some(
                POST_NU6_FUNDING_STREAMS_TESTNET
                    .recipients()
                    .iter()
                    .map(|(&receiver, recipient)| ConfiguredFundingStreamRecipient {
                        receiver,
                        numerator: recipient.numerator(),
                        addresses: Some(
                            recipient
                                .addresses()
                                .iter()
                                .map(|addr| addr.to_string())
                                .collect(),
                        ),
                    })
                    .collect(),
            ),
        })
        .to_network();

    let mut hash_map = HashMap::new();
    hash_map.insert(
        FundingStreamReceiver::Deferred,
        Amount::try_from(18_750_000)?,
    );
    hash_map.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(12_500_000)?,
    );

    let nu6_height = Nu6.activation_height(&network).unwrap();

    for height in [
        nu6_height,
        Height(nu6_height.0 + 1),
        Height(nu6_height.0 + 1),
    ] {
        assert_eq!(
            funding_stream_values(height, &network, block_subsidy_pre_nsm(height, &network)?)
                .unwrap(),
            hash_map
        );
    }

    // TODO: Replace this with Mainnet once there's an NU6 activation height defined for Mainnet
    let network = testnet::Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(Blossom.activation_height(&network).unwrap().0),
            nu6: Some(POST_NU6_FUNDING_STREAMS_MAINNET.height_range().start.0),
            ..Default::default()
        })
        .with_post_nu6_funding_streams(ConfiguredFundingStreams {
            // Start checking funding streams from block height 1
            height_range: Some(POST_NU6_FUNDING_STREAMS_MAINNET.height_range().clone()),
            // Use default post-NU6 recipients
            recipients: Some(
                POST_NU6_FUNDING_STREAMS_TESTNET
                    .recipients()
                    .iter()
                    .map(|(&receiver, recipient)| ConfiguredFundingStreamRecipient {
                        receiver,
                        numerator: recipient.numerator(),
                        addresses: Some(
                            recipient
                                .addresses()
                                .iter()
                                .map(|addr| addr.to_string())
                                .collect(),
                        ),
                    })
                    .collect(),
            ),
        })
        .to_network();

    let mut hash_map = HashMap::new();
    hash_map.insert(
        FundingStreamReceiver::Deferred,
        Amount::try_from(18_750_000)?,
    );
    hash_map.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(12_500_000)?,
    );

    let nu6_height = Nu6.activation_height(&network).unwrap();

    for height in [
        nu6_height,
        Height(nu6_height.0 + 1),
        Height(nu6_height.0 + 1),
    ] {
        assert_eq!(
            funding_stream_values(height, &network, block_subsidy_pre_nsm(height, &network)?)
                .unwrap(),
            hash_map
        );
    }

    Ok(())
}

/// Check mainnet and testnet funding stream addresses are valid transparent P2SH addresses.
#[test]
fn test_funding_stream_addresses() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        for (receiver, recipient) in network.pre_nu6_funding_streams().recipients() {
            for address in recipient.addresses() {
                let expected_network_kind = match network.kind() {
                    NetworkKind::Mainnet => NetworkKind::Mainnet,
                    // `Regtest` uses `Testnet` transparent addresses.
                    NetworkKind::Testnet | NetworkKind::Regtest => NetworkKind::Testnet,
                };

                assert_eq!(
                    address.network_kind(),
                    expected_network_kind,
                    "incorrect network for {receiver:?} funding stream address constant: {address}",
                );

                // Asserts if address is not a P2SH address.
                let _script = new_coinbase_script(address);
            }
        }
    }

    Ok(())
}
