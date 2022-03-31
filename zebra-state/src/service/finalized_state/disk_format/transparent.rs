//! Transparent transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use zebra_chain::{
    amount::{self, Amount, NonNegative},
    block::Height,
    parameters::Network::*,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::{self, Address::*},
};

use crate::service::finalized_state::disk_format::{
    block::{TransactionIndex, TransactionLocation, TRANSACTION_LOCATION_DISK_BYTES},
    expand_zero_be_bytes, truncate_zero_be_bytes, FromDisk, IntoDisk,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
#[cfg(any(test, feature = "proptest-impl"))]
use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

/// Transparent balances are stored as an 8 byte integer on disk.
pub const BALANCE_DISK_BYTES: usize = 8;

/// [`OutputIndex`]es are stored as 3 bytes on disk.
///
/// This reduces database size and increases lookup performance.
pub const OUTPUT_INDEX_DISK_BYTES: usize = 3;

/// [`OutputLocation`]s are stored as a 3 byte height, 2 byte transaction index,
/// and 3 byte output index on disk.
///
/// This reduces database size and increases lookup performance.
pub const OUTPUT_LOCATION_DISK_BYTES: usize =
    TRANSACTION_LOCATION_DISK_BYTES + OUTPUT_INDEX_DISK_BYTES;

// Transparent types

/// A transparent output's index in its transaction.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Serialize, Deserialize))]
pub struct OutputIndex(u32);

impl OutputIndex {
    /// Create a transparent output index from the Zcash consensus integer type.
    ///
    /// `u32` is also the inner type.
    pub fn from_index(output_index: u32) -> OutputIndex {
        OutputIndex(output_index)
    }

    /// Returns this index as the inner type.
    pub fn index(&self) -> u32 {
        self.0
    }

    /// Create a transparent output index from `usize`.
    #[allow(dead_code)]
    pub fn from_usize(output_index: usize) -> OutputIndex {
        OutputIndex(
            output_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Return this index as `usize`.
    #[allow(dead_code)]
    pub fn as_usize(&self) -> usize {
        self.0
            .try_into()
            .expect("the maximum valid index fits in usize")
    }

    /// Create a transparent output index from `u64`.
    #[allow(dead_code)]
    pub fn from_u64(output_index: u64) -> OutputIndex {
        OutputIndex(
            output_index
                .try_into()
                .expect("the maximum u64 index fits in the inner type"),
        )
    }

    /// Return this index as `u64`.
    #[allow(dead_code)]
    pub fn as_u64(&self) -> u64 {
        self.0.into()
    }
}

/// A transparent output's location in the chain, by block height and transaction index.
///
/// [`OutputLocation`]s are sorted in increasing chain order, by height, transaction index,
/// and output index.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Serialize, Deserialize)
)]
pub struct OutputLocation {
    /// The location of the transparent input's transaction.
    transaction_location: TransactionLocation,

    /// The index of the transparent output in its transaction.
    output_index: OutputIndex,
}

impl OutputLocation {
    /// Creates an output location from a block height, and `usize` transaction and output indexes.
    #[allow(dead_code)]
    pub fn from_usize(
        height: Height,
        transaction_index: usize,
        output_index: usize,
    ) -> OutputLocation {
        OutputLocation {
            transaction_location: TransactionLocation::from_usize(height, transaction_index),
            output_index: OutputIndex::from_usize(output_index),
        }
    }

    /// Creates an output location from an [`Outpoint`],
    /// and the [`TransactionLocation`] of its transaction.
    ///
    /// The [`TransactionLocation`] is provided separately,
    /// because the lookup is a database operation.
    pub fn from_outpoint(
        transaction_location: TransactionLocation,
        outpoint: &transparent::OutPoint,
    ) -> OutputLocation {
        OutputLocation::from_output_index(transaction_location, outpoint.index)
    }

    /// Creates an output location from a [`TransactionLocation`] and a `u32` output index.
    ///
    /// Output indexes are serialized to `u32` in the Zcash consensus-critical transaction format.
    pub fn from_output_index(
        transaction_location: TransactionLocation,
        output_index: u32,
    ) -> OutputLocation {
        OutputLocation {
            transaction_location,
            output_index: OutputIndex::from_index(output_index),
        }
    }

    /// Returns the height of this [`transparent::Output`].
    pub fn height(&self) -> Height {
        self.transaction_location.height
    }

    /// Returns the transaction index of this [`transparent::Output`].
    pub fn transaction_index(&self) -> TransactionIndex {
        self.transaction_location.index
    }

    /// Returns the output index of this [`transparent::Output`].
    pub fn output_index(&self) -> OutputIndex {
        self.output_index
    }

    /// Returns the location of the transaction for this [`transparent::Output`].
    pub fn transaction_location(&self) -> TransactionLocation {
        self.transaction_location
    }

    /// Allows tests to set the height of this output location.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn height_mut(&mut self) -> &mut Height {
        &mut self.transaction_location.height
    }
}

