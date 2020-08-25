#![allow(dead_code, unused_variables)]
use super::Transaction;
use crate::{
    parameters::{ConsensusBranchId, NetworkUpgrade},
    serialization::ZcashSerialize,
    transparent,
};
use blake2b_simd::Hash;
use byteorder::{LittleEndian, WriteBytesExt};
use io::Write;
use std::io;

const OVERWINTER_VERSION_GROUP_ID: u32 = 0x03C4_8270;
const SAPLING_VERSION_GROUP_ID: u32 = 0x892F_2085;

const ZCASH_SIGHASH_PERSONALIZATION_PREFIX: &[u8; 12] = b"ZcashSigHash";
const ZCASH_PREVOUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashPrevoutHash";
const ZCASH_SEQUENCE_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSequencHash";
const ZCASH_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashOutputsHash";
const ZCASH_JOINSPLITS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashJSplitsHash";
const ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSSpendsHash";
const ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION: &[u8; 16] = b"ZcashSOutputHash";

bitflags::bitflags! {
    pub(super) struct HashType: u32 {
        const ALL = 0b0000_0001;
        const NONE = 0b0000_0010;
        const SINGLE = Self::ALL.bits | Self::NONE.bits;
        const ANYONECANPAY = 0b1000_0000;
    }
}

impl HashType {
    fn masked(self) -> Self {
        Self::from_bits_truncate(self.bits & 0b0001_1111)
    }
}

pub(super) struct SigHasher<'a> {
    pub(super) trans: &'a Transaction,
    pub(super) hash_type: HashType,
    pub(super) network_upgrade: NetworkUpgrade,
    pub(super) input: Option<(u32, transparent::Output)>,
}

impl<'a> SigHasher<'a> {
    pub fn sighash(self) -> Hash {
        use NetworkUpgrade::*;
        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(&self.personal())
            .to_state();

        match self.network_upgrade {
            Genesis => unreachable!("Zebra checkpoints on Sapling activation"),
            BeforeOverwinter => unreachable!("Zebra checkpoints on Sapling activation"),
            Overwinter => self
                .hash_sighash_zip143(&mut hash)
                .expect("serialization into hasher never fails"),
            Sapling | Blossom | Heartwood | Canopy => self
                .hash_sighash_zip243(&mut hash)
                .expect("serialization into hasher never fails"),
        }

        hash.finalize()
    }

    fn consensus_branch_id(&self) -> ConsensusBranchId {
        self.network_upgrade
            .branch_id()
            .expect("Zebra checkpoints on Sapling activation")
    }

    /// Sighash implementation for the overwinter consensus branch
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
        self.hash_input(&mut writer)?;

