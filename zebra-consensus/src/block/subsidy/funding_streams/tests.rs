//! Tests for funding streams.

use color_eyre::Report;
use zebra_chain::parameters::{subsidy::FundingStreamReceiver, NetworkKind};

use crate::block::subsidy::general::block_subsidy;

use super::*;

/// Checks that the Mainnet funding stream values are correct.
// FIXME: Would this work after Nu7 activation?
#[test]
fn test_funding_stream_values() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = &Network::Mainnet;

    let canopy_activation_height = Canopy.activation_height(network).unwrap();
    let nu6_activation_height = Nu6.activation_height(network).unwrap();

    let dev_fund_height_range = network.pre_nu6_funding_streams().height_range();
    let nu6_fund_height_range = network.post_nu6_funding_streams().height_range();

    let nu6_fund_end = Height(3_146_400);

    assert_eq!(canopy_activation_height, Height(1_046_400));
    assert_eq!(nu6_activation_height, Height(2_726_400));

    assert_eq!(dev_fund_height_range.start, canopy_activation_height);
    assert_eq!(dev_fund_height_range.end, nu6_activation_height);

    assert_eq!(nu6_fund_height_range.start, nu6_activation_height);
    assert_eq!(nu6_fund_height_range.end, nu6_fund_end);

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
    ] {
        let fsv = funding_stream_values(height, network, block_subsidy(height, network)?).unwrap();

        if height < canopy_activation_height {
            assert!(fsv.is_empty());
        } else if height < nu6_activation_height {
            assert_eq!(fsv, expected_dev_fund);
        } else if height < nu6_fund_end {
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