/// The location of the first [`transparent::Output`] sent to an address.
///
/// The address location stays the same, even if the corresponding output
/// has been spent.
///
/// The first output location is used to represent the address in the database,
/// because output locations are significantly smaller than addresses.
///
/// TODO: make this a different type to OutputLocation?
///       derive IntoDisk and FromDisk?
pub type AddressLocation = OutputLocation;

/// Data which Zebra indexes for each [`transparent::Address`].
///
/// Currently, Zebra tracks this data 1:1 for each address:
/// - the balance [`Amount`] for a transparent address, and
/// - the [`AddressLocation`] for the first [`transparent::Output`] sent to that address
///   (regardless of whether that output is spent or unspent).
///
/// All other address data is tracked multiple times for each address
/// (UTXOs and transactions).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Serialize, Deserialize)
)]
pub struct AddressBalanceLocation {
    /// The total balance of all UTXOs sent to an address.
    balance: Amount<NonNegative>,

    /// The location of the first [`transparent::Output`] sent to an address.
    location: AddressLocation,
}

impl AddressBalanceLocation {
    /// Creates a new [`AddressBalanceLocation`] from the location of
    /// the first [`transparent::Output`] sent to an address.
    ///
    /// The returned value has a zero initial balance.
    pub fn new(first_output: OutputLocation) -> AddressBalanceLocation {
        AddressBalanceLocation {
            balance: Amount::zero(),
            location: first_output,
        }
    }

    /// Returns the current balance for the address.
    pub fn balance(&self) -> Amount<NonNegative> {
        self.balance
    }

    /// Returns a mutable reference to the current balance for the address.
    pub fn balance_mut(&mut self) -> &mut Amount<NonNegative> {
        &mut self.balance
    }

    /// Updates the current balance by adding the supplied output's value.
    pub fn receive_output(
        &mut self,
        unspent_output: &transparent::Output,
    ) -> Result<(), amount::Error> {
        self.balance = (self.balance + unspent_output.value())?;

        Ok(())
    }

    /// Updates the current balance by subtracting the supplied output's value.
    pub fn spend_output(
        &mut self,
        spent_output: &transparent::Output,
    ) -> Result<(), amount::Error> {
        self.balance = (self.balance - spent_output.value())?;

        Ok(())
    }

    /// Returns the location of the first [`transparent::Output`] sent to an address.
    pub fn address_location(&self) -> AddressLocation {
        self.location
    }

    /// Allows tests to set the height of the address location.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn height_mut(&mut self) -> &mut Height {
        &mut self.location.transaction_location.height
    }
}

/// A single unspent output for a [`transparent::Address`].
///
/// We store both the address location key and unspend output location value
/// in the RocksDB column family key. This improves insert and delete performance.
///
/// This requires 8 extra bytes for each unspent output,
/// because we repeat the key for each value.
/// But RocksDB compression reduces the duplicate data size on disk.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Serialize, Deserialize)
)]
pub struct AddressUnspentOutput {
    /// The location of the first [`transparent::Output`] sent to the address in `output`.
    address_location: AddressLocation,

