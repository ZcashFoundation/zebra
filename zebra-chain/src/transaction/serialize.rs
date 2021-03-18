//! Contains impls of `ZcashSerialize`, `ZcashDeserialize` for all of the
//! transaction types, so that all of the serialization logic is in one place.

use std::{io, sync::Arc};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::{
    block::MAX_BLOCK_BYTES,
    parameters::{OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID, TX_V5_VERSION_GROUP_ID},
    primitives::ZkSnarkProof,
    serialization::{
        ReadZcashExt, SafePreallocate, SerializationError, WriteZcashExt, ZcashDeserialize,
        ZcashDeserializeInto, ZcashSerialize,
    },
    sprout,
};

use super::*;

impl ZcashDeserialize for jubjub::Fq {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let possible_scalar = jubjub::Fq::from_bytes(&reader.read_32_bytes()?);

        if possible_scalar.is_some().into() {
            Ok(possible_scalar.unwrap())
        } else {
            Err(SerializationError::Parse(
                "Invalid jubjub::Fq, input not canonical",
            ))
        }
    }
}
impl<P: ZkSnarkProof> ZcashSerialize for JoinSplitData<P> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(self.joinsplits().count() as u64)?;
        for joinsplit in self.joinsplits() {
            joinsplit.zcash_serialize(&mut writer)?;
        }
        writer.write_all(&<[u8; 32]>::from(self.pub_key)[..])?;
        writer.write_all(&<[u8; 64]>::from(self.sig)[..])?;
        Ok(())
    }
}

impl<P: ZkSnarkProof> ZcashDeserialize for Option<JoinSplitData<P>> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let num_joinsplits = reader.read_compactsize()?;
        match num_joinsplits {
            0 => Ok(None),
            n => {
                let first = sprout::JoinSplit::zcash_deserialize(&mut reader)?;
                let mut rest = Vec::with_capacity((n - 1) as usize);
                for _ in 0..(n - 1) {
                    rest.push(sprout::JoinSplit::zcash_deserialize(&mut reader)?);
                }
                let pub_key = reader.read_32_bytes()?.into();
                let sig = reader.read_64_bytes()?.into();
                Ok(Some(JoinSplitData {
                    first,
                    rest,
                    pub_key,
                    sig,
                }))
            }
        }
    }
}

