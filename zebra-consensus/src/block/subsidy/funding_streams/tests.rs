//! Tests for funding streams.

#![allow(clippy::unwrap_in_result)]

use std::collections::HashMap;

use color_eyre::Report;
use zebra_chain::amount::Amount;
use zebra_chain::parameters::NetworkUpgrade::*;
use zebra_chain::parameters::{subsidy::FundingStreamReceiver, NetworkKind};

use crate::block::subsidy::new_coinbase_script;

use super::*;

/// Checks that the Mainnet funding stream values are correct.
#[test]
fn test_funding_stream_values() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = &Network::Mainnet;

    let canopy_activation_height = Canopy.activation_height(network).unwrap();
    let nu6_activation_height = Nu6.activation_height(network).unwrap();
    let nu6_1_activation_height = Nu6_1.activation_height(network).unwrap();

    let dev_fund_height_range = network.all_funding_streams()[0].height_range();
    let nu6_fund_height_range = network.all_funding_streams()[1].height_range();
    let nu6_1_fund_height_range = network.all_funding_streams()[2].height_range();

    let nu6_fund_end = Height(3_146_400);
    let nu6_1_fund_end = Height(4_406_400);

    assert_eq!(canopy_activation_height, Height(1_046_400));
    assert_eq!(nu6_activation_height, Height(2_726_400));
    assert_eq!(nu6_1_activation_height, Height(3_146_400));

    assert_eq!(dev_fund_height_range.start, canopy_activation_height);
    assert_eq!(dev_fund_height_range.end, nu6_activation_height);

    assert_eq!(nu6_fund_height_range.start, nu6_activation_height);
    assert_eq!(nu6_fund_height_range.end, nu6_fund_end);

    assert_eq!(nu6_1_fund_height_range.start, nu6_1_activation_height);
    assert_eq!(nu6_1_fund_height_range.end, nu6_1_fund_end);

    assert_eq!(dev_fund_height_range.end, nu6_fund_height_range.start);

    let mut expected_dev_fund = HashMap::new();

    expected_dev_fund.insert(FundingStreamReceiver::Ecc, Amount::try_from(21_875_000)?);
    expected_dev_fund.insert(
        FundingStreamReceiver::ZcashFoundation,
        Amount::try_from(15_625_000)?,
    );
    expected_dev_fund.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(25_000_000)?,
    );
    let expected_dev_fund = expected_dev_fund;

    let mut expected_nu6_fund = HashMap::new();
    expected_nu6_fund.insert(
        FundingStreamReceiver::Deferred,
        Amount::try_from(18_750_000)?,
    );
    expected_nu6_fund.insert(
        FundingStreamReceiver::MajorGrants,
        Amount::try_from(12_500_000)?,
    );
    let expected_nu6_fund = expected_nu6_fund;

    for height in [
        dev_fund_height_range.start.previous().unwrap(),
        dev_fund_height_range.start,
        dev_fund_height_range.start.next().unwrap(),
        dev_fund_height_range.end.previous().unwrap(),
        dev_fund_height_range.end,
        dev_fund_height_range.end.next().unwrap(),
        nu6_fund_height_range.start.previous().unwrap(),
        nu6_fund_height_range.start,
        nu6_fund_height_range.start.next().unwrap(),
        nu6_fund_height_range.end.previous().unwrap(),
        nu6_fund_height_range.end,
        nu6_fund_height_range.end.next().unwrap(),
        nu6_1_fund_height_range.start.previous().unwrap(),
        nu6_1_fund_height_range.start,
        nu6_1_fund_height_range.start.next().unwrap(),
        nu6_1_fund_height_range.end.previous().unwrap(),
        nu6_1_fund_height_range.end,
        nu6_1_fund_height_range.end.next().unwrap(),
    ] {
        let fsv = funding_stream_values(height, network, block_subsidy(height, network)?).unwrap();

        if height < canopy_activation_height {
            assert!(fsv.is_empty());
        } else if height < nu6_activation_height {
            assert_eq!(fsv, expected_dev_fund);
        } else if height < nu6_1_fund_end {
            // NU6 and NU6.1 funding streams are in the same halving and expected to have the same values
            assert_eq!(fsv, expected_nu6_fund);
        } else {
            assert!(fsv.is_empty());
        }
    }

    Ok(())
}

/// Check mainnet and testnet funding stream addresses are valid transparent P2SH addresses.
#[test]
fn test_funding_stream_addresses() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        for (receiver, recipient) in network
            .all_funding_streams()
            .iter()
            .flat_map(|fs| fs.recipients())
        {
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

//Test if funding streams ranges do not overlap
#[test]
fn test_funding_stream_ranges_dont_overlap() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        let funding_streams = network.all_funding_streams();
        // This is quadratic but it's fine since the number of funding streams is small.
        for i in 0..funding_streams.len() {
            for j in (i + 1)..funding_streams.len() {
                let range_a = funding_streams[i].height_range();
                let range_b = funding_streams[j].height_range();
                assert!(
                    // https://stackoverflow.com/a/325964
                    !(range_a.start < range_b.end && range_b.start < range_a.end),
                    "Funding streams {i} and {j} overlap: {range_a:?} and {range_b:?}",
                );
            }
        }
    }
    Ok(())
}
