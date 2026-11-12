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
        block::Height,
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
    assert_eq!(
        custom_testnet.fee_reduction_height(),
        testnet.fee_reduction_height()
    );
    assert_eq!(regtest.fee_reduction_height(), Height(1));

    for network in [Network::Mainnet, testnet, regtest, custom_testnet] {
        let activation = network.fee_reduction_height();
        for (height, marginal_fee, cap) in [
            (activation.previous().unwrap(), 5_000, 4.0),
            (activation, 1_000, 10.0),
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
                cap,
            );
        }
    }
}

#[test]
fn zip2009_mempool_admission_at_activation() {
    use crate::{
        parameters::Network,
        transaction::{LockTime, Transaction, UnminedTx, VerifiedUnminedTx},
    };
    use std::sync::Arc;

    let transaction = UnminedTx::from(Arc::new(Transaction::test_v1(
        vec![],
        vec![],
        LockTime::unlocked(),
    )));
    for network in [Network::Mainnet, Network::new_default_testnet()] {
        let activation = network.fee_reduction_height();
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
                assert_eq!(verified.fee_weight_ratio, 1.0);
            } else {
                assert_eq!(result.unwrap_err(), Error::UnpaidActions);
            }
        }
    }
}
