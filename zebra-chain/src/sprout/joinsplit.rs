use std::io;

use serde::{Deserialize, Serialize};

use crate::{
    amount::{Amount, NegativeAllowed, NonNegative},
    block::MAX_BLOCK_BYTES,
    primitives::{x25519, Bctv14Proof, Groth16Proof, ZkSnarkProof},
    serialization::{
        ReadZcashExt, SerializationError, TrustedPreallocate, WriteZcashExt, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
};

use super::{commitment, note, tree};

/// A _JoinSplit Description_, as described in [protocol specification §7.2][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct JoinSplit<P: ZkSnarkProof> {
    /// A value that the JoinSplit transfer removes from the transparent value
    /// pool.
    pub vpub_old: Amount<NonNegative>,
    /// A value that the JoinSplit transfer inserts into the transparent value
    /// pool.
    ///
    pub vpub_new: Amount<NonNegative>,
    /// A root of the Sprout note commitment tree at some block height in the
    /// past, or the root produced by a previous JoinSplit transfer in this
    /// transaction.
    pub anchor: tree::Root,
    /// A nullifier for the input notes.
    pub nullifiers: [note::Nullifier; 2],
    /// A note commitment for this output note.
    pub commitments: [commitment::NoteCommitment; 2],
    /// An X25519 public key.
    pub ephemeral_key: x25519::PublicKey,
    /// A 256-bit seed that must be chosen independently at random for each
    /// JoinSplit description.
    pub random_seed: [u8; 32],
    /// A message authentication tag.
    pub vmacs: [note::Mac; 2],
    /// A ZK JoinSplit proof, either a
    /// [`Groth16Proof`](crate::primitives::Groth16Proof) or a
    /// [`Bctv14Proof`](crate::primitives::Bctv14Proof).
    #[serde(bound(serialize = "P: ZkSnarkProof", deserialize = "P: ZkSnarkProof"))]
    pub zkproof: P,
    /// A ciphertext component for this output note.
    pub enc_ciphertexts: [note::EncryptedNote; 2],
}

impl<P: ZkSnarkProof> ZcashSerialize for JoinSplit<P> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.vpub_old.zcash_serialize(&mut writer)?;
        self.vpub_new.zcash_serialize(&mut writer)?;
        writer.write_32_bytes(&self.anchor.into())?;
        writer.write_32_bytes(&self.nullifiers[0].into())?;
        writer.write_32_bytes(&self.nullifiers[1].into())?;
        writer.write_32_bytes(&self.commitments[0].into())?;
        writer.write_32_bytes(&self.commitments[1].into())?;
        writer.write_all(&self.ephemeral_key.as_bytes()[..])?;
        writer.write_all(&self.random_seed[..])?;
        self.vmacs[0].zcash_serialize(&mut writer)?;
        self.vmacs[1].zcash_serialize(&mut writer)?;
        self.zkproof.zcash_serialize(&mut writer)?;
        self.enc_ciphertexts[0].zcash_serialize(&mut writer)?;
        self.enc_ciphertexts[1].zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl<P: ZkSnarkProof> JoinSplit<P> {
    /// Return the sprout value balance,
    /// the change in the transaction value pool due to this sprout [`JoinSplit`].
    ///
    /// https://zebra.zfnd.org/dev/rfcs/0012-value-pools.html#definitions
    ///
    /// See [`Transaction::sprout_value_balance`] for details.
    pub fn value_balance(&self) -> Amount<NegativeAllowed> {
        let vpub_new = self
            .vpub_new
            .constrain()
            .expect("constrain::NegativeAllowed is always valid");
        let vpub_old = self
            .vpub_old
            .constrain()
            .expect("constrain::NegativeAllowed is always valid");

        (vpub_new - vpub_old).expect("subtraction of two valid amounts is a valid NegativeAllowed")
    }
}

impl<P: ZkSnarkProof> ZcashDeserialize for JoinSplit<P> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(JoinSplit::<P> {
            vpub_old: (&mut reader).zcash_deserialize_into()?,
            vpub_new: (&mut reader).zcash_deserialize_into()?,
            anchor: tree::Root::from(reader.read_32_bytes()?),
            nullifiers: [
                reader.read_32_bytes()?.into(),
                reader.read_32_bytes()?.into(),
            ],
            commitments: [
                commitment::NoteCommitment::from(reader.read_32_bytes()?),
                commitment::NoteCommitment::from(reader.read_32_bytes()?),
            ],
            ephemeral_key: x25519_dalek::PublicKey::from(reader.read_32_bytes()?),
            random_seed: reader.read_32_bytes()?,
            vmacs: [
                note::Mac::zcash_deserialize(&mut reader)?,
                note::Mac::zcash_deserialize(&mut reader)?,
            ],
            zkproof: P::zcash_deserialize(&mut reader)?,
            enc_ciphertexts: [
                note::EncryptedNote::zcash_deserialize(&mut reader)?,
                note::EncryptedNote::zcash_deserialize(&mut reader)?,
            ],
        })
    }
}

/// The size of a joinsplit, excluding the ZkProof
///
/// Excluding the ZkProof, a Joinsplit consists of an 8 byte vpub_old, an 8 byte vpub_new, a 32 byte anchor,
/// two 32 byte nullifiers, two 32 byte committments, a 32 byte epheremral key, a 32 byte random seed
/// two 32 byte vmacs, and two 601 byte encrypted ciphertexts.
const JOINSPLIT_SIZE_WITHOUT_ZKPROOF: u64 =
    8 + 8 + 32 + (32 * 2) + (32 * 2) + 32 + 32 + (32 * 2) + (601 * 2);
/// The size of a version 2 or 3 joinsplit transaction, which uses a BCTV14 proof.
///
/// A BTCV14 proof takes 296 bytes, per the Zcash [protocol specification §7.2][ps]
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
pub(crate) const BCTV14_JOINSPLIT_SIZE: u64 = JOINSPLIT_SIZE_WITHOUT_ZKPROOF + 296;
/// The size of a version 4+ joinsplit transaction, which uses a Groth16 proof
///
/// A Groth16 proof takes 192 bytes, per the Zcash [protocol specification §7.2][ps]
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
pub(crate) const GROTH16_JOINSPLIT_SIZE: u64 = JOINSPLIT_SIZE_WITHOUT_ZKPROOF + 192;

impl TrustedPreallocate for JoinSplit<Bctv14Proof> {
    fn max_allocation() -> u64 {
        // The longest Vec<JoinSplit> we receive from an honest peer must fit inside a valid block.
        // Since encoding the length of the vec takes at least one byte
        // (MAX_BLOCK_BYTES - 1) / BCTV14_JOINSPLIT_SIZE is a loose upper bound on the max allocation
        (MAX_BLOCK_BYTES - 1) / BCTV14_JOINSPLIT_SIZE
    }
}

impl TrustedPreallocate for JoinSplit<Groth16Proof> {
    // The longest Vec<JoinSplit> we receive from an honest peer must fit inside a valid block.
    // Since encoding the length of the vec takes at least one byte
    // (MAX_BLOCK_BYTES - 1) / GROTH16_JOINSPLIT_SIZE is a loose upper bound on the max allocation
    fn max_allocation() -> u64 {
        (MAX_BLOCK_BYTES - 1) / GROTH16_JOINSPLIT_SIZE
    }
}
