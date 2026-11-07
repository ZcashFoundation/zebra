//! State snapshot commitments: the trust anchor of state-snapshot sync.
//!
//! A pruned node that starts from a state snapshot trusts the snapshot's
//! contents through a [`StateSnapshotCommitment`]: the height and block hash
//! the snapshot was taken at, and the hash of its manifest, which in turn
//! names every piece of the snapshot by hash. A commitment is pinned in this
//! crate for a network like its checkpoints are, or configured by the
//! operator.

use std::{fmt, str::FromStr};

use hex::FromHex;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::{
    block,
    parameters::{subsidy::FundingStreamReceiver, Network, NetworkKind, NetworkUpgrade},
    serialization::BytesInDisplayOrder,
    transparent,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(test)]
mod tests;

/// The SHA-256 hash of a content-addressed synchronization artifact: a
/// snapshot manifest or piece, as served by `get-object` requests.
///
/// Displayed, parsed, and serialized as 64 lowercase hex digits, in byte order.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct ObjectHash(pub [u8; 32]);

impl ObjectHash {
    /// Returns the hash of `object`'s complete contents.
    pub fn of(object: &[u8]) -> Self {
        ObjectHash(Sha256::digest(object).into())
    }
}

impl fmt::Display for ObjectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for ObjectHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for ObjectHash {
    type Err = hex::FromHexError;

    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        <[u8; 32]>::from_hex(hex).map(ObjectHash)
    }
}

impl From<[u8; 32]> for ObjectHash {
    fn from(bytes: [u8; 32]) -> Self {
        ObjectHash(bytes)
    }
}

impl Serialize for ObjectHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ObjectHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        hex.parse().map_err(de::Error::custom)
    }
}

/// What a node trusts when it starts from a state snapshot: the snapshot's
/// height, the hash of the block at that height, and the hash of the manifest
/// that names the snapshot's pieces.
///
/// In a config file it is written as `{ height, hash, manifest }`, with the
/// hashes as hex strings.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSnapshotCommitment {
    /// The height the snapshot was taken at.
    pub height: block::Height,

    /// The hash of the block at [`Self::height`].
    #[serde(rename = "hash", with = "block_hash_hex")]
    pub block_hash: block::Hash,

    /// The hash of the snapshot manifest.
    pub manifest: ObjectHash,
}

/// The height of the Mainnet state snapshot this build pins: a checkpoint below
/// the last checkpoint, so the checkpoint list authenticates the snapshot block,
/// and the block after it is checkpoint-verified.
pub const MAINNET_STATE_SNAPSHOT_HEIGHT: block::Height = block::Height(3_522_060);

/// The hash of the block at [`MAINNET_STATE_SNAPSHOT_HEIGHT`]: the Mainnet
/// checkpoint there, in the usual display byte order.
pub const MAINNET_STATE_SNAPSHOT_BLOCK_HASH: &str =
    "00000000000705964fc4c75da9cce97978197b666c9dcdfc1bcf8d44cd776e4d";

/// PLACEHOLDER: the manifest hash of the Mainnet state snapshot at
/// [`MAINNET_STATE_SNAPSHOT_HEIGHT`], which the snapshot's deterministic export
/// gives. `None` until that export is done and checked, so this build pins no
/// Mainnet commitment yet, and Mainnet nodes sync normally.
pub const MAINNET_STATE_SNAPSHOT_MANIFEST: Option<ObjectHash> = None;

impl Network {
    /// Returns the state snapshot commitment this build pins for the network,
    /// if any.
    ///
    /// A commitment is pinned per release, together with the checkpoint list
    /// it was cut with. Only Mainnet has one, at a checkpoint, once its
    /// [`MAINNET_STATE_SNAPSHOT_MANIFEST`] is filled in.
    pub fn state_snapshot_commitment(&self) -> Option<StateSnapshotCommitment> {
        if !matches!(self, Network::Mainnet) {
            return None;
        }

        Some(StateSnapshotCommitment {
            height: MAINNET_STATE_SNAPSHOT_HEIGHT,
            block_hash: MAINNET_STATE_SNAPSHOT_BLOCK_HASH
                .parse()
                .expect("the pinned block hash is valid hex"),
            manifest: MAINNET_STATE_SNAPSHOT_MANIFEST?,
        })
    }

