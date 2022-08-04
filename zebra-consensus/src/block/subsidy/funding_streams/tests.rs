use super::*;
use color_eyre::Report;
use std::convert::TryFrom;

#[test]
// Check funding streams are correct in the entire period.
fn test_funding_stream_values() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // funding streams not active
    let canopy_height_minus1 = Canopy.activation_height(network).unwrap() - 1;
    assert!(funding_stream_values(canopy_height_minus1.unwrap(), network)?.is_empty());

    // funding stream is active
    let canopy_height = Canopy.activation_height(network);
    let canopy_height_plus1 = Canopy.activation_height(network).unwrap() + 1;
    let canopy_height_plus2 = Canopy.activation_height(network).unwrap() + 2;

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
        funding_stream_values(canopy_height.unwrap(), network).unwrap(),
        hash_map
    );
    assert_eq!(
        funding_stream_values(canopy_height_plus1.unwrap(), network).unwrap(),
        hash_map
    );
    assert_eq!(
        funding_stream_values(canopy_height_plus2.unwrap(), network).unwrap(),
        hash_map
    );

    // funding stream period is ending
    let range = FUNDING_STREAM_HEIGHT_RANGES.get(&network).unwrap();
    let end = range.end;
    let last = end - 1;

    assert_eq!(
        funding_stream_values(last.unwrap(), network).unwrap(),
        hash_map
    );
    assert!(funding_stream_values(end, network)?.is_empty());

    Ok(())
}
