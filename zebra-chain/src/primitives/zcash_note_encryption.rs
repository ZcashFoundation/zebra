//! Contains code that interfaces with the zcash_note_encryption crate from
//! librustzcash.

use std::ops::Deref;

use crate::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
    transaction::Transaction,
};

/// Returns true if all Sapling, Orchard, or Ironwood outputs, if any, decrypt successfully
/// with an all-zeroes outgoing viewing key.
pub fn decrypts_successfully(tx: &Transaction, network: &Network, height: Height) -> bool {
    zero_ovk_memos(tx, network, height).all(|memo| memo.is_some())
}

/// Returns the memo of each Sapling, Orchard, and Ironwood output of `tx`, in transaction encoding
/// order, as recovered with an all-zeroes outgoing viewing key, or `None` for an output that does
/// not decrypt with that key.
///
/// Consensus requires every shielded output of a coinbase transaction to decrypt with this key, so
/// these are the publicly readable memos of a coinbase transaction.
pub fn zero_ovk_memos<'a>(
    tx: &'a Transaction,
    network: &Network,
    height: Height,
) -> impl Iterator<Item = Option<[u8; 512]>> + 'a {
    let nu = NetworkUpgrade::current(network, height);

    let null_sapling_ovk = sapling_crypto::keys::OutgoingViewingKey([0u8; 32]);

    // Note that, since this function is used to validate coinbase transactions, we can ignore
    // the "grace period" mentioned in ZIP-212.
    let zip_212_enforcement = if nu >= NetworkUpgrade::Canopy {
        sapling_crypto::note_encryption::Zip212Enforcement::On
    } else {
        sapling_crypto::note_encryption::Zip212Enforcement::Off
    };

    let sapling = tx
        .inner()
        .deref()
        .sapling_bundle()
        .into_iter()
        .flat_map(|bundle| bundle.shielded_outputs())
        .map(move |output| {
            sapling_crypto::note_encryption::try_sapling_output_recovery(
                &null_sapling_ovk,
                output,
                zip_212_enforcement,
            )
            .map(|(_, _, memo)| memo)
        });

    let orchard = tx
        .inner()
        .deref()
        .orchard_bundle()
        .into_iter()
        .flat_map(|bundle| bundle.actions())
        .map(|act| {
            zcash_note_encryption::try_output_recovery_with_ovk(
                &orchard::note_encryption::OrchardDomain::for_action(act),
                &orchard::keys::OutgoingViewingKey::from([0u8; 32]),
                act,
                act.cv_net(),
                &act.encrypted_note().out_ciphertext,
            )
            .map(|(_, _, memo)| memo)
        });

    // From NU6.3, newly shielded coinbase value is routed to the Ironwood pool, so the coinbase
    // output-decryptability rule must cover Ironwood actions too. The Ironwood bundle reuses the
    // Orchard action shape but its notes use the `IronwoodDomain` (V3) note-plaintext version.
    let ironwood = tx
        .ironwood_bundle()
        .into_iter()
        .flat_map(|bundle| bundle.actions())
        .map(|act| {
            zcash_note_encryption::try_output_recovery_with_ovk(
                &orchard::note_encryption::IronwoodDomain::for_action(act),
                &orchard::keys::OutgoingViewingKey::from([0u8; 32]),
                act,
                act.cv_net(),
                &act.encrypted_note().out_ciphertext,
            )
            .map(|(_, _, memo)| memo)
        });

    sapling.chain(orchard).chain(ironwood)
}
