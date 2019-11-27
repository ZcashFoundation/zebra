/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitBctv14 {}
/// unimplemented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitGroth16 {}

/// Pre-Sapling JoinSplit data using Sprout-on-BCTV14 proofs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyJoinSplitData {
    /// A sequence of JoinSplit descriptions using BCTV14 proofs.
    pub joinsplits: Vec<JoinSplitBctv14>,
    /// The public key for the JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 pubkey.
    pub pub_key: [u8; 32],
    /// The JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub sig: [u64; 8],
}

/// Post-Sapling JoinSplit data using Sprout-on-Groth16 proofs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaplingJoinSplitData {
    /// A sequence of JoinSplit descriptions using Groth16 proofs.
    pub joinsplits: Vec<JoinSplitGroth16>,
    /// The public key for the JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 pubkey.
    pub pub_key: [u8; 32],
    /// The JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub sig: [u64; 8],
}
