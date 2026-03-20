//! Contains code that interfaces with the zcash_note_encryption crate from
//! librustzcash.

use crate::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
    transaction::Transaction,
};

use orchard::{
    bundle::{Authorization, Bundle},
    primitives::OrchardPrimitives,
};

use zcash_primitives::transaction::OrchardBundle;

/// Returns true if **all** Sapling or Orchard outputs decrypt successfully with
/// an all-zeroes outgoing viewing key.
pub fn decrypts_successfully(tx: &Transaction, network: &Network, height: Height) -> bool {
    let nu = NetworkUpgrade::current(network, height);

    let Ok(tx) = tx.to_librustzcash(nu) else {
        return false;
    };

    let null_sapling_ovk = sapling_crypto::keys::OutgoingViewingKey([0u8; 32]);

    // Note that, since this function is used to validate coinbase transactions, we can ignore
    // the "grace period" mentioned in ZIP-212.
    let zip_212_enforcement = if nu >= NetworkUpgrade::Canopy {
        sapling_crypto::note_encryption::Zip212Enforcement::On
    } else {
        sapling_crypto::note_encryption::Zip212Enforcement::Off
    };

    if let Some(bundle) = tx.sapling_bundle() {
        for output in bundle.shielded_outputs().iter() {
            let recovery = sapling_crypto::note_encryption::try_sapling_output_recovery(
                &null_sapling_ovk,
                output,
                zip_212_enforcement,
            );
            if recovery.is_none() {
                return false;
            }
        }
    }

    if let Some(bundle) = tx.orchard_bundle() {
        let is_decrypted_successfully = match bundle {
            OrchardBundle::OrchardVanilla(bundle) => orchard_bundle_decrypts_successfully(bundle),
            #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
            OrchardBundle::OrchardZSA(bundle) => orchard_bundle_decrypts_successfully(bundle),
        };

        if !is_decrypted_successfully {
            return false;
        }
    }

    true
}

/// Checks if all actions in an Orchard bundle decrypt successfully.
fn orchard_bundle_decrypts_successfully<A: Authorization, V, D: OrchardPrimitives>(
    bundle: &Bundle<A, V, D>,
) -> bool {
    bundle.actions().iter().all(|act| {
        zcash_note_encryption::try_output_recovery_with_ovk(
            &orchard::primitives::OrchardDomain::for_action(act),
            &orchard::keys::OutgoingViewingKey::from([0u8; 32]),
            act,
            act.cv_net(),
            &act.encrypted_note().out_ciphertext,
        )
        .is_some()
    })
}
