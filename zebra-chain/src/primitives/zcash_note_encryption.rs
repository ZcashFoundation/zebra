//! Contains code that interfaces with the zcash_note_encryption crate from
//! librustzcash.

use crate::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_primitives::convert_tx_to_librustzcash,
    transaction::Transaction,
};

/// Returns true if all Sapling or Orchard outputs, if any, decrypt successfully with
/// an all-zeroes outgoing viewing key.
///
/// # Panics
///
/// If passed a network/height without matching consensus branch ID (pre-Overwinter),
/// since `librustzcash` won't be able to parse it.
pub fn decrypts_successfully(transaction: &Transaction, network: &Network, height: Height) -> bool {
    let network_upgrade = NetworkUpgrade::current(network, height);
    let alt_tx = convert_tx_to_librustzcash(transaction, network_upgrade)
        .expect("zcash_primitives and Zebra transaction formats must be compatible");

    let alt_height = height.0.into();
    let null_sapling_ovk = zcash_primitives::keys::OutgoingViewingKey([0u8; 32]);
    let network = match network {
        Network::Mainnet => zcash_primitives::consensus::Network::MainNetwork,
        Network::Testnet(params) => {
            // # Correctness:
            //
            // There are differences between the `TestNetwork` parameters and those returned by
            // `CRegTestParams()` in zcashd, so this function can't return `TestNetwork` unless
            // Zebra is using the default public Testnet.
            //
            // TODO: Remove this conversion by implementing `zcash_primitives::consensus::Parameters`
            //       for `Network` (#8365).
            assert!(
                params.is_default_testnet(),
                "could not convert configured testnet to zcash_primitives::consensus::Network"
            );
            zcash_primitives::consensus::Network::TestNetwork
        }
    };

    if let Some(bundle) = alt_tx.sapling_bundle() {
        for output in bundle.shielded_outputs().iter() {
            let recovery = zcash_primitives::sapling::note_encryption::try_sapling_output_recovery(
                &network,
                alt_height,
                &null_sapling_ovk,
                output,
            );
            if recovery.is_none() {
                return false;
            }
        }
    }

    if let Some(bundle) = alt_tx.orchard_bundle() {
        for act in bundle.actions() {
            if zcash_note_encryption::try_output_recovery_with_ovk(
                &orchard::note_encryption::OrchardDomain::for_action(act),
                &orchard::keys::OutgoingViewingKey::from([0u8; 32]),
                act,
                act.cv_net(),
                &act.encrypted_note().out_ciphertext,
            )
            .is_none()
            {
                return false;
            }
        }
    }

    true
}
