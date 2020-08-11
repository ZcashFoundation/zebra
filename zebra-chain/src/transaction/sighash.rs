#![allow(dead_code)]
use super::{Transaction, TransparentInput};
use crate::{
    parameters::ConsensusBranchId, serialization::ZcashSerialize, types::BlockHeight, Network,
    NetworkUpgrade,
};
use blake2b_simd::Hash;
use byteorder::{LittleEndian, WriteBytesExt};

const ZCASH_SIGHASH_PERSONALIZATION_PREFIX: &[u8; 12] = b"ZcashSigHash";
const ZCASH_PREVOUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashPrevoutHash";
const ZCASH_SEQUENCE_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSequencHash";
const ZCASH_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashOutputsHash";
const ZCASH_JOINSPLITS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashJSplitsHash";
const ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSSpendsHash";
const ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSOutputHash";

pub const SIGHASH_ALL: u32 = 1;
const SIGHASH_NONE: u32 = 2;
const SIGHASH_SINGLE: u32 = 3;
const SIGHASH_MASK: u32 = 0x1f;
const SIGHASH_ANYONECANPAY: u32 = 0x80;

pub struct SigHasher<'a> {
    pub(crate) trans: &'a Transaction,
    pub(crate) hash_type: u32,
    pub(crate) network: Network,
    pub(crate) height: BlockHeight,
}

impl<'a> SigHasher<'a> {
    pub fn sighash(self) -> Hash {
        match self.network_upgrade() {
            NetworkUpgrade::Genesis => unimplemented!(),
            NetworkUpgrade::BeforeOverwinter => unimplemented!(),
            NetworkUpgrade::Overwinter | NetworkUpgrade::Sapling => self.sighash_zip143(),
            NetworkUpgrade::Blossom => unimplemented!(),
            NetworkUpgrade::Heartwood => unimplemented!(),
            NetworkUpgrade::Canopy => unimplemented!(),
        }
    }

    fn network_upgrade(&self) -> NetworkUpgrade {
        NetworkUpgrade::current(self.network, self.height)
    }

    fn consensus_branch_id(&self) -> ConsensusBranchId {
        self.network_upgrade().branch_id().unwrap()
    }

    /// Sighash implementation for the overwinter and sapling consensus branches
    fn sighash_zip143(&self) -> Hash {
        let mut personal = [0; 16];
        (&mut personal[..12]).copy_from_slice(ZCASH_SIGHASH_PERSONALIZATION_PREFIX);
        (&mut personal[12..])
            .write_u32::<LittleEndian>(self.consensus_branch_id().into())
            .unwrap();

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(&personal)
            .to_state();

        hash.write_u32::<LittleEndian>(self.trans.header())
            .expect("write to hasher will never fail");

        hash.write_u32::<LittleEndian>(self.trans.group_id().expect("fOverwintered is always set"))
            .expect("write to hasher will never fail");

        hash.update(
            self.hash_prevouts()
                .as_ref()
                .map(|h| h.as_ref())
                .unwrap_or(&[0; 32]),
        );
        hash.update(
            self.hash_sequence()
                .as_ref()
                .map(|h| h.as_ref())
                .unwrap_or(&[0; 32]),
        );
        hash.update(
            self.hash_outputs()
                .as_ref()
                .map(|h| h.as_ref())
                .unwrap_or(&[0; 32]),
        );

        // update_hash!(h, !tx.joinsplits.is_empty(), joinsplits_hash(tx));
        // if sigversion == SigHashVersion::Sapling {
        //     update_hash!(h, !tx.shielded_spends.is_empty(), shielded_spends_hash(tx));
        //     update_hash!(
        //         h,
        //         !tx.shielded_outputs.is_empty(),
        //         shielded_outputs_hash(tx)
        //     );
        // }

        self.trans
            .lock_time()
            .zcash_serialize(&mut hash)
            .expect("write to hasher will never fail");

        hash.write_u32::<LittleEndian>(
            self.trans
                .expiry_height()
                .expect("fOverwintered is always set")
                .0,
        )
        .expect("write to hasher will never fail");

        // if sigversion == SigHashVersion::Sapling {
        //     h.update(&tx.value_balance.to_i64_le_bytes());
        // }
        // update_u32!(h, hash_type, tmp);

        // if let Some((n, script_code, amount)) = transparent_input {
        //     let mut data = vec![];
        //     tx.vin[n].prevout.write(&mut data).unwrap();
        //     script_code.write(&mut data).unwrap();
        //     data.extend_from_slice(&amount.to_i64_le_bytes());
        //     (&mut data)
        //         .write_u32::<LittleEndian>(tx.vin[n].sequence)
        //         .unwrap();
        //     h.update(&data);
        // }

        hash.finalize()
    }

    fn hash_prevouts(&self) -> Option<Hash> {
        if self.hash_type & SIGHASH_ANYONECANPAY == 0 {
            return None;
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_PREVOUTS_HASH_PERSONALIZATION)
            .to_state();

        let mut buf = vec![];

        self.trans
            .inputs()
            .filter_map(|input| match input {
                TransparentInput::PrevOut { outpoint, .. } => Some(outpoint),
                TransparentInput::Coinbase { .. } => None,
            })
            .try_for_each(|outpoint| outpoint.zcash_serialize(&mut buf))
            .expect("serialization into vec is infallible");

        hash.update(&buf);

        Some(hash.finalize())
    }

    fn hash_sequence(&self) -> Option<Hash> {
        if self.hash_type & SIGHASH_ANYONECANPAY == 0
            && (self.hash_type & SIGHASH_MASK) != SIGHASH_SINGLE
            && (self.hash_type & SIGHASH_MASK) != SIGHASH_NONE
        {
            return None;
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_PREVOUTS_HASH_PERSONALIZATION)
            .to_state();

        let mut buf = vec![];

        self.trans
            .inputs()
            .map(|input| match input {
                TransparentInput::PrevOut { sequence, .. } => sequence,
                TransparentInput::Coinbase { sequence, .. } => sequence,
            })
            .try_for_each(|sequence| (&mut buf).write_u32::<LittleEndian>(*sequence))
            .expect("serialization into vec is infallible");

        hash.update(&buf);

        Some(hash.finalize())
    }

    fn hash_outputs(&self) -> Option<Hash> {
        if (self.hash_type & SIGHASH_MASK) != SIGHASH_SINGLE
            && (self.hash_type & SIGHASH_MASK) != SIGHASH_NONE
        {
            Some(self.outputs_hash())
        } else if (self.hash_type & SIGHASH_MASK) == SIGHASH_SINGLE
            && transparent_input.is_some()
            && transparent_input.as_ref().unwrap().0 < tx.vout.len()
        {
            Some(self.single_output_hash())
        } else {
            None
        }
    }

    fn outputs_hash(&self) -> Hash {
        todo!()
    }

    fn single_output_hash(&self) -> Hash {
        todo!()
    }
}
