//! ZIP-317 tests.

use super::{mempool_checks, Amount, Error, NonNegative};
#[test]
fn zip317_unpaid_actions_err() {
    let check = mempool_checks(1, Amount::try_from(1).unwrap(), 1);

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::UnpaidActions));
}

#[test]
fn zip317_minimum_rate_fee_err() {
    let check = mempool_checks(0, Amount::try_from(1).unwrap(), 1000);

    assert!(check.is_err());
    assert_eq!(check.err(), Some(Error::FeeBelowMinimumRate));
}

#[test]
fn zip317_mempool_checks_ok() {
    assert!(mempool_checks(0, Amount::try_from(100).unwrap(), 1000).is_ok())
}

#[test]
fn zip2009_fee_parameters_at_activation() {
    use crate::{
        parameters::{testnet::Parameters, Network},
        transaction::{LockTime, Transaction, UnminedTx},
    };

    let transaction = UnminedTx::from(std::sync::Arc::new(Transaction::test_v1(
        vec![],
        vec![],
        LockTime::unlocked(),
    )));
    assert_eq!(transaction.conventional_actions, 2);

    let testnet = Network::new_default_testnet();
    let regtest = Network::new_regtest(Default::default());
    let custom_testnet = Parameters::build()
        .with_network_name("FeeTest")
        .unwrap()
        .to_network()
        .unwrap();

    for network in [Network::Mainnet, testnet, regtest, custom_testnet] {
        let activation = network.fee_reduction_height();
        // The fee is 10 times the legacy conventional fee, which is above the legacy cap, and
        // 125 times the reduced one, which has no cap.
        for (height, marginal_fee, weight_ratio) in [
            (activation.previous().unwrap(), 5_000, 4.0),
            (activation, 400, 125.0),
        ] {
            assert_eq!(super::marginal_fee(&network, height), marginal_fee);
            assert_eq!(
                super::conventional_fee(transaction.conventional_actions, &network, height),
                Amount::<NonNegative>::try_from(2 * marginal_fee).unwrap(),
            );
            assert_eq!(
                super::conventional_fee_weight_ratio(
                    &transaction,
                    Amount::try_from(100_000).unwrap(),
                    &network,
                    height,
                ),
                weight_ratio,
            );
        }
    }
}

#[test]
fn zip2009_mempool_admission_at_activation() {
    use crate::{
        block::Height,
        parameters::{testnet::Parameters, Network},
        transaction::{LockTime, Transaction, UnminedTx, VerifiedUnminedTx},
    };
    use std::sync::Arc;

    let transaction = UnminedTx::from(Arc::new(Transaction::test_v1(
        vec![],
        vec![],
        LockTime::unlocked(),
    )));
    let custom_testnet = Parameters::build()
        .with_network_name("FeeTest")
        .unwrap()
        .to_network()
        .unwrap();
    for (network, activation) in [
        (Network::Mainnet, Height(3_590_000)),
        (Network::new_default_testnet(), Height(4_465_026)),
        (Network::new_regtest(Default::default()), Height(1)),
        (custom_testnet, Height(1)),
    ] {
        for (height, accepted) in [(activation.previous().unwrap(), false), (activation, true)] {
            let result = VerifiedUnminedTx::new(
                transaction.clone(),
                Amount::try_from(2_000).unwrap(),
                0,
                0,
                Arc::new(vec![]),
                &network,
                height,
            );
            if accepted {
                let verified = result.expect("the reduced conventional fee applies at activation");
                assert!(verified.pays_conventional_fee());
                // The fee is 2.5 times the reduced conventional fee for 2 actions.
                assert_eq!(verified.fee_weight_ratio, 2.5);
            } else {
                assert_eq!(result.unwrap_err(), Error::UnpaidActions);
            }
        }
    }
}

/// The serialized-size fee floor never rejects a transaction that pays its conventional fee,
/// before or after the fee reduction, whatever its size. The reduced conventional fee for two
/// logical actions is below the floor that `zcashd` has for a transaction of 8,010 bytes or more,
/// such as a two-action Orchard transaction.
#[test]
fn conventional_fee_is_never_below_the_size_fee_floor() {
    use crate::{block::MAX_BLOCK_BYTES, parameters::Network};

    /// The serialized size of a v5 transaction with two Orchard actions and nothing else.
    const TWO_ACTION_ORCHARD_TX_SIZE: usize = 9_165;

    let network = Network::Mainnet;
    let activation = network.fee_reduction_height();
    let max_size = usize::try_from(MAX_BLOCK_BYTES).unwrap();

    for height in [activation.previous().unwrap(), activation] {
        let conventional_fee = super::conventional_fee(2, &network, height);
        for size in [1, 4_000, 8_009, 8_010, TWO_ACTION_ORCHARD_TX_SIZE, max_size] {
            assert_eq!(mempool_checks(0, conventional_fee, size), Ok(()), "{size}");
        }
    }

    // The floor still applies to a fee below its cap.
    assert_eq!(
        mempool_checks(0, Amount::try_from(399).unwrap(), 4_000),
        Err(Error::FeeBelowMinimumRate)
    );
    assert_eq!(
        mempool_checks(0, Amount::try_from(400).unwrap(), max_size),
        Ok(())
    );
}
