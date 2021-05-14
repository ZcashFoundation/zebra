//! Signature hashes for Zcash transactions

use super::Transaction;

use crate::{
    parameters::{
        ConsensusBranchId, NetworkUpgrade, OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID,
        TX_V5_VERSION_GROUP_ID,
    },
    sapling,
    serialization::{WriteZcashExt, ZcashSerialize},
    transparent,
};

use byteorder::{LittleEndian, WriteBytesExt};

use std::{
    convert::TryInto,
    io::{self, Write},
};

use crate::primitives::zcash_primitives::sighash;

static ZIP143_EXPLANATION: &str = "Invalid transaction version: after Overwinter activation transaction versions 1 and 2 are rejected";
static ZIP243_EXPLANATION: &str = "Invalid transaction version: after Sapling activation transaction versions 1, 2, and 3 are rejected";

const ZCASH_SIGHASH_PERSONALIZATION_PREFIX: &[u8; 12] = b"ZcashSigHash";
const ZCASH_PREVOUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashPrevoutHash";
const ZCASH_SEQUENCE_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSequencHash";
const ZCASH_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashOutputsHash";
const ZCASH_JOINSPLITS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashJSplitsHash";
const ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSSpendsHash";
const ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSOutputHash";

bitflags::bitflags! {
    /// The different SigHash types, as defined in https://zips.z.cash/zip-0143
    pub struct HashType: u32 {
        /// Sign all the outputs
        const ALL = 0b0000_0001;
        /// Sign none of the outputs - anyone can spend
        const NONE = 0b0000_0010;
        /// Sign one of the outputs - anyone can spend the rest
        const SINGLE = Self::ALL.bits | Self::NONE.bits;
        /// Anyone can add inputs to this transaction
        const ANYONECANPAY = 0b1000_0000;
    }
}

impl HashType {
    fn masked(self) -> Self {
        Self::from_bits_truncate(self.bits & 0b0001_1111)
    }
}

/// A Signature Hash (or SIGHASH) as specified in
/// https://zips.z.cash/protocol/protocol.pdf#sighash
#[derive(Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, Debug)]
pub struct Hash(pub [u8; 32]);

impl AsRef<[u8; 32]> for Hash {
    fn as_ref(&self) -> &[u8; 32] {
        &self.0
    }
}

impl AsRef<[u8]> for Hash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

pub(super) struct SigHasher<'a> {
    trans: &'a Transaction,
    hash_type: HashType,
    network_upgrade: NetworkUpgrade,
    input: Option<(transparent::Output, &'a transparent::Input, usize)>,
}

impl<'a> SigHasher<'a> {
    pub fn new(
        trans: &'a Transaction,
        hash_type: HashType,
        network_upgrade: NetworkUpgrade,
        input: Option<(u32, transparent::Output)>,
    ) -> Self {
        let input = if let Some((index, prevout)) = input {
            let index = index as usize;
            let inputs = trans.inputs();

            Some((prevout, &inputs[index], index))
        } else {
            None
        };

        SigHasher {
            trans,
            hash_type,
            network_upgrade,
            input,
        }
    }

    pub(super) fn sighash(self) -> Hash {
        use NetworkUpgrade::*;
        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(&self.personal())
            .to_state();

        match self.network_upgrade {
            Genesis | BeforeOverwinter => unreachable!(ZIP143_EXPLANATION),
            Overwinter => self
                .hash_sighash_zip143(&mut hash)
                .expect("serialization into hasher never fails"),
            Sapling | Blossom | Heartwood | Canopy => self
                .hash_sighash_zip243(&mut hash)
                .expect("serialization into hasher never fails"),
            Nu5 => return self.hash_sighash_zip244(),
        }

        Hash(hash.finalize().as_ref().try_into().unwrap())
    }

    fn consensus_branch_id(&self) -> ConsensusBranchId {
        self.network_upgrade.branch_id().expect(ZIP143_EXPLANATION)
    }

