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

    let null_sapling_ovk = sapling::keys::OutgoingViewingKey([0u8; 32]);

    // Note that, since this function is used to validate coinbase transactions, we can ignore
    // the "grace period" mentioned in ZIP-212.
    let zip_212_enforcement = if network_upgrade >= NetworkUpgrade::Canopy {
        sapling::note_encryption::Zip212Enforcement::On
    } else {
        sapling::note_encryption::Zip212Enforcement::Off
    };

    if let Some(bundle) = alt_tx.sapling_bundle() {
        for output in bundle.shielded_outputs().iter() {
            let recovery = sapling::note_encryption::try_sapling_output_recovery(
                &null_sapling_ovk,
                output,
                zip_212_enforcement,
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