impl ZcashSerialize for Transaction {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        // Post-Sapling, transaction size is limited to MAX_BLOCK_BYTES.
        // (Strictly, the maximum transaction size is about 1.5 kB less,
        // because blocks also include a block header.)
        //
        // Currently, all transaction structs are parsed as part of a
        // block. So we don't need to check transaction size here, until
        // we start parsing mempool transactions, or generating our own
        // transactions (see #483).
        //
        // Since we checkpoint on Canopy activation, we won't ever need
        // to check the smaller pre-Sapling transaction size limit.
        match self {
            Transaction::V1 {
                inputs,
                outputs,
                lock_time,
            } => {
                writer.write_u32::<LittleEndian>(1)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
            }
            Transaction::V2 {
                inputs,
                outputs,
                lock_time,
                joinsplit_data,
            } => {
                writer.write_u32::<LittleEndian>(2)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                match joinsplit_data {
                    // Write 0 for nJoinSplits to signal no JoinSplitData.
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }
            }
            Transaction::V3 {
                inputs,
                outputs,
                lock_time,
                expiry_height,
                joinsplit_data,
            } => {
                // Write version 3 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(3 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(OVERWINTER_VERSION_GROUP_ID)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;
                match joinsplit_data {
                    // Write 0 for nJoinSplits to signal no JoinSplitData.
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }
            }
            Transaction::V4 {
                inputs,
                outputs,
                lock_time,
                expiry_height,
                value_balance,
                shielded_data,
                joinsplit_data,
            } => {
                // Write version 4 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(4 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(SAPLING_VERSION_GROUP_ID)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;
                value_balance.zcash_serialize(&mut writer)?;

                // The previous match arms serialize in one go, because the
                // internal structure happens to nicely line up with the
                // serialized structure. However, this is not possible for
                // version 4 transactions, as the binding_sig for the
                // ShieldedData is placed at the end of the transaction. So
                // instead we have to interleave serialization of the
                // ShieldedData and the JoinSplitData.

                match shielded_data {
                    None => {
                        // Signal no shielded spends and no shielded outputs.
                        writer.write_compactsize(0)?;
                        writer.write_compactsize(0)?;
                    }
                    Some(shielded_data) => {
                        writer.write_compactsize(shielded_data.spends().count() as u64)?;
                        for spend in shielded_data.spends() {
                            spend.zcash_serialize(&mut writer)?;
                        }
                        writer.write_compactsize(shielded_data.outputs().count() as u64)?;
                        for output in shielded_data.outputs() {
                            output.zcash_serialize(&mut writer)?;
                        }
                    }
                }

                match joinsplit_data {
                    None => writer.write_compactsize(0)?,
                    Some(jsd) => jsd.zcash_serialize(&mut writer)?,
                }

                match shielded_data {
                    Some(sd) => writer.write_all(&<[u8; 64]>::from(sd.binding_sig)[..])?,
                    None => {}
                }
            }
            Transaction::V5 {
                lock_time,
                expiry_height,
                inputs,
                outputs,
                rest,
            } => {
                // Write version 5 and set the fOverwintered bit.
                writer.write_u32::<LittleEndian>(5 | (1 << 31))?;
                writer.write_u32::<LittleEndian>(TX_V5_VERSION_GROUP_ID)?;
                lock_time.zcash_serialize(&mut writer)?;
                writer.write_u32::<LittleEndian>(expiry_height.0)?;
                inputs.zcash_serialize(&mut writer)?;
                outputs.zcash_serialize(&mut writer)?;
                // write the rest
                writer.write_all(rest)?;
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for Transaction {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let (version, overwintered) = {
            const LOW_31_BITS: u32 = (1 << 31) - 1;
            let header = reader.read_u32::<LittleEndian>()?;
            (header & LOW_31_BITS, header >> 31 != 0)
        };

        // The overwintered flag MUST NOT be set for version 1 and 2 transactions.
        match (version, overwintered) {
            (1, false) => Ok(Transaction::V1 {
                inputs: Vec::zcash_deserialize(&mut reader)?,
                outputs: Vec::zcash_deserialize(&mut reader)?,
                lock_time: LockTime::zcash_deserialize(&mut reader)?,
            }),
            (2, false) => {
                // Version 2 transactions use Sprout-on-BCTV14.
                type OptV2Jsd = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V2 {
                    inputs: Vec::zcash_deserialize(&mut reader)?,
                    outputs: Vec::zcash_deserialize(&mut reader)?,
                    lock_time: LockTime::zcash_deserialize(&mut reader)?,
                    joinsplit_data: OptV2Jsd::zcash_deserialize(&mut reader)?,
                })
            }
            (3, true) => {
                let id = reader.read_u32::<LittleEndian>()?;
                if id != OVERWINTER_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse(
                        "expected OVERWINTER_VERSION_GROUP_ID",
                    ));
                }
                // Version 3 transactions use Sprout-on-BCTV14.
                type OptV3Jsd = Option<JoinSplitData<Bctv14Proof>>;
                Ok(Transaction::V3 {
                    inputs: Vec::zcash_deserialize(&mut reader)?,
                    outputs: Vec::zcash_deserialize(&mut reader)?,
                    lock_time: LockTime::zcash_deserialize(&mut reader)?,
                    expiry_height: block::Height(reader.read_u32::<LittleEndian>()?),
                    joinsplit_data: OptV3Jsd::zcash_deserialize(&mut reader)?,
                })
            }
            (4, true) => {
                let id = reader.read_u32::<LittleEndian>()?;
                if id != SAPLING_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse(
                        "expected SAPLING_VERSION_GROUP_ID",
                    ));
                }
                // Version 4 transactions use Sprout-on-Groth16.
                type OptV4Jsd = Option<JoinSplitData<Groth16Proof>>;

                // The previous match arms deserialize in one go, because the
                // internal structure happens to nicely line up with the
                // serialized structure. However, this is not possible for
                // version 4 transactions, as the binding_sig for the
                // ShieldedData is placed at the end of the transaction. So
                // instead we have to pull the component parts out manually and
                // then assemble them.

                let inputs = Vec::zcash_deserialize(&mut reader)?;
                let outputs = Vec::zcash_deserialize(&mut reader)?;
                let lock_time = LockTime::zcash_deserialize(&mut reader)?;
                let expiry_height = block::Height(reader.read_u32::<LittleEndian>()?);
                let value_balance = (&mut reader).zcash_deserialize_into()?;
                let mut shielded_spends = Vec::zcash_deserialize(&mut reader)?;
                let mut shielded_outputs = Vec::zcash_deserialize(&mut reader)?;
                let joinsplit_data = OptV4Jsd::zcash_deserialize(&mut reader)?;

                use futures::future::Either::*;
                let shielded_data = if !shielded_spends.is_empty() {
                    Some(ShieldedData {
                        first: Left(shielded_spends.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: reader.read_64_bytes()?.into(),
                    })
                } else if !shielded_outputs.is_empty() {
                    Some(ShieldedData {
                        first: Right(shielded_outputs.remove(0)),
                        rest_spends: shielded_spends,
                        rest_outputs: shielded_outputs,
                        binding_sig: reader.read_64_bytes()?.into(),
                    })
                } else {
                    None
                };

                Ok(Transaction::V4 {
                    inputs,
                    outputs,
                    lock_time,
                    expiry_height,
                    value_balance,
                    shielded_data,
                    joinsplit_data,
                })
            }
            (5, false) => {
                let id = reader.read_u32::<LittleEndian>()?;
                if id != TX_V5_VERSION_GROUP_ID {
                    return Err(SerializationError::Parse("expected TX_V5_VERSION_GROUP_ID"));
                }
                let lock_time = LockTime::zcash_deserialize(&mut reader)?;
                let expiry_height = block::Height(reader.read_u32::<LittleEndian>()?);
                let inputs = Vec::zcash_deserialize(&mut reader)?;
                let outputs = Vec::zcash_deserialize(&mut reader)?;
                let mut rest = Vec::new();
                reader.read_to_end(&mut rest)?;

                Ok(Transaction::V5 {
                    lock_time,
                    expiry_height,
                    inputs,
                    outputs,
                    rest,
                })
            }
            (_, _) => Err(SerializationError::Parse("bad tx header")),
        }
    }
}

