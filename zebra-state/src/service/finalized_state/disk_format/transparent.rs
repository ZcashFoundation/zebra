//! Transparent transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::Network::*,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::{self, Address::*},
};

use crate::service::finalized_state::disk_format::{
    block::{
        TransactionIndex, TransactionLocation, HEIGHT_DISK_BYTES, TRANSACTION_LOCATION_DISK_BYTES,
    },
    expand_zero_be_bytes, truncate_zero_be_bytes, FromDisk, IntoDisk,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

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

/// A transaction's index in its block.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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

    /// Create a transparent output index from the native index integer type.
    #[allow(dead_code)]
    pub fn from_usize(output_index: usize) -> OutputIndex {
        OutputIndex(
            output_index
                .try_into()
                .expect("the maximum valid index fits in the inner type"),
        )
    }

    /// Return this index as the native index integer type.
    #[allow(dead_code)]
    pub fn as_usize(&self) -> usize {
        self.0
            .try_into()
            .expect("the maximum valid index fits in usize")
    }
}

/// A transparent output's location in the chain, by block height and transaction index.
///
/// [`OutputLocation`]s are sorted in increasing chain order, by height, transaction index,
/// and output index.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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
    #[allow(dead_code)]
    pub fn height(&self) -> Height {
        self.transaction_location.height
    }

    /// Returns the transaction index of this [`transparent::Output`].
    #[allow(dead_code)]
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
/// - the [`OutputLocation`] for the first [`transparent::Output`] sent to that address
///   (regardless of whether that output is spent or unspent).
///
/// All other address data is tracked multiple times for each address
/// (UTXOs and transactions).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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

    /// Returns the location of the first [`transparent::Output`] sent to an address.
    pub fn location(&self) -> AddressLocation {
        self.location
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
        let location_bytes = self.location().as_bytes().to_vec();

        [balance_bytes, location_bytes].concat().try_into().unwrap()
    }
}

impl FromDisk for AddressBalanceLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (balance_bytes, location_bytes) = disk_bytes.as_ref().split_at(BALANCE_DISK_BYTES);

        let balance = Amount::from_bytes(balance_bytes.try_into().unwrap()).unwrap();
        let location = AddressLocation::from_bytes(location_bytes);

        let mut balance_location = AddressBalanceLocation::new(location);
        *balance_location.balance_mut() = balance;

        balance_location
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

// TODO: delete UTXO serialization (#3953)
impl IntoDisk for transparent::Utxo {
    type Bytes = Vec<u8>;

    fn as_bytes(&self) -> Self::Bytes {
        let height_bytes = self.height.as_bytes().to_vec();
        let coinbase_flag_bytes = [self.from_coinbase as u8].to_vec();
        let output_bytes = self
            .output
            .zcash_serialize_to_vec()
            .expect("serialization to vec doesn't fail");

        [height_bytes, coinbase_flag_bytes, output_bytes].concat()
    }
}

impl FromDisk for transparent::Utxo {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let (height_bytes, rest_bytes) = bytes.as_ref().split_at(HEIGHT_DISK_BYTES);
        let (coinbase_flag_bytes, output_bytes) = rest_bytes.split_at(1);

        let height = Height::from_bytes(height_bytes);
        let from_coinbase = coinbase_flag_bytes[0] == 1u8;
        let output = output_bytes
            .zcash_deserialize_into()
            .expect("db has valid serialized data");

        Self {
            output,
            height,
            from_coinbase,
        }
    }
}