    /// The location of this unspent output.
    unspent_output_location: OutputLocation,
}

impl AddressUnspentOutput {
    /// Create a new [`AddressUnspentOutput`] from an address location,
    /// and an unspent output location.
    pub fn new(
        address_location: AddressLocation,
        unspent_output_location: OutputLocation,
    ) -> AddressUnspentOutput {
        AddressUnspentOutput {
            address_location,
            unspent_output_location,
        }
    }

    /// Create an [`AddressUnspentOutput`] which starts iteration for the supplied address.
    /// Used to look up the first output with [`ReadDisk::zs_next_key_value_from`].
    ///
    /// The unspent output location is before all unspent output locations in the index.
    /// It is always invalid, due to the genesis consensus rules. But this is not an issue
    /// since [`ReadDisk::zs_next_key_value_from`] will fetch the next existing (valid) value.
    pub fn address_iterator_start(address_location: AddressLocation) -> AddressUnspentOutput {
        // Iterating from the lowest possible output location gets us the first output.
        let zero_output_location = OutputLocation::from_usize(Height(0), 0, 0);

        AddressUnspentOutput {
            address_location,
            unspent_output_location: zero_output_location,
        }
    }

    /// Update the unspent output location to the next possible output for the supplied address.
    /// Used to look up the next output with [`ReadDisk::zs_next_key_value_from`].
    ///
    /// The updated unspent output location may be invalid, which is not an issue
    /// since [`ReadDisk::zs_next_key_value_from`] will fetch the next existing (valid) value.
    pub fn address_iterator_next(&mut self) {
        // Iterating from the next possible output location gets us the next output,
        // even if it is in a later block or transaction.
        //
        // Consensus: the block size limit is 2MB, which is much lower than the index range.
        self.unspent_output_location.output_index.0 += 1;
    }

    /// The location of the first [`transparent::Output`] sent to the address of this output.
    ///
    /// This can be used to look up the address.
    pub fn address_location(&self) -> AddressLocation {
        self.address_location
    }

    /// The location of this unspent output.
    pub fn unspent_output_location(&self) -> OutputLocation {
        self.unspent_output_location
    }

    /// Allows tests to modify the address location.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn address_location_mut(&mut self) -> &mut AddressLocation {
        &mut self.address_location
    }

    /// Allows tests to modify the unspent output location.
    #[cfg(any(test, feature = "proptest-impl"))]
    #[allow(dead_code)]
    pub fn unspent_output_location_mut(&mut self) -> &mut OutputLocation {
        &mut self.unspent_output_location
    }
}

// Transparent trait impls

/// Returns a byte representing the [`transparent::Address`] variant.
fn address_variant(address: &transparent::Address) -> u8 {
    // Return smaller values for more common variants.
    //
    // (This probably doesn't matter, but it might help slightly with data compression.)
    match (address.network(), address) {
        (Mainnet, PayToPublicKeyHash { .. }) => 0,
        (Mainnet, PayToScriptHash { .. }) => 1,
        (Testnet, PayToPublicKeyHash { .. }) => 2,
        (Testnet, PayToScriptHash { .. }) => 3,
    }
}

impl IntoDisk for transparent::Address {
    type Bytes = [u8; 21];

    fn as_bytes(&self) -> Self::Bytes {
        let variant_bytes = vec![address_variant(self)];
        let hash_bytes = self.hash_bytes().to_vec();

        [variant_bytes, hash_bytes].concat().try_into().unwrap()
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl FromDisk for transparent::Address {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (address_variant, hash_bytes) = disk_bytes.as_ref().split_at(1);

        let address_variant = address_variant[0];
        let hash_bytes = hash_bytes.try_into().unwrap();

        let network = if address_variant < 2 {
            Mainnet
        } else {
            Testnet
        };

        if address_variant % 2 == 0 {
            transparent::Address::from_pub_key_hash(network, hash_bytes)
        } else {
            transparent::Address::from_script_hash(network, hash_bytes)
        }
    }
}

impl IntoDisk for Amount<NonNegative> {
    type Bytes = [u8; BALANCE_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        self.to_bytes()
    }
}

impl FromDisk for Amount<NonNegative> {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        Amount::from_bytes(array).unwrap()
    }
}

