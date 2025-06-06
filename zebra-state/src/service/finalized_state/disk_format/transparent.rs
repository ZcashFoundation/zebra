//! Transparent transfer serialization formats for finalized data.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{cmp::max, fmt::Debug};

use serde::{Deserialize, Serialize};

use zebra_chain::{
    amount::{self, Amount, Constraint, NegativeAllowed, NonNegative},
    block::Height,
    parameters::NetworkKind,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transparent::{self, Address::*, OutputIndex},
};

use crate::service::finalized_state::disk_format::{
    block::{TransactionIndex, TransactionLocation, TRANSACTION_LOCATION_DISK_BYTES},
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

/// The maximum value of an on-disk serialized [`OutputIndex`].
///
/// This allows us to store [`OutputLocation`]s in
/// 8 bytes, which makes database searches more efficient.
///
/// # Consensus
///
/// This output index is impossible with the current 2 MB block size limit.
///
/// Since Zebra only stores fully verified blocks on disk, blocks with larger indexes
/// are rejected before reaching the database.
pub const MAX_ON_DISK_OUTPUT_INDEX: OutputIndex =
    OutputIndex::from_index((1 << (OUTPUT_INDEX_DISK_BYTES * 8)) - 1);

/// [`OutputLocation`]s are stored as a 3 byte height, 2 byte transaction index,
/// and 3 byte output index on disk.
///
/// This reduces database size and increases lookup performance.
pub const OUTPUT_LOCATION_DISK_BYTES: usize =
    TRANSACTION_LOCATION_DISK_BYTES + OUTPUT_INDEX_DISK_BYTES;

// Transparent types

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

    /// Creates an output location from an [`transparent::OutPoint`],
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

/// The inner type of [`AddressBalanceLocation`] and [`AddressBalanceLocationChange`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Serialize, Deserialize),
    serde(bound = "C: Constraint + Clone")
)]
pub struct AddressBalanceLocationInner<C: Constraint + Copy + std::fmt::Debug> {
    /// The total balance of all UTXOs sent to an address.
    balance: Amount<C>,

    /// The total balance of all spent and unspent outputs sent to an address.
    received: u64,

    /// The location of the first [`transparent::Output`] sent to an address.
    location: AddressLocation,
}

impl<C: Constraint + Copy + std::fmt::Debug> AddressBalanceLocationInner<C> {
    /// Creates a new [`AddressBalanceLocationInner`] from the location of
    /// the first [`transparent::Output`] sent to an address.
    ///
    /// The returned value has a zero initial balance and received balance.
    fn new(first_output: OutputLocation) -> Self {
        Self {
            balance: Amount::zero(),
            received: 0,
            location: first_output,
        }
    }

    /// Returns the current balance for the address.
    pub fn balance(&self) -> Amount<C> {
        self.balance
    }

    /// Returns the current received balance for the address.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Returns a mutable reference to the current balance for the address.
    pub fn balance_mut(&mut self) -> &mut Amount<C> {
        &mut self.balance
    }

