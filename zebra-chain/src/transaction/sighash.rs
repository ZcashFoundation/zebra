use super::Transaction;
use crate::{
    parameters::{
        ConsensusBranchId, NetworkUpgrade, OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID,
        TX_V5_VERSION_GROUP_ID,
    },
    serialization::{WriteZcashExt, ZcashSerialize},
    transparent,
};
use blake2b_simd::Hash;
use byteorder::{LittleEndian, WriteBytesExt};
use io::Write;
use std::io;

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
            Nu5 => unimplemented!(
                "Nu5 upgrade uses a new transaction digest algorithm, as specified in ZIP-244"
            ),
        }

        hash.finalize()
    }

    fn consensus_branch_id(&self) -> ConsensusBranchId {
        self.network_upgrade.branch_id().expect(ZIP143_EXPLANATION)
    }

    /// Sighash implementation for the overwinter network upgrade
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

    pub(super) fn personal(&self) -> [u8; 16] {
        let mut personal = [0; 16];
        (&mut personal[..12]).copy_from_slice(ZCASH_SIGHASH_PERSONALIZATION_PREFIX);
        (&mut personal[12..])
            .write_u32::<LittleEndian>(self.consensus_branch_id().into())
            .unwrap();
        personal
    }

    fn hash_header<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let overwintered_flag = 1 << 31;

        writer.write_u32::<LittleEndian>(match &self.trans {
            Transaction::V1 { .. } | Transaction::V2 { .. } => unreachable!(ZIP143_EXPLANATION),
            Transaction::V3 { .. } => 3 | overwintered_flag,
            Transaction::V4 { .. } => 4 | overwintered_flag,
            Transaction::V5 { .. } => 5 | overwintered_flag,
        })
    }

    fn hash_groupid<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(match &self.trans {
            Transaction::V1 { .. } | Transaction::V2 { .. } => unreachable!(ZIP143_EXPLANATION),
            Transaction::V3 { .. } => OVERWINTER_VERSION_GROUP_ID,
            Transaction::V4 { .. } => SAPLING_VERSION_GROUP_ID,
            Transaction::V5 { .. } => TX_V5_VERSION_GROUP_ID,
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

    fn hash_joinsplits<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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

    fn hash_lock_time<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.trans.lock_time().zcash_serialize(&mut writer)
    }

    fn hash_expiry_height<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.trans.expiry_height().expect(ZIP143_EXPLANATION).0)
    }

    fn hash_hash_type<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_u32::<LittleEndian>(self.hash_type.bits())
    }

    fn hash_input<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.input.is_some() {
            self.hash_input_prevout(&mut writer)?;
            self.hash_input_script_code(&mut writer)?;
            self.hash_input_amount(&mut writer)?;
            self.hash_input_sequence(&mut writer)?;
        }

        Ok(())
    }

    fn hash_input_prevout<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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

    fn hash_input_script_code<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let (transparent::Output { lock_script, .. }, _, _) = self
            .input
            .as_ref()
            .expect("caller verifies input is not none");

        writer.write_all(&lock_script.0)?;

        Ok(())
    }

    fn hash_input_amount<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        let (transparent::Output { value, .. }, _, _) = self
            .input
            .as_ref()
            .expect("caller verifies input is not none");

        writer.write_all(&value.to_bytes())?;

        Ok(())
    }

    fn hash_input_sequence<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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

    fn hash_shielded_spends<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        use Transaction::*;

        let shielded_data = match self.trans {
            V4 {
                sapling_shielded_data: Some(shielded_data),
                ..
            } => shielded_data,
            V4 {
                sapling_shielded_data: None,
                ..
            } => return writer.write_all(&[0; 32]),
            V5 { .. } => unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244"),
            V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(ZIP243_EXPLANATION),
        };

        if shielded_data.spends().next().is_none() {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION)
            .to_state();

        // TODO: make a generic wrapper in `spends.rs` that does this serialization
        for spend in shielded_data.spends() {
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

    fn hash_shielded_outputs<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        use Transaction::*;

        let shielded_data = match self.trans {
            V4 {
                sapling_shielded_data: Some(shielded_data),
                ..
            } => shielded_data,
            V4 {
                sapling_shielded_data: None,
                ..
            } => return writer.write_all(&[0; 32]),
            V5 { .. } => unimplemented!("v5 transaction hash as specified in ZIP-225 and ZIP-244"),
            V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(ZIP243_EXPLANATION),
        };

        if shielded_data.outputs().next().is_none() {
            return writer.write_all(&[0; 32]);
        }

        let mut hash = blake2b_simd::Params::new()
            .hash_length(32)
            .personal(ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION)
            .to_state();

        for output in shielded_data.outputs() {
            output.zcash_serialize(&mut hash)?;
        }

        writer.write_all(hash.finalize().as_ref())
    }

    fn hash_value_balance<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
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
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{amount::Amount, serialization::ZcashDeserializeInto, transaction::Transaction};
    use color_eyre::eyre;
    use eyre::Result;
    use transparent::Script;
    use zebra_test::vectors::{ZIP143_1, ZIP143_2, ZIP243_1, ZIP243_2, ZIP243_3};

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
                result.len = result.len(),
                hash_fn = stringify!($f)
            );
            let guard = span.enter();
            assert_eq!(expected, result);
            drop(guard);
        };
    }

    #[test]
    fn test_vec143_1() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP143_1.zcash_deserialize_into::<Transaction>()?;

        let hasher = SigHasher {
            trans: &transaction,
            hash_type: HashType::ALL,
            network_upgrade: NetworkUpgrade::Overwinter,
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

        assert_hash_eq!(
            "030000807082c403d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a0000000000000000000000000000000000000000000000000000000000000000481cdd86b3cc431801000000",
            hasher,
            hash_sighash_zip143
        );

        let hash = hasher.sighash();
        let expected = "a1f1a4e5cd9bd522322d661edd2af1bf2a7019cfab94ece18f4ba935b0a19073";
        let result = hex::encode(hash.as_bytes());
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_final",
            expected.len = expected.len(),
            buf.len = result.len()
        );
        let _guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }

    #[test]
    fn test_vec143_2() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP143_2.zcash_deserialize_into::<Transaction>()?;

        let value = hex::decode("2f6e04963b4c0100")?.zcash_deserialize_into::<Amount<_>>()?;
        let lock_script = Script(hex::decode("0153")?);
        let input_ind = 1;

        let hasher = SigHasher::new(
            &transaction,
            HashType::SINGLE,
            NetworkUpgrade::Overwinter,
            Some((input_ind, transparent::Output { value, lock_script })),
        );

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

        assert_hash_eq!(
            "378af1e40f64e125946f62c2fa7b2fecbcb64b6968912a6381ce3dc166d56a1d62f5a8d7",
            hasher,
            hash_input_prevout
        );

        assert_hash_eq!("0153", hasher, hash_input_script_code);

        assert_hash_eq!("2f6e04963b4c0100", hasher, hash_input_amount);

        assert_hash_eq!("e8c7203d", hasher, hash_input_sequence);

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
        let _guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }

    #[test]
    fn test_vec243_1() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP243_1.zcash_deserialize_into::<Transaction>()?;

        let hasher = SigHasher {
            trans: &transaction,
            hash_type: HashType::ALL,
            network_upgrade: NetworkUpgrade::Sapling,
            input: None,
        };

        assert_hash_eq!("04000080", hasher, hash_header);

        assert_hash_eq!("85202f89", hasher, hash_groupid);

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

        assert_hash_eq!(
            "3fd9edb96dccf5b9aeb71e3db3710e74be4f1dfb19234c1217af26181f494a36",
            hasher,
            hash_shielded_spends
        );

        assert_hash_eq!(
            "dafece799f638ba7268bf8fe43f02a5112f0bb32a84c4a8c2f508c41ff1c78b5",
            hasher,
            hash_shielded_outputs
        );

        assert_hash_eq!("481cdd86", hasher, hash_lock_time);

        assert_hash_eq!("b3cc4318", hasher, hash_expiry_height);

        assert_hash_eq!("442117623ceb0500", hasher, hash_value_balance);

        assert_hash_eq!("01000000", hasher, hash_hash_type);

        assert_hash_eq!(
            "0400008085202f89d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a00000000000000000000000000000000000000000000000000000000000000003fd9edb96dccf5b9aeb71e3db3710e74be4f1dfb19234c1217af26181f494a36dafece799f638ba7268bf8fe43f02a5112f0bb32a84c4a8c2f508c41ff1c78b5481cdd86b3cc4318442117623ceb050001000000",
            hasher,
            hash_sighash_zip243
        );

        let hash = hasher.sighash();
        let expected = "63d18534de5f2d1c9e169b73f9c783718adbef5c8a7d55b5e7a37affa1dd3ff3";
        let result = hex::encode(hash.as_bytes());
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_final",
            expected.len = expected.len(),
            buf.len = result.len()
        );
        let _guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }

    #[test]
    fn test_vec243_2() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP243_2.zcash_deserialize_into::<Transaction>()?;

        let value = hex::decode("adedf02996510200")?.zcash_deserialize_into::<Amount<_>>()?;
        let lock_script = Script(hex::decode("00")?);
        let input_ind = 1;

        let hasher = SigHasher::new(
            &transaction,
            HashType::NONE,
            NetworkUpgrade::Sapling,
            Some((input_ind, transparent::Output { value, lock_script })),
        );

        assert_hash_eq!("04000080", hasher, hash_header);

        assert_hash_eq!("85202f89", hasher, hash_groupid);

        assert_hash_eq!(
            "cacf0f5210cce5fa65a59f314292b3111d299e7d9d582753cf61e1e408552ae4",
            hasher,
            hash_prevouts
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_sequence
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_outputs
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_joinsplits
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_shielded_spends
        );

        assert_hash_eq!(
            "b79530fcec83211d21e3c355db538c138d625784c27370e9d1039a8515a23f87",
            hasher,
            hash_shielded_outputs
        );

        assert_hash_eq!("d7034302", hasher, hash_lock_time);

        assert_hash_eq!("011b9a07", hasher, hash_expiry_height);

        assert_hash_eq!("6620edc067ff0200", hasher, hash_value_balance);

        assert_hash_eq!("02000000", hasher, hash_hash_type);

        assert_hash_eq!(
            "090f47a068e227433f9e49d3aa09e356d8d66d0c0121e91a3c4aa3f27fa1b63396e2b41d",
            hasher,
            hash_input_prevout
        );

        assert_hash_eq!("00", hasher, hash_input_script_code);

        assert_hash_eq!("adedf02996510200", hasher, hash_input_amount);

        assert_hash_eq!("4e970568", hasher, hash_input_sequence);

        assert_hash_eq!(
            "0400008085202f89cacf0f5210cce5fa65a59f314292b3111d299e7d9d582753cf61e1e408552ae40000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000b79530fcec83211d21e3c355db538c138d625784c27370e9d1039a8515a23f87d7034302011b9a076620edc067ff020002000000090f47a068e227433f9e49d3aa09e356d8d66d0c0121e91a3c4aa3f27fa1b63396e2b41d00adedf029965102004e970568",
            hasher,
            hash_sighash_zip243
        );

        let hash = hasher.sighash();
        let expected = "bbe6d84f57c56b29b914c694baaccb891297e961de3eb46c68e3c89c47b1a1db";
        let result = hex::encode(hash.as_bytes());
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_final",
            expected.len = expected.len(),
            buf.len = result.len()
        );
        let _guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }

    #[test]
    fn test_vec243_3() -> Result<()> {
        zebra_test::init();

        let transaction = ZIP243_3.zcash_deserialize_into::<Transaction>()?;

        let value = hex::decode("80f0fa0200000000")?.zcash_deserialize_into::<Amount<_>>()?;
        let lock_script = Script(hex::decode(
            "1976a914507173527b4c3318a2aecd793bf1cfed705950cf88ac",
        )?);
        let input_ind = 0;

        let hasher = SigHasher::new(
            &transaction,
            HashType::ALL,
            NetworkUpgrade::Sapling,
            Some((input_ind, transparent::Output { value, lock_script })),
        );

        assert_hash_eq!("04000080", hasher, hash_header);

        assert_hash_eq!("85202f89", hasher, hash_groupid);

        assert_hash_eq!(
            "fae31b8dec7b0b77e2c8d6b6eb0e7e4e55abc6574c26dd44464d9408a8e33f11",
            hasher,
            hash_prevouts
        );

        assert_hash_eq!(
            "6c80d37f12d89b6f17ff198723e7db1247c4811d1a695d74d930f99e98418790",
            hasher,
            hash_sequence
        );

        assert_hash_eq!(
            "d2b04118469b7810a0d1cc59568320aad25a84f407ecac40b4f605a4e6868454",
            hasher,
            hash_outputs
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_joinsplits
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_shielded_spends
        );

        assert_hash_eq!(
            "0000000000000000000000000000000000000000000000000000000000000000",
            hasher,
            hash_shielded_outputs
        );

        assert_hash_eq!("29b00400", hasher, hash_lock_time);

        assert_hash_eq!("48b00400", hasher, hash_expiry_height);

        assert_hash_eq!("0000000000000000", hasher, hash_value_balance);

        assert_hash_eq!("01000000", hasher, hash_hash_type);

        assert_hash_eq!(
            "a8c685478265f4c14dada651969c45a65e1aeb8cd6791f2f5bb6a1d9952104d901000000",
            hasher,
            hash_input_prevout
        );

        assert_hash_eq!(
            "1976a914507173527b4c3318a2aecd793bf1cfed705950cf88ac",
            hasher,
            hash_input_script_code
        );

        assert_hash_eq!("80f0fa0200000000", hasher, hash_input_amount);

        assert_hash_eq!("feffffff", hasher, hash_input_sequence);

        assert_hash_eq!(
            "0400008085202f89fae31b8dec7b0b77e2c8d6b6eb0e7e4e55abc6574c26dd44464d9408a8e33f116c80d37f12d89b6f17ff198723e7db1247c4811d1a695d74d930f99e98418790d2b04118469b7810a0d1cc59568320aad25a84f407ecac40b4f605a4e686845400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000029b0040048b00400000000000000000001000000a8c685478265f4c14dada651969c45a65e1aeb8cd6791f2f5bb6a1d9952104d9010000001976a914507173527b4c3318a2aecd793bf1cfed705950cf88ac80f0fa0200000000feffffff",
            hasher,
            hash_sighash_zip243
        );

        let hash = hasher.sighash();
        let expected = "f3148f80dfab5e573d5edfe7a850f5fd39234f80b5429d3a57edcc11e34c585b";
        let result = hex::encode(hash.as_bytes());
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_final",
            expected.len = expected.len(),
            buf.len = result.len()
        );
        let _guard = span.enter();
        assert_eq!(expected, result);

        Ok(())
    }
}