    /// Returns the hash of this network's consensus parameters that a state
    /// snapshot manifest for the network carries, or `None` for Mainnet and
    /// the default Testnet, which their genesis hash identifies.
    ///
    /// Every Regtest network shares one genesis block, and a configured
    /// testnet can change its activation heights or funding streams without
    /// changing its genesis block, so a manifest for one of them names the
    /// parameters it was exported under. The checkpoints are excluded: they do
    /// not change which chain is valid, and a node that imports a snapshot
    /// usually has more of them than the node that exported it. The network
    /// name and magic are excluded too, because they only label the network.
    pub fn state_snapshot_parameters_hash(&self) -> Option<[u8; 32]> {
        let Network::Testnet(parameters) = self else {
            return None;
        };
        if parameters.is_default_testnet() {
            return None;
        }

        let mut hash = ParametersHasher::default();
        hash.u8(network_kind_code(self.kind()));
        hash.bytes(&parameters.genesis_hash().0);

        let activation_heights = parameters.activation_heights();
        hash.len(activation_heights.len());
        for (height, network_upgrade) in activation_heights {
            hash.u32(height.0);
            hash.u8(network_upgrade_code(*network_upgrade));
        }

        hash.u32(parameters.slow_start_interval().0);
        hash.u32(parameters.slow_start_shift().0);

        let funding_streams = parameters.funding_streams();
        hash.len(funding_streams.len());
        for funding_streams in funding_streams {
            hash.u32(funding_streams.height_range().start.0);
            hash.u32(funding_streams.height_range().end.0);

            // The recipients are a hash map, so they are hashed in receiver order.
            let mut recipients: Vec<_> = funding_streams
                .recipients()
                .iter()
                .map(|(receiver, recipient)| (receiver_code(*receiver), recipient))
                .collect();
            recipients.sort_by_key(|(code, _recipient)| *code);
            hash.len(recipients.len());
            for (code, recipient) in recipients {
                hash.u8(code);
                hash.bytes(&recipient.numerator().to_le_bytes());
                hash.len(recipient.addresses().len());
                for address in recipient.addresses() {
                    hash.address(address);
                }
            }
        }

        hash.bytes(
            &parameters
                .target_difficulty_limit()
                .bytes_in_serialized_order(),
        );
        hash.u8(u8::from(parameters.disable_pow()));
        hash.u8(u8::from(
            parameters.should_allow_unshielded_coinbase_spends(),
        ));
        hash.bytes(&parameters.initial_nsm_value_balance().to_bytes());
        hash.optional_height(parameters.nsm_reissuance_height());
        hash.bytes(&parameters.pre_blossom_halving_interval().to_le_bytes());
        hash.bytes(&parameters.post_blossom_halving_interval().to_le_bytes());

        let lockbox_disbursements = parameters.lockbox_disbursements();
        hash.len(lockbox_disbursements.len());
        for (address, amount) in lockbox_disbursements {
            hash.address(&address);
            hash.bytes(&amount.to_bytes());
        }

        hash.optional_height(parameters.temporary_orchard_disabling_soft_fork_height());

        Some(hash.finish())
    }
}

/// The domain separator of [`Network::state_snapshot_parameters_hash`], and
/// the version of its encoding.
///
/// The encoding uses fixed codes and raw bytes, not names or address
/// encodings, so it only changes when this module changes, and then this
/// version must change with it: an existing manifest's hash would otherwise
/// stop matching its network's. A test pins the hash of the default Regtest
/// parameters.
const PARAMETERS_HASH_DOMAIN: &[u8] = b"ZebraStateSnapshotParameters/1";

/// Returns the code a network kind is hashed as.
fn network_kind_code(kind: NetworkKind) -> u8 {
    match kind {
        NetworkKind::Mainnet => 0,
        NetworkKind::Testnet => 1,
        NetworkKind::Regtest => 2,
    }
}

/// Returns the code a network upgrade is hashed as: fixed, unlike its name.
fn network_upgrade_code(network_upgrade: NetworkUpgrade) -> u8 {
    match network_upgrade {
        NetworkUpgrade::Genesis => 0,
        NetworkUpgrade::BeforeOverwinter => 1,
        NetworkUpgrade::Overwinter => 2,
        NetworkUpgrade::Sapling => 3,
        NetworkUpgrade::Blossom => 4,
        NetworkUpgrade::Heartwood => 5,
        NetworkUpgrade::Canopy => 6,
        NetworkUpgrade::Nu5 => 7,
        NetworkUpgrade::Nu6 => 8,
        NetworkUpgrade::Nu6_1 => 9,
        NetworkUpgrade::Nu6_2 => 10,
        NetworkUpgrade::Nu6_3 => 11,
        NetworkUpgrade::Nu7 => 12,
        #[cfg(zcash_unstable = "zfuture")]
        NetworkUpgrade::ZFuture => 255,
    }
}

/// Returns the code a funding stream receiver is hashed as.
fn receiver_code(receiver: FundingStreamReceiver) -> u8 {
    match receiver {
        FundingStreamReceiver::Ecc => 0,
        FundingStreamReceiver::ZcashFoundation => 1,
        FundingStreamReceiver::MajorGrants => 2,
        FundingStreamReceiver::Deferred => 3,
    }
}

/// Hashes the consensus parameters of a network as a sequence of
/// little-endian integers and length-prefixed byte strings.
struct ParametersHasher(Sha256);

impl Default for ParametersHasher {
    fn default() -> Self {
        ParametersHasher(Sha256::new_with_prefix(PARAMETERS_HASH_DOMAIN))
    }
}

impl ParametersHasher {
    fn u8(&mut self, value: u8) {
        self.0.update([value]);
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// Hashes a collection length.
    fn len(&mut self, len: usize) {
        // `usize` always fits in `u64` on supported targets.
        self.0.update((len as u64).to_le_bytes());
    }

    /// Hashes a transparent address as its kind, its network kind and its
    /// 20-byte hash, not as its encoding, which can change how it is
    /// displayed without changing the address.
    fn address(&mut self, address: &transparent::Address) {
        let (kind, network_kind, hash) = match address {
            transparent::Address::PayToScriptHash {
                network_kind,
                script_hash,
            } => (0, network_kind, script_hash),
            transparent::Address::PayToPublicKeyHash {
                network_kind,
                pub_key_hash,
            } => (1, network_kind, pub_key_hash),
            transparent::Address::Tex {
                network_kind,
                validating_key_hash,
            } => (2, network_kind, validating_key_hash),
        };
        self.u8(kind);
        self.u8(network_kind_code(*network_kind));
        self.bytes(hash);
    }

    fn optional_height(&mut self, height: Option<block::Height>) {
        match height {
            Some(height) => {
                self.u8(1);
                self.u32(height.0);
            }
            None => self.u8(0),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// Serializes a block hash as its display-order hex string, as the
/// `block::Hash` `Display` and `FromStr` impls do.
mod block_hash_hex {
    use serde::{de, Deserialize, Deserializer, Serializer};

    use crate::block;

    pub fn serialize<S: Serializer>(hash: &block::Hash, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hash.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<block::Hash, D::Error> {
        let hex = String::deserialize(deserializer)?;
        hex.parse().map_err(de::Error::custom)
    }
}
