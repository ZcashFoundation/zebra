#![allow(dead_code, unused_variables)]
use super::{Transaction, TransparentInput};
use crate::{
    parameters::ConsensusBranchId, serialization::ZcashSerialize, types::BlockHeight, Network,
    NetworkUpgrade,
};
use blake2b_simd::Hash;
use byteorder::{LittleEndian, WriteBytesExt};
use std::io;

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
        use NetworkUpgrade::*;
        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(&self.personal())
            .to_state();

        match self.network_upgrade() {
            Genesis => unimplemented!(),
            BeforeOverwinter => unimplemented!(),
            Overwinter | Sapling => self
                .hash_sighash_zip143(&mut hash)
                .expect("serialization into hasher never fails"),
            Blossom => unimplemented!(),
            Heartwood => unimplemented!(),
            Canopy => unimplemented!(),
        }

        hash.finalize()
    }

    fn network_upgrade(&self) -> NetworkUpgrade {
        NetworkUpgrade::current(self.network, self.height)
    }

    fn consensus_branch_id(&self) -> ConsensusBranchId {
        self.network_upgrade().branch_id().expect(
            "Pre-Overwinter blocks with V3 or later transactions have already been rejected",
        )
    }

    /// Sighash implementation for the overwinter and sapling consensus branches
    fn hash_sighash_zip143<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.hash_header(&mut writer)?;
        self.hash_groupid(&mut writer)?;
        self.hash_prevouts(&mut writer)?;
        self.hash_sequence(&mut writer)?;
        self.hash_outputs(&mut writer)?;
        self.hash_joinsplits(&mut writer)?;
        self.hash_lock_time(&mut writer)?;
        self.hash_expiry_height(&mut writer)?;
        self.hash_hash_type(&mut writer)?;

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

    fn hash_header<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.trans.header())
    }

    fn hash_groupid<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.trans.group_id().expect("Transaction is V3 or later"))
    }

    fn hash_prevouts<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.hash_type & SIGHASH_ANYONECANPAY != 0 {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_PREVOUTS_HASH_PERSONALIZATION)
            .to_state();

        let mut buf = vec![];

        self.trans
            .inputs()
            .iter()
            .filter_map(|input| match input {
                TransparentInput::PrevOut { outpoint, .. } => Some(outpoint),
                TransparentInput::Coinbase { .. } => None,
            })
            .try_for_each(|outpoint| outpoint.zcash_serialize(&mut buf))?;

        hash.update(&buf);

        writer.write_all(hash.finalize().as_ref())
    }

    fn hash_sequence<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.hash_type & SIGHASH_ANYONECANPAY != 0
            || (self.hash_type & SIGHASH_MASK) == SIGHASH_SINGLE
            || (self.hash_type & SIGHASH_MASK) == SIGHASH_NONE
        {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_SEQUENCE_HASH_PERSONALIZATION)
            .to_state();

        let mut buf = vec![];

        self.trans
            .inputs()
            .iter()
            .map(|input| match input {
                TransparentInput::PrevOut { sequence, .. } => sequence,
                TransparentInput::Coinbase { sequence, .. } => sequence,
            })
            .try_for_each(|sequence| (&mut buf).write_u32::<LittleEndian>(*sequence))?;

        hash.update(&buf);

        writer.write_all(hash.finalize().as_ref())
    }

    /// Writes the u256 hash of the transactions outputs to the provided Writer
    fn hash_outputs<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()
        // if (self.hash_type & SIGHASH_MASK) == SIGHASH_SINGLE
        //     || (self.hash_type & SIGHASH_MASK) == SIGHASH_NONE
        // {
        //     self.outputs_hash(writer)
        // } else if (self.hash_type & SIGHASH_MASK) == SIGHASH_SINGLE
        // //     && transparent_input.is_some()
        // //     && transparent_input.as_ref().unwrap().0 < tx.vout.len()
        // {
        //     self.single_output_hash(writer)
        // } else {
        //     writer.write_all(&[0; 32])
        // }
    }

    fn outputs_hash<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()
    }

    fn single_output_hash<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()
    }

    fn hash_joinsplits<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()

        // if !tx.joinsplits.is_empty() {
        //     writer
        //         .write_all(&[0; 32])
        //         .expect("write to hasher will never fail");
        // }

        // todo!()
    }

    fn hash_lock_time<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.trans.lock_time().zcash_serialize(&mut writer)
    }

    fn hash_expiry_height<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(
            self.trans
                .expiry_height()
                .expect("Transaction is V3 or later")
                .0,
        )
    }

    fn hash_hash_type<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.hash_type)
    }
}

#[cfg(test)]
mod test {
    use super::SigHasher;
    use crate::{
        serialization::ZcashDeserializeInto, transaction::Transaction, types::BlockHeight, Network,
    };
    use color_eyre::eyre;
    use eyre::Result;
    use zebra_test::vectors::ZIP143_1;

    macro_rules! assert_hash_eq {
        ($expected:literal, $hasher:expr, $f:ident) => {
            let mut buf = vec![];
            $hasher
                .$f(&mut buf)
                .expect("hashing into a vec never fails");
            assert_eq!(hex::decode($expected.as_ref()).unwrap(), buf);
        };
    }

    #[test]
    fn test_vec1() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP143_1.zcash_deserialize_into::<Transaction>()?;

        let hasher = SigHasher {
            trans: &transaction,
            hash_type: 1,
            network: Network::Mainnet,
            height: BlockHeight(653600 - 1),
        };

        assert_hash_eq!(b"03000080", hasher, hash_header);

        assert_hash_eq!(b"7082c403", hasher, hash_groupid);

        assert_hash_eq!(
            b"d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136",
            hasher,
            hash_prevouts
        );

        assert_hash_eq!(
            b"a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272",
            hasher,
            hash_sequence
        );

        // assert_hash_eq!(
        //     b"ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a",
        //     hasher,
        //     hash_outputs
        // );

        // assert_hash_eq!(
        //     b"0000000000000000000000000000000000000000000000000000000000000000",
        //     hasher,
        //     hash_joinsplits
        // );

        assert_hash_eq!(b"481cdd86", hasher, hash_lock_time);

        assert_hash_eq!(b"b3cc4318", hasher, hash_expiry_height);

        assert_hash_eq!(b"01000000", hasher, hash_hash_type);

        // assert_hash_eq!(
        //     b"a1f1a4e5cd9bd522322d661edd2af1bf2a7019cfab94ece18f4ba935b0a19073",
        //     hasher,
        //     hash_sighash_zip143
        // );

        Ok(())
    }
}
