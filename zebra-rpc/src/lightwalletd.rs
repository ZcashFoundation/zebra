//! A tonic gRPC server that implements the lightwalletd `CompactTxStreamer` client API.
//!
//! The interface is defined by the [`zcash/lightwalletd`] proto files, so Zcash light
//! clients that normally connect to a `lightwalletd` instance can connect directly to
//! Zebra instead.
//!
//! [`zcash/lightwalletd`]: https://github.com/zcash/lightwalletd/tree/master/walletrpc

use zebra_chain::{block, transaction, transaction::Transaction};

#[cfg(test)]
mod tests;

pub mod methods;
pub mod server;
pub mod tracing;

// The generated lightwalletd proto for the `cash.z.wallet.sdk.rpc` package.
//
// The generated code only has documentation where the proto files have comments,
// so the `missing_docs` lint is allowed for the whole module.
#[allow(missing_docs)]
mod wire {
    tonic::include_proto!("cash.z.wallet.sdk.rpc");
}
pub use wire::*;

pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("lightwalletd_descriptor");

/// The number of bytes of an encrypted note ciphertext included in compact
/// outputs and actions, from the `compact_formats.proto` specification.
const COMPACT_CIPHERTEXT_SIZE: usize = 52;

/// Returns true if the transaction contains any data that belongs in a
/// [`CompactTx`]: Sapling spends or outputs, or Orchard or Ironwood actions.
pub(crate) fn has_compact_data(tx: &Transaction) -> bool {
    tx.sapling_nullifiers().next().is_some()
        || tx.sapling_outputs().next().is_some()
        || tx.orchard_actions().next().is_some()
        || tx.ironwood_actions().next().is_some()
}

/// Returns true if the transaction contains any nullifiers, so it belongs in a
/// nullifiers-only [`CompactBlock`].
///
/// Outputs are omitted in that mode, so a transaction with only Sapling outputs would
/// otherwise be included as a [`CompactTx`] carrying nothing but an index and a hash.
pub(crate) fn has_nullifiers(tx: &Transaction) -> bool {
    tx.sapling_nullifiers().next().is_some()
        || tx.orchard_actions().next().is_some()
        || tx.ironwood_actions().next().is_some()
}

/// Builds a [`CompactOrchardAction`] from an Orchard-encoded action.
///
/// Ironwood reuses the Orchard action encoding, so the same conversion serves both pools.
/// If `nullifiers_only` is true, only the nullifier is included.
fn compact_action(
    action: &::orchard::Action<
        <::orchard::bundle::Authorized as ::orchard::bundle::Authorization>::SpendAuth,
    >,
    nullifiers_only: bool,
) -> CompactOrchardAction {
    let mut compact_action = CompactOrchardAction {
        nullifier: action.nullifier().to_bytes().to_vec(),
        ..Default::default()
    };

    if !nullifiers_only {
        compact_action.cmx = action.cmx().to_bytes().to_vec();
        compact_action.ephemeral_key = action.encrypted_note().epk_bytes.to_vec();
        compact_action.ciphertext =
            action.encrypted_note().enc_ciphertext[..COMPACT_CIPHERTEXT_SIZE].to_vec();
    }

    compact_action
}

impl CompactBlock {
    /// Builds a [`CompactBlock`] from a full block and the note commitment tree sizes
    /// at the end of that block.
    ///
    /// If `nullifiers_only` is true, compact outputs are omitted and compact actions
    /// only contain nullifiers, matching the lightwalletd `GetBlockNullifiers` and
    /// `GetBlockRangeNullifiers` methods.
    pub fn from_block(
        block: &block::Block,
        hash: block::Hash,
        height: block::Height,
        sapling_tree_size: u64,
        orchard_tree_size: u64,
        ironwood_tree_size: u64,
        nullifiers_only: bool,
    ) -> Self {
        let vtx = block
            .transactions
            .iter()
            .enumerate()
            .filter(|(_, tx)| {
                if nullifiers_only {
                    has_nullifiers(tx)
                } else {
                    has_compact_data(tx)
                }
            })
            .map(|(index, tx)| {
                CompactTx::from_transaction(index as u64, tx.hash(), tx, nullifiers_only)
            })
            .collect();

        CompactBlock {
            proto_version: 0,
            height: height.0.into(),
            hash: hash.0.to_vec(),
            prev_hash: block.header.previous_block_hash.0.to_vec(),
            // Cast is safe until 2106: consensus rules restrict block times to 32-bit values.
            time: block.header.time.timestamp() as u32,
            header: Vec::new(),
            vtx,
            chain_metadata: Some(ChainMetadata {
                // Casts are safe: note commitment trees hold at most `2^32 - 1` notes,
                // since note positions are 32-bit values.
                sapling_commitment_tree_size: sapling_tree_size as u32,
                orchard_commitment_tree_size: orchard_tree_size as u32,
                ironwood_commitment_tree_size: ironwood_tree_size as u32,
            }),
        }
    }
}

impl CompactTx {
    /// Builds a [`CompactTx`] from a transaction, its precomputed mined transaction
    /// ID, and its index within its block.
    ///
    /// If `nullifiers_only` is true, compact outputs are omitted and compact actions
    /// only contain nullifiers.
    pub fn from_transaction(
        index: u64,
        hash: transaction::Hash,
        tx: &Transaction,
        nullifiers_only: bool,
    ) -> Self {
        let spends = tx
            .sapling_nullifiers()
            .map(|nullifier| CompactSaplingSpend {
                nf: <[u8; 32]>::from(nullifier).to_vec(),
            })
            .collect();

        let outputs = if nullifiers_only {
            Vec::new()
        } else {
            tx.sapling_outputs()
                .map(|output| CompactSaplingOutput {
                    cmu: output.cmu().to_bytes().to_vec(),
                    ephemeral_key: output.ephemeral_key().0.to_vec(),
                    ciphertext: output.enc_ciphertext()[..COMPACT_CIPHERTEXT_SIZE].to_vec(),
                })
                .collect()
        };

        let actions = tx
            .orchard_actions()
            .map(|action| compact_action(action, nullifiers_only))
            .collect();

        let ironwood_actions = tx
            .ironwood_actions()
            .map(|action| compact_action(action, nullifiers_only))
            .collect();

        CompactTx {
            index,
            hash: hash.0.to_vec(),
            // The fee is optional in the protocol, and stateless servers can't
            // calculate it for transactions with transparent inputs, so it is unset.
            fee: 0,
            spends,
            outputs,
            actions,
            ironwood_actions,
        }
    }
}