    /// Sighash implementation for the overwinter network upgrade
    pub(super) fn hash_sighash_zip143<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.hash_header(&mut writer)?;
        self.hash_groupid(&mut writer)?;
        self.hash_prevouts(&mut writer)?;
        self.hash_sequence(&mut writer)?;
        self.hash_outputs(&mut writer)?;
        self.hash_joinsplits(&mut writer)?;
        self.hash_lock_time(&mut writer)?;
        self.hash_expiry_height(&mut writer)?;
        self.hash_hash_type(&mut writer)?;
        self.hash_input(&mut writer)?;

        Ok(())
    }

    pub(super) fn personal(&self) -> [u8; 16] {
        let mut personal = [0; 16];
        (&mut personal[..12]).copy_from_slice(ZCASH_SIGHASH_PERSONALIZATION_PREFIX);
        (&mut personal[12..])
            .write_u32::<LittleEndian>(self.consensus_branch_id().into())
            .unwrap();
        personal
    }

    pub(super) fn hash_header<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let overwintered_flag = 1 << 31;

        writer.write_u32::<LittleEndian>(match &self.trans {
            Transaction::V1 { .. } | Transaction::V2 { .. } => unreachable!(ZIP143_EXPLANATION),
            Transaction::V3 { .. } => 3 | overwintered_flag,
            Transaction::V4 { .. } => 4 | overwintered_flag,
            Transaction::V5 { .. } => 5 | overwintered_flag,
        })
    }

    pub(super) fn hash_groupid<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(match &self.trans {
            Transaction::V1 { .. } | Transaction::V2 { .. } => unreachable!(ZIP143_EXPLANATION),
            Transaction::V3 { .. } => OVERWINTER_VERSION_GROUP_ID,
            Transaction::V4 { .. } => SAPLING_VERSION_GROUP_ID,
            Transaction::V5 { .. } => TX_V5_VERSION_GROUP_ID,
        })
    }

    pub(super) fn hash_prevouts<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.hash_type.contains(HashType::ANYONECANPAY) {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_PREVOUTS_HASH_PERSONALIZATION)
            .to_state();

        self.trans
            .inputs()
            .iter()
            .filter_map(|input| match input {
                transparent::Input::PrevOut { outpoint, .. } => Some(outpoint),
                transparent::Input::Coinbase { .. } => None,
            })
            .try_for_each(|outpoint| outpoint.zcash_serialize(&mut hash))?;

        writer.write_all(hash.finalize().as_ref())
    }

    pub(super) fn hash_sequence<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.hash_type.contains(HashType::ANYONECANPAY)
            || self.hash_type.masked() == HashType::SINGLE
            || self.hash_type.masked() == HashType::NONE
        {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_SEQUENCE_HASH_PERSONALIZATION)
            .to_state();

        self.trans
            .inputs()
            .iter()
            .map(|input| match input {
                transparent::Input::PrevOut { sequence, .. } => sequence,
                transparent::Input::Coinbase { sequence, .. } => sequence,
            })
            .try_for_each(|sequence| (&mut hash).write_u32::<LittleEndian>(*sequence))?;

        writer.write_all(hash.finalize().as_ref())
    }

    /// Writes the u256 hash of the transactions outputs to the provided Writer
    pub(super) fn hash_outputs<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.hash_type.masked() != HashType::SINGLE && self.hash_type.masked() != HashType::NONE
        {
            self.outputs_hash(writer)
        } else if self.hash_type.masked() == HashType::SINGLE
            && self
                .input
                .as_ref()
                .map(|&(_, _, index)| index)
                .map(|index| index < self.trans.outputs().len())
                .unwrap_or(false)
        {
            self.single_output_hash(writer)
        } else {
            writer.write_all(&[0; 32])
        }
    }

    fn outputs_hash<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_OUTPUTS_HASH_PERSONALIZATION)
            .to_state();

        self.trans
            .outputs()
            .iter()
            .try_for_each(|output| output.zcash_serialize(&mut hash))?;

        writer.write_all(hash.finalize().as_ref())
    }

    fn single_output_hash<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let &(_, _, output) = self
            .input
            .as_ref()
            .expect("already checked index is some in `hash_outputs`");

        let output = &self.trans.outputs()[output];

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_OUTPUTS_HASH_PERSONALIZATION)
            .to_state();

        output.zcash_serialize(&mut hash)?;

        writer.write_all(hash.finalize().as_ref())
    }

    pub(super) fn hash_joinsplits<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let has_joinsplits = match self.trans {
            Transaction::V1 { .. } | Transaction::V2 { .. } => unreachable!(ZIP143_EXPLANATION),
            Transaction::V3 { joinsplit_data, .. } => joinsplit_data.is_some(),
            Transaction::V4 { joinsplit_data, .. } => joinsplit_data.is_some(),
            Transaction::V5 { .. } => {
                unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244")
            }
        };

        if !has_joinsplits {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_JOINSPLITS_HASH_PERSONALIZATION)
            .to_state();

        // This code, and the check above for has_joinsplits cannot be combined
        // into a single branch because the `joinsplit_data` type of each
        // transaction kind has a different type.
        //
        // For v3 joinsplit_data is a JoinSplitData<Bctv14Proof>
        // For v4 joinsplit_data is a JoinSplitData<Groth16Proof>
        //
        // The type parameter on these types prevents them from being unified,
        // which forces us to duplicate the logic in each branch even though the
        // code within each branch is identical.
        //
        // TODO: use a generic function to remove the duplicate code
        match self.trans {
            Transaction::V3 {
                joinsplit_data: Some(jsd),
                ..
            } => {
                for joinsplit in jsd.joinsplits() {
                    joinsplit.zcash_serialize(&mut hash)?;
                }
                (&mut hash).write_all(&<[u8; 32]>::from(jsd.pub_key)[..])?;
            }
            Transaction::V4 {
                joinsplit_data: Some(jsd),
                ..
            } => {
                for joinsplit in jsd.joinsplits() {
                    joinsplit.zcash_serialize(&mut hash)?;
                }
                (&mut hash).write_all(&<[u8; 32]>::from(jsd.pub_key)[..])?;
            }
            Transaction::V5 { .. } => {
                unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244")
            }

            Transaction::V1 { .. }
            | Transaction::V2 { .. }
            | Transaction::V3 {
                joinsplit_data: None,
                ..
            }
            | Transaction::V4 {
                joinsplit_data: None,
                ..
            } => unreachable!("already checked transaction kind above"),
        };

        writer.write_all(hash.finalize().as_ref())
    }

    pub(super) fn hash_lock_time<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.trans.lock_time().zcash_serialize(&mut writer)
    }

    pub(super) fn hash_expiry_height<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.trans.expiry_height().expect(ZIP143_EXPLANATION).0)
    }

    pub(super) fn hash_hash_type<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.hash_type.bits())
    }

    pub(super) fn hash_input<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.input.is_some() {
            self.hash_input_prevout(&mut writer)?;
            self.hash_input_script_code(&mut writer)?;
            self.hash_input_amount(&mut writer)?;
            self.hash_input_sequence(&mut writer)?;
        }

        Ok(())
    }

    pub(super) fn hash_input_prevout<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let (_, input, _) = self
            .input
            .as_ref()
            .expect("caller verifies input is not none");

        let outpoint = match input {
            transparent::Input::PrevOut { outpoint, .. } => outpoint,
            transparent::Input::Coinbase { .. } => {
                unreachable!("sighash should only ever be called for valid Input types")
            }
        };

        outpoint.zcash_serialize(&mut writer)?;

        Ok(())
    }

    pub(super) fn hash_input_script_code<W: io::Write>(
        &self,
        mut writer: W,
    ) -> Result<(), io::Error> {
        let (transparent::Output { lock_script, .. }, _, _) = self
            .input
            .as_ref()
            .expect("caller verifies input is not none");

        lock_script.zcash_serialize(&mut writer)?;

        Ok(())
    }

    pub(super) fn hash_input_amount<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let (transparent::Output { value, .. }, _, _) = self
            .input
            .as_ref()
            .expect("caller verifies input is not none");

        writer.write_all(&value.to_bytes())?;

        Ok(())
    }

    pub(super) fn hash_input_sequence<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let (_, input, _) = self
            .input
            .as_ref()
            .expect("caller verifies input is not none");

        let sequence = match input {
            transparent::Input::PrevOut { sequence, .. } => sequence,
            transparent::Input::Coinbase { .. } => {
                unreachable!("sighash should only ever be called for valid Input types")
            }
        };

        writer.write_u32::<LittleEndian>(*sequence)?;

        Ok(())
    }

    // ********************
    // * ZIP243 ADDITIONS *
    // ********************

    /// Sighash implementation for the sapling network upgrade and every
    /// subsequent network upgrade
    pub(super) fn hash_sighash_zip243<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.hash_header(&mut writer)?;
        self.hash_groupid(&mut writer)?;
        self.hash_prevouts(&mut writer)?;
        self.hash_sequence(&mut writer)?;
        self.hash_outputs(&mut writer)?;
        self.hash_joinsplits(&mut writer)?;
        self.hash_shielded_spends(&mut writer)?;
        self.hash_shielded_outputs(&mut writer)?;
        self.hash_lock_time(&mut writer)?;
        self.hash_expiry_height(&mut writer)?;
        self.hash_value_balance(&mut writer)?;
        self.hash_hash_type(&mut writer)?;
        self.hash_input(&mut writer)?;

        Ok(())
    }

    pub(super) fn hash_shielded_spends<W: io::Write>(
        &self,
        mut writer: W,
    ) -> Result<(), io::Error> {
        use Transaction::*;

        let sapling_shielded_data = match self.trans {
            V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => sapling_shielded_data,
            V4 {
                sapling_shielded_data: None,
                ..
            } => return writer.write_all(&[0; 32]),
            V5 { .. } => unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244"),
            V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(ZIP243_EXPLANATION),
        };

        if sapling_shielded_data.spends().next().is_none() {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION)
            .to_state();

        // TODO: make a generic wrapper in `spends.rs` that does this serialization
        for spend in sapling_shielded_data.spends() {
            // This is the canonical transaction serialization, minus the `spendAuthSig`.
            spend.cv.zcash_serialize(&mut hash)?;
            // TODO: ZIP-243 Sapling to Canopy only
            hash.write_all(&spend.per_spend_anchor.0[..])?;
            hash.write_32_bytes(&spend.nullifier.into())?;
            hash.write_all(&<[u8; 32]>::from(spend.rk)[..])?;
            spend.zkproof.zcash_serialize(&mut hash)?;
        }

        writer.write_all(hash.finalize().as_ref())
    }

    pub(super) fn hash_shielded_outputs<W: io::Write>(
        &self,
        mut writer: W,
    ) -> Result<(), io::Error> {
        use Transaction::*;

        let sapling_shielded_data = match self.trans {
            V4 {
                sapling_shielded_data: Some(sapling_shielded_data),
                ..
            } => sapling_shielded_data,
            V4 {
                sapling_shielded_data: None,
                ..
            } => return writer.write_all(&[0; 32]),
            V5 { .. } => unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244"),
            V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(ZIP243_EXPLANATION),
        };

        if sapling_shielded_data.outputs().next().is_none() {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION)
            .to_state();

        // Correctness: checked for V4 transaction above
        for output in sapling_shielded_data
            .outputs()
            .cloned()
            .map(sapling::OutputInTransactionV4)
        {
            output.zcash_serialize(&mut hash)?;
        }

        writer.write_all(hash.finalize().as_ref())
    }

    pub(super) fn hash_value_balance<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        use crate::amount::Amount;
        use std::convert::TryFrom;
        use Transaction::*;

        let value_balance = match self.trans {
            V4 {
                sapling_shielded_data,
                ..
            } => match sapling_shielded_data {
                Some(s) => s.value_balance,
                None => Amount::try_from(0).unwrap(),
            },
            V5 { .. } => unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244"),
            V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(ZIP243_EXPLANATION),
        };

        writer.write_all(&value_balance.to_bytes())?;

        Ok(())
    }

    /// Compute a signature hash for V5 transactions according to ZIP-244.
    fn hash_sighash_zip244(&self) -> Hash {
        let input = self
            .input
            .as_ref()
            .map(|(output, input, idx)| (output, *input, *idx));
        sighash(&self.trans, self.hash_type, self.network_upgrade, input)
    }
}