// TODO: serialize the index into a smaller number of bytes (#3953)
//       serialize the index in big-endian order (#3953)
impl IntoDisk for OutputIndex {
    type Bytes = [u8; OUTPUT_INDEX_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let mem_bytes = self.index().to_be_bytes();

        let disk_bytes = truncate_zero_be_bytes(&mem_bytes, OUTPUT_INDEX_DISK_BYTES);

        disk_bytes.try_into().unwrap()
    }
}

impl FromDisk for OutputIndex {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let mem_len = u32::BITS / 8;
        let mem_len = mem_len.try_into().unwrap();

        let mem_bytes = expand_zero_be_bytes(disk_bytes.as_ref(), mem_len);
        let mem_bytes = mem_bytes.try_into().unwrap();
        OutputIndex::from_index(u32::from_be_bytes(mem_bytes))
    }
}

impl IntoDisk for OutputLocation {
    type Bytes = [u8; OUTPUT_LOCATION_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let transaction_location_bytes = self.transaction_location().as_bytes().to_vec();
        let output_index_bytes = self.output_index().as_bytes().to_vec();

        [transaction_location_bytes, output_index_bytes]
            .concat()
            .try_into()
            .unwrap()
    }
}

impl FromDisk for OutputLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (transaction_location_bytes, output_index_bytes) = disk_bytes
            .as_ref()
            .split_at(TRANSACTION_LOCATION_DISK_BYTES);

        let transaction_location = TransactionLocation::from_bytes(transaction_location_bytes);
        let output_index = OutputIndex::from_bytes(output_index_bytes);

        OutputLocation {
            transaction_location,
            output_index,
        }
    }
}

impl IntoDisk for AddressBalanceLocation {
    type Bytes = [u8; BALANCE_DISK_BYTES + OUTPUT_LOCATION_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let balance_bytes = self.balance().as_bytes().to_vec();
        let address_location_bytes = self.address_location().as_bytes().to_vec();

        [balance_bytes, address_location_bytes]
            .concat()
            .try_into()
            .unwrap()
    }
}

impl FromDisk for AddressBalanceLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (balance_bytes, address_location_bytes) =
            disk_bytes.as_ref().split_at(BALANCE_DISK_BYTES);

        let balance = Amount::from_bytes(balance_bytes.try_into().unwrap()).unwrap();
        let address_location = AddressLocation::from_bytes(address_location_bytes);

        let mut address_balance_location = AddressBalanceLocation::new(address_location);
        *address_balance_location.balance_mut() = balance;

        address_balance_location
    }
}

impl IntoDisk for transparent::Output {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        self.zcash_serialize_to_vec().unwrap()
    }
}

impl FromDisk for transparent::Output {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        bytes.as_ref().zcash_deserialize_into().unwrap()
    }
}

impl IntoDisk for AddressUnspentOutput {
    type Bytes = [u8; OUTPUT_LOCATION_DISK_BYTES + OUTPUT_LOCATION_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let address_location_bytes = self.address_location().as_bytes();
        let unspent_output_location_bytes = self.unspent_output_location().as_bytes();

        [address_location_bytes, unspent_output_location_bytes]
            .concat()
            .try_into()
            .unwrap()
    }
}

impl FromDisk for AddressUnspentOutput {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (address_location_bytes, unspent_output_location_bytes) =
            disk_bytes.as_ref().split_at(OUTPUT_LOCATION_DISK_BYTES);

        let address_location = AddressLocation::from_bytes(address_location_bytes);
        let unspent_output_location = AddressLocation::from_bytes(unspent_output_location_bytes);

        AddressUnspentOutput::new(address_location, unspent_output_location)
    }
}
