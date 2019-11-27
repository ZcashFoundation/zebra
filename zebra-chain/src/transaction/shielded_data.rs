//! Transaction types.

/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendDescription {}
/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDescription {}

/// Sapling-on-Groth16 spend and output descriptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShieldedData {
    /// A sequence of spend descriptions for this transaction.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#spendencoding
    pub shielded_spends: Vec<SpendDescription>,
    /// A sequence of shielded outputs for this transaction.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#outputencoding
    pub shielded_outputs: Vec<OutputDescription>,
    /// A signature on the transaction hash.
    // XXX refine this type to a RedJubjub signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub binding_sig: [u64; 8],
}