    /// Returns a mutable reference to the current received balance for the address.
    pub fn received_mut(&mut self) -> &mut u64 {
        &mut self.received
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

impl<C: Constraint + Copy + std::fmt::Debug> std::ops::Add for AddressBalanceLocationInner<C> {
    type Output = Result<Self, amount::Error>;

    fn add(self, rhs: Self) -> Self::Output {
        Ok(AddressBalanceLocationInner {
            balance: (self.balance + rhs.balance)?,
            received: self.received.saturating_add(rhs.received),
            location: self.location.min(rhs.location),
        })
    }
}

/// Represents a change in the [`AddressBalanceLocation`] of a transparent address
/// in the finalized state.
pub struct AddressBalanceLocationChange(AddressBalanceLocationInner<NegativeAllowed>);

impl AddressBalanceLocationChange {
    /// Creates a new [`AddressBalanceLocationChange`].
    ///
    /// See [`AddressBalanceLocationInner::new`] for more details.
    pub fn new(location: AddressLocation) -> Self {
        Self(AddressBalanceLocationInner::new(location))
    }

    /// Updates the current balance by adding the supplied output's value.
    #[allow(clippy::unwrap_in_result)]
    pub fn receive_output(
        &mut self,
        unspent_output: &transparent::Output,
    ) -> Result<(), amount::Error> {
        self.balance = (self
            .balance
            .zatoshis()
            .checked_add(unspent_output.value().zatoshis()))
        .expect("adding two Amounts is always within an i64")
        .try_into()?;
        self.received = self.received.saturating_add(unspent_output.value().into());
        Ok(())
    }

    /// Updates the current balance by subtracting the supplied output's value.
    #[allow(clippy::unwrap_in_result)]
    pub fn spend_output(
        &mut self,
        spent_output: &transparent::Output,
    ) -> Result<(), amount::Error> {
        self.balance = (self
            .balance
            .zatoshis()
            .checked_sub(spent_output.value().zatoshis()))
        .expect("subtracting two Amounts is always within an i64")
        .try_into()?;

        Ok(())
    }
}

impl std::ops::Deref for AddressBalanceLocationChange {
    type Target = AddressBalanceLocationInner<NegativeAllowed>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for AddressBalanceLocationChange {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::ops::Add for AddressBalanceLocationChange {
    type Output = Result<Self, amount::Error>;

    fn add(self, rhs: Self) -> Self::Output {
        (self.0 + rhs.0).map(Self)
    }
}

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
pub struct AddressBalanceLocation(AddressBalanceLocationInner<NonNegative>);

impl AddressBalanceLocation {
    /// Creates a new [`AddressBalanceLocation`].
    ///
    /// See [`AddressBalanceLocationInner::new`] for more details.
    pub fn new(first_output: OutputLocation) -> Self {
        Self(AddressBalanceLocationInner::new(first_output))
    }

    /// Consumes self and returns a new [`AddressBalanceLocationChange`] with
    /// a zero balance, zero received balance, and the `location` of `self`.
    pub fn into_new_change(self) -> AddressBalanceLocationChange {
        AddressBalanceLocationChange::new(self.location)
    }
}

impl std::ops::Deref for AddressBalanceLocation {
    type Target = AddressBalanceLocationInner<NonNegative>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for AddressBalanceLocation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::ops::Add for AddressBalanceLocation {
    type Output = Result<Self, amount::Error>;

    fn add(self, rhs: Self) -> Self::Output {
        (self.0 + rhs.0).map(Self)
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

    /// Create an [`AddressUnspentOutput`] which starts iteration for the
    /// supplied address. Used to look up the first output with
    /// [`ReadDisk::zs_next_key_value_from`][1].
    ///
    /// The unspent output location is before all unspent output locations in
    /// the index. It is always invalid, due to the genesis consensus rules. But
    /// this is not an issue since [`ReadDisk::zs_next_key_value_from`][1] will
    /// fetch the next existing (valid) value.
    ///
    /// [1]: super::super::disk_db::ReadDisk::zs_next_key_value_from
    pub fn address_iterator_start(address_location: AddressLocation) -> AddressUnspentOutput {
        // Iterating from the lowest possible output location gets us the first output.
        let zero_output_location = OutputLocation::from_usize(Height(0), 0, 0);

        AddressUnspentOutput {
            address_location,
            unspent_output_location: zero_output_location,
        }
    }

    /// Update the unspent output location to the next possible output for the
    /// supplied address. Used to look up the next output with
    /// [`ReadDisk::zs_next_key_value_from`][1].
    ///
    /// The updated unspent output location may be invalid, which is not an
    /// issue since [`ReadDisk::zs_next_key_value_from`][1] will fetch the next
    /// existing (valid) value.
    ///
    /// [1]: super::super::disk_db::ReadDisk::zs_next_key_value_from
    pub fn address_iterator_next(&mut self) {
        // Iterating from the next possible output location gets us the next output,
        // even if it is in a later block or transaction.
        //
        // Consensus: the block size limit is 2MB, which is much lower than the index range.
        self.unspent_output_location.output_index += 1;
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

/// A single transaction sent to a [`transparent::Address`].
///
/// We store both the address location key and transaction location value
/// in the RocksDB column family key. This improves insert and delete performance.
///
/// This requires 8 extra bytes for each transaction location,
/// because we repeat the key for each value.
/// But RocksDB compression reduces the duplicate data size on disk.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(Arbitrary, Serialize, Deserialize)
)]
pub struct AddressTransaction {
    /// The location of the first [`transparent::Output`] sent to the address in `output`.
    address_location: AddressLocation,

    /// The location of the transaction sent to the address.
    transaction_location: TransactionLocation,
}

impl AddressTransaction {
    /// Create a new [`AddressTransaction`] from an address location,
    /// and a transaction location.
    pub fn new(
        address_location: AddressLocation,
        transaction_location: TransactionLocation,
    ) -> AddressTransaction {
        AddressTransaction {
            address_location,
            transaction_location,
        }
    }

    /// Create a range of [`AddressTransaction`]s which starts iteration for the supplied
    /// address. Starts at the first UTXO, or at the `query` start height, whichever is greater.
    /// Ends at the maximum possible transaction index for the end height.
    ///
    /// Used to look up transactions with [`DiskDb::zs_forward_range_iter`][1].
    ///
    /// The transaction locations in the:
    /// - start bound might be invalid, if it is based on the `query` start height.
    /// - end bound will always be invalid.
    ///
    /// But this is not an issue, since [`DiskDb::zs_forward_range_iter`][1] will fetch all existing
    /// (valid) values in the range.
    ///
    /// [1]: super::super::disk_db::DiskDb
    pub fn address_iterator_range(
        address_location: AddressLocation,
        query: std::ops::RangeInclusive<Height>,
    ) -> std::ops::RangeInclusive<AddressTransaction> {
        // Iterating from the lowest possible transaction location gets us the first transaction.
        //
        // The address location is the output location of the first UTXO sent to the address,
        // and addresses can not spend funds until they receive their first UTXO.
        let first_utxo_location = address_location.transaction_location();

        // Iterating from the start height to the end height filters out transactions that aren't needed.
        let query_start_location = TransactionLocation::from_index(*query.start(), 0);
        let query_end_location = TransactionLocation::from_index(*query.end(), u16::MAX);

        let addr_tx = |tx_loc| AddressTransaction::new(address_location, tx_loc);

        addr_tx(max(first_utxo_location, query_start_location))..=addr_tx(query_end_location)
    }

    /// Update the transaction location to the next possible transaction for the
    /// supplied address. Used to look up the next output with
    /// [`ReadDisk::zs_next_key_value_from`][1].
    ///
    /// The updated transaction location may be invalid, which is not an issue
    /// since [`ReadDisk::zs_next_key_value_from`][1] will fetch the next
    /// existing (valid) value.
    ///
    /// [1]: super::super::disk_db::ReadDisk::zs_next_key_value_from
    #[allow(dead_code)]
    pub fn address_iterator_next(&mut self) {
        // Iterating from the next possible output location gets us the next output,
        // even if it is in a later block or transaction.
        //
        // Consensus: the block size limit is 2MB, which is much lower than the index range.
        self.transaction_location.index.0 += 1;
    }

    /// The location of the first [`transparent::Output`] sent to the address of this output.
    ///
    /// This can be used to look up the address.
    pub fn address_location(&self) -> AddressLocation {
        self.address_location
    }

    /// The location of this transaction.
    pub fn transaction_location(&self) -> TransactionLocation {
        self.transaction_location
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
    pub fn transaction_location_mut(&mut self) -> &mut TransactionLocation {
        &mut self.transaction_location
    }
}

// Transparent trait impls

/// Returns a byte representing the [`transparent::Address`] variant.
fn address_variant(address: &transparent::Address) -> u8 {
    use NetworkKind::*;
    // Return smaller values for more common variants.
    //
    // (This probably doesn't matter, but it might help slightly with data compression.)
    match (address.network_kind(), address) {
        (Mainnet, PayToPublicKeyHash { .. }) => 0,
        (Mainnet, PayToScriptHash { .. }) => 1,
        // There's no way to distinguish between Regtest and Testnet for encoded transparent addresses,
        // we can consider `Regtest` to use `Testnet` transparent addresses, so it's okay to use the `Testnet`
        // address variant for `Regtest` transparent addresses in the db format
        (Testnet | Regtest, PayToPublicKeyHash { .. }) => 2,
        (Testnet | Regtest, PayToScriptHash { .. }) => 3,
        // TEX address variants
        (Mainnet, Tex { .. }) => 4,
        (Testnet | Regtest, Tex { .. }) => 5,
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
            NetworkKind::Mainnet
        } else {
            NetworkKind::Testnet
        };

        if address_variant % 2 == 0 {
            transparent::Address::from_pub_key_hash(network, hash_bytes)
        } else {
            transparent::Address::from_script_hash(network, hash_bytes)
        }
    }
}

impl<C: Constraint> IntoDisk for Amount<C> {
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

impl IntoDisk for OutputIndex {
    type Bytes = [u8; OUTPUT_INDEX_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let mem_bytes = self.index().to_be_bytes();

        let disk_bytes = truncate_zero_be_bytes(&mem_bytes, OUTPUT_INDEX_DISK_BYTES);

        match disk_bytes {
            Some(b) => b.try_into().unwrap(),
            // # Security
            //
            // The RPC method or state query was given a transparent output index that is
            // impossible with the current block size limit of 2 MB. To save space in database
            // indexes, we don't support output indexes 2^24 and above.
            //
            // Instead,  we return an invalid database output index to the lookup code,
            // which can never be inserted into the database as part of a valid block.
            // So RPC methods will return an error or None.
            None => {
                #[cfg(test)]
                {
                    use zebra_chain::serialization::TrustedPreallocate;
                    assert!(
                        u64::from(MAX_ON_DISK_OUTPUT_INDEX.index())
                            > zebra_chain::transparent::Output::max_allocation(),
                        "increased block size requires database output index format change",
                    );
                }

                truncate_zero_be_bytes(
                    &MAX_ON_DISK_OUTPUT_INDEX.index().to_be_bytes(),
                    OUTPUT_INDEX_DISK_BYTES,
                )
                .expect("max on disk output index is valid")
                .try_into()
                .unwrap()
            }
        }
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

impl<C: Constraint + Copy + std::fmt::Debug> IntoDisk for AddressBalanceLocationInner<C> {
    type Bytes = [u8; BALANCE_DISK_BYTES + OUTPUT_LOCATION_DISK_BYTES + size_of::<u64>()];

    fn as_bytes(&self) -> Self::Bytes {
        let balance_bytes = self.balance().as_bytes().to_vec();
        let address_location_bytes = self.address_location().as_bytes().to_vec();
        let received_bytes = self.received().to_le_bytes().to_vec();

        [balance_bytes, address_location_bytes, received_bytes]
            .concat()
            .try_into()
            .unwrap()
    }
}

impl IntoDisk for AddressBalanceLocation {
    type Bytes = [u8; BALANCE_DISK_BYTES + OUTPUT_LOCATION_DISK_BYTES + size_of::<u64>()];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.as_bytes()
    }
}

impl IntoDisk for AddressBalanceLocationChange {
    type Bytes = [u8; BALANCE_DISK_BYTES + OUTPUT_LOCATION_DISK_BYTES + size_of::<u64>()];

    fn as_bytes(&self) -> Self::Bytes {
        self.0.as_bytes()
    }
}

impl<C: Constraint + Copy + std::fmt::Debug> FromDisk for AddressBalanceLocationInner<C> {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (balance_bytes, rest) = disk_bytes.as_ref().split_at(BALANCE_DISK_BYTES);
        let (address_location_bytes, rest) = rest.split_at(BALANCE_DISK_BYTES);
        let (received_bytes, _) = rest.split_at_checked(size_of::<u64>()).unwrap_or_default();

        let balance = Amount::from_bytes(balance_bytes.try_into().unwrap()).unwrap();
        let address_location = AddressLocation::from_bytes(address_location_bytes);
        // # Backwards Compatibility
        //
        // If the value is missing a `received` field, default to 0.
        let received = u64::from_le_bytes(received_bytes.try_into().unwrap_or_default());

        let mut address_balance_location = Self::new(address_location);
        *address_balance_location.balance_mut() = balance;
        *address_balance_location.received_mut() = received;

        address_balance_location
    }
}

impl FromDisk for AddressBalanceLocation {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        Self(AddressBalanceLocationInner::from_bytes(disk_bytes))
    }
}

impl FromDisk for AddressBalanceLocationChange {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        Self(AddressBalanceLocationInner::from_bytes(disk_bytes))
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

impl IntoDisk for AddressTransaction {
    type Bytes = [u8; OUTPUT_LOCATION_DISK_BYTES + TRANSACTION_LOCATION_DISK_BYTES];

    fn as_bytes(&self) -> Self::Bytes {
        let address_location_bytes: [u8; OUTPUT_LOCATION_DISK_BYTES] =
            self.address_location().as_bytes();
        let transaction_location_bytes: [u8; TRANSACTION_LOCATION_DISK_BYTES] =
            self.transaction_location().as_bytes();

        address_location_bytes
            .iter()
            .copied()
            .chain(transaction_location_bytes.iter().copied())
            .collect::<Vec<u8>>()
            .try_into()
            .expect("concatenation of fixed-sized arrays should have the correct size")
    }
}

impl FromDisk for AddressTransaction {
    fn from_bytes(disk_bytes: impl AsRef<[u8]>) -> Self {
        let (address_location_bytes, transaction_location_bytes) =
            disk_bytes.as_ref().split_at(OUTPUT_LOCATION_DISK_BYTES);

        let address_location = AddressLocation::from_bytes(address_location_bytes);
        let transaction_location = TransactionLocation::from_bytes(transaction_location_bytes);

        AddressTransaction::new(address_location, transaction_location)
    }
}