impl<T> ZcashDeserialize for Arc<T>
where
    T: ZcashDeserialize,
{
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        Ok(Arc::new(T::zcash_deserialize(reader)?))
    }
}

impl<T> ZcashSerialize for Arc<T>
where
    T: ZcashSerialize,
{
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error> {
        T::zcash_serialize(self, writer)
    }
}

/// A Tx Input must have an Outpoint (32 byte hash + 4 byte index), a 4 byte sequence number,
/// and a signature script, which always takes a min of 1 byte (for a length 0 script)
const MIN_TRANSPARENT_INPUT_SIZE: u64 = 32 + 4 + 4 + 1;
/// A Transparent output has an 8 byte value and script which takes a min of 1 byte
const MIN_TRANSPARENT_OUTPUT_SIZE: u64 = 8 + 1;
// All txs must have at least one input and a 4 byte locktime
const MIN_TRANSPARENT_TX_SIZE: u64 = MIN_TRANSPARENT_INPUT_SIZE + 4;

/// No valid Zcash message contains more transactions than can fit in a single block
impl SafePreallocate for Arc<Transaction> {
    fn max_allocation() -> u64 {
        MAX_BLOCK_BYTES / MIN_TRANSPARENT_TX_SIZE
    }
}
/// No valid Zcash message contains more transactions inputs than can fit in maximally large transaction
impl SafePreallocate for transparent::Input {
    fn max_allocation() -> u64 {
        MAX_BLOCK_BYTES / MIN_TRANSPARENT_INPUT_SIZE
    }
}
/// No valid Zcash message contains more transactions outputs than can fit in maximally large transaction
impl SafePreallocate for transparent::Output {
    fn max_allocation() -> u64 {
        MAX_BLOCK_BYTES / MIN_TRANSPARENT_OUTPUT_SIZE
    }
}

#[cfg(test)]
mod test_safe_preallocate {
    use super::{
        transparent::Input, transparent::Output, Transaction, MAX_BLOCK_BYTES,
        MIN_TRANSPARENT_INPUT_SIZE, MIN_TRANSPARENT_OUTPUT_SIZE, MIN_TRANSPARENT_TX_SIZE,
    };
    use crate::serialization::{SafePreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::{convert::TryInto, sync::Arc};
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        /// Confirm that each spend takes at least MIN_TRANSPARENT_TX_SIZE bytes when serialized.
        /// This verifies that our calculated `SafePreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn tx_size_is_small_enough(tx in Transaction::arbitrary()) {
            let serialized = tx.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 >= MIN_TRANSPARENT_TX_SIZE)
        }

        /// Confirm that each spend takes at least MIN_TRANSPARENT_TX_SIZE bytes when serialized.
        /// This verifies that our calculated `SafePreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn transparent_input_size_is_small_enough(input in Input::arbitrary()) {
            let serialized = input.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 >= MIN_TRANSPARENT_INPUT_SIZE)
        }

        /// Confirm that each spend takes at least MIN_TRANSPARENT_TX_SIZE bytes when serialized.
        /// This verifies that our calculated `SafePreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn transparent_output_size_is_small_enough(output in Output::arbitrary()) {
            let serialized = output.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 >= MIN_TRANSPARENT_OUTPUT_SIZE)
        }

    }
    proptest! {
        // This test is pretty slow, so only run a few
        #![proptest_config(ProptestConfig::with_cases(7))]
        #[test]
        fn tx_max_allocation_is_big_enough(tx in Transaction::arbitrary()) {

            let max_allocation: usize = <Arc<Transaction>>::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(<Arc<Transaction>>::max_allocation()+1) {
                smallest_disallowed_vec.push(Arc::new(tx.clone()));
            }
            let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == <Arc<Transaction>>::max_allocation());
            // Check that our smallest_disallowed_vec is too big to be included in a valid block
            prop_assert!(serialized.len() as u64 >= MAX_BLOCK_BYTES);
        }

        #[test]
        fn input_max_allocation_is_big_enough(input in Input::arbitrary()) {

            let max_allocation: usize = Input::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Input::max_allocation()+1) {
                smallest_disallowed_vec.push(input.clone());
            }
            let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Input::max_allocation());
            // Check that our smallest_disallowed_vec is too big to be included in a valid block
            prop_assert!(serialized.len() as u64 >= MAX_BLOCK_BYTES);
        }
        #[test]
        fn output_max_allocation_is_big_enough(output in Output::arbitrary()) {

            let max_allocation: usize = Output::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Output::max_allocation()+1) {
                smallest_disallowed_vec.push(output.clone());
            }
            let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Output::max_allocation());
            // Check that our smallest_disallowed_vec is too big to be included in a valid block
            prop_assert!(serialized.len() as u64 >= MAX_BLOCK_BYTES);
        }
    }
}