        Ok(())
    }

    /// Sighash implementation for the sapling consensus branch and every
    /// subsequent consensus branch
    fn hash_sighash_zip243<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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

    pub(super) fn personal(&self) -> [u8; 16] {
        let mut personal = [0; 16];
        (&mut personal[..12]).copy_from_slice(ZCASH_SIGHASH_PERSONALIZATION_PREFIX);
        (&mut personal[12..])
            .write_u32::<LittleEndian>(self.consensus_branch_id().into())
            .unwrap();
        personal
    }

    fn hash_header<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(match &self.trans {
            Transaction::V1 { .. } => 1,
            Transaction::V2 { .. } => 2,
            Transaction::V3 { .. } => 3 | 1 << 31,
            Transaction::V4 { .. } => 4 | 1 << 31,
        })
    }

    fn hash_groupid<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(match &self.trans {
            Transaction::V1 { .. } => unimplemented!("value to be determined"),
            Transaction::V2 { .. } => unimplemented!("value to be determined"),
            Transaction::V3 { .. } => OVERWINTER_VERSION_GROUP_ID,
            Transaction::V4 { .. } => SAPLING_VERSION_GROUP_ID,
        })
    }

    fn hash_prevouts<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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

    fn hash_sequence<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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
    fn hash_outputs<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.hash_type.masked() != HashType::SINGLE && self.hash_type.masked() != HashType::NONE
        {
            self.outputs_hash(writer)
        } else if self.hash_type.masked() == HashType::SINGLE
            && self
                .input
                .as_ref()
                .map(|(index, _)| (*index as usize) < self.trans.outputs().len())
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
        let (index, outpoint) = self
            .input
            .as_ref()
            .expect("already checked index is some in `hash_outputs`");

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_OUTPUTS_HASH_PERSONALIZATION)
            .to_state();

        self.trans.outputs()[*index as usize].zcash_serialize(&mut hash)?;

        writer.write_all(hash.finalize().as_ref())
    }

    fn hash_joinsplits<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let has_joinsplits = match self.trans {
            Transaction::V1 { .. } => false,
            Transaction::V2 { joinsplit_data, .. } | Transaction::V3 { joinsplit_data, .. } => {
                joinsplit_data.is_some()
            }
            Transaction::V4 { joinsplit_data, .. } => joinsplit_data.is_some(),
        };

        if !has_joinsplits {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_JOINSPLITS_HASH_PERSONALIZATION)
            .to_state();

        match self.trans {
            Transaction::V2 {
                joinsplit_data: Some(jsd),
                ..
            }
            | Transaction::V3 {
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
            _ => unreachable!("already checked for joinsplits"),
        };

        writer.write_all(hash.finalize().as_ref())
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
        writer.write_u32::<LittleEndian>(self.hash_type.bits())
    }

    fn hash_input<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let (index, transparent::Output { value, lock_script }) =
            if let Some(input) = self.input.as_ref() {
                input
            } else {
                return Ok(());
            };

        let (outpoint, unlock_script, sequence) = match &self.trans.inputs()[*index as usize] {
            transparent::Input::PrevOut {
                outpoint,
                unlock_script,
                sequence,
            } => (outpoint, unlock_script, sequence),
            transparent::Input::Coinbase { .. } => {
                unreachable!("sighash should only ever be called for valid Input types")
            }
        };

        outpoint.zcash_serialize(&mut writer)?;
        // todo: wtf is script code?
        writer.write_all(&value.to_bytes())?;
        writer.write_u32::<LittleEndian>(*sequence)?;

        Ok(())
    }

    fn hash_shielded_spends<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()
    }

    fn hash_shielded_outputs<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()
    }

    fn hash_value_balance<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        todo!()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::block;
    use crate::{
        parameters::Network, serialization::ZcashDeserializeInto, transaction::Transaction,
    };
    use color_eyre::eyre;
    use eyre::Result;
    use zebra_test::vectors::{ZIP143_1, ZIP143_2};

    macro_rules! assert_hash_eq {
        ($expected:literal, $hasher:expr, $f:ident) => {
            let mut buf = vec![];
            $hasher
                .$f(&mut buf)
                .expect("hashing into a vec never fails");
            let expected = $expected;
            let result = hex::encode(buf);
            let span = tracing::span!(
                tracing::Level::ERROR,
                "compare_vecs",
                expected.len = expected.len(),
                result.len = result.len()
            );
            let guard = span.enter();
            assert_eq!(expected, result);
            drop(guard);
        };
    }

    #[test]
    fn test_vec1() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP143_1.zcash_deserialize_into::<Transaction>()?;

        let hasher = SigHasher {
            trans: &transaction,
            hash_type: HashType::from_bits_truncate(1),
            network_upgrade: NetworkUpgrade::current(Network::Mainnet, block::Height(653600 - 1)),
            input: None,
        };

        assert_hash_eq!("03000080", hasher, hash_header);

        assert_hash_eq!("7082c403", hasher, hash_groupid);

        assert_hash_eq!(
            "d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136",
            hasher,
            hash_prevouts
        );

        assert_hash_eq!(
            "a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272",
            hasher,
            hash_sequence
        );

        assert_hash_eq!(
            "ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a",
            hasher,
            hash_outputs
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_joinsplits
        );

        assert_hash_eq!("481cdd86", hasher, hash_lock_time);

        assert_hash_eq!("b3cc4318", hasher, hash_expiry_height);

        assert_hash_eq!("01000000", hasher, hash_hash_type);

        // assert_hash_eq!(
        //     "030000807082c403d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272ec55f4afc6cebfe1c35bdcded7519ff6efb381ab1d5a8dd0060c13b2a512932b0000000000000000000000000000000000000000000000000000000000000000481cdd86b3cc431801000000",
        //     hasher,
        //     hash_sighash_zip143
        // );

        let hash = hasher.sighash();
        let expected = "a1f1a4e5cd9bd522322d661edd2af1bf2a7019cfab94ece18f4ba935b0a19073";
        let result = hex::encode(hash.as_bytes());
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_final",
            expected.len = expected.len(),
            buf.len = result.len()
        );
        let guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }

    #[test]
    fn test_vec2() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP143_2.zcash_deserialize_into::<Transaction>()?;

        let hasher = SigHasher {
            trans: &transaction,
            hash_type: HashType::from_bits_truncate(3),
            network_upgrade: NetworkUpgrade::current(Network::Mainnet, block::Height(653600 - 1)),
            // input: Some(1),  TODO: figure out the correct inputs
            input: None,
        };

        assert_hash_eq!("03000080", hasher, hash_header);

        assert_hash_eq!("7082c403", hasher, hash_groupid);

        assert_hash_eq!(
            "92b8af1f7e12cb8de105af154470a2ae0a11e64a24a514a562ff943ca0f35d7f",
            hasher,
            hash_prevouts
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_sequence
        );

        assert_hash_eq!(
            "edc32cce530f836f7c31c53656f859f514c3ff8dcae642d3e17700fdc6e829a4",
            hasher,
            hash_outputs
        );

        assert_hash_eq!(
            "f59e41b40f3a60be90bee2be11b0956dfff06a6d8e22668c4f215bd87b20d514",
            hasher,
            hash_joinsplits
        );

        assert_hash_eq!("97b0e4e4", hasher, hash_lock_time);

        assert_hash_eq!("c705fc05", hasher, hash_expiry_height);

        assert_hash_eq!("03000000", hasher, hash_hash_type);

        let hash = hasher.sighash();
        let expected = "23652e76cb13b85a0e3363bb5fca061fa791c40c533eccee899364e6e60bb4f7";
        let result = hash.as_bytes();
        let result = hex::encode(result);
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_final",
            expected.len = expected.len(),
            buf.len = result.len()
        );
        let guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }
}
