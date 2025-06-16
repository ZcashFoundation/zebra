//! Tests for types and functions for the `getblocktemplate` RPC.

use zcash_address::TryFromRawAddress;
use zcash_keys::address::Address;
use zebra_chain::{
    amount::Amount,
    block::Height,
    parameters::testnet::{self, ConfiguredActivationHeights, ConfiguredFundingStreams},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::Transaction,
};

use super::generate_coinbase_transaction;

/// Tests that a minimal coinbase transaction can be generated.
#[test]
fn minimal_coinbase() -> Result<(), Box<dyn std::error::Error>> {
    let regtest = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu6: Some(1),
            ..Default::default()
        })
        .with_post_nu6_funding_streams(ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(10)),
            recipients: None,
        })
        .to_network();

    // It should be possible to generate a coinbase tx from these params.
    generate_coinbase_transaction(
        &regtest,
        Height(1),
        &Address::try_from_raw_transparent_p2pkh([0x42; 20]).unwrap(),
        Amount::zero(),
        false,
        vec![],
    )
    .transaction
    .zcash_serialize_to_vec()?
    // Deserialization contains checks for elementary consensus rules, which must pass.
    .zcash_deserialize_into::<Transaction>()?;

    Ok(())
}
