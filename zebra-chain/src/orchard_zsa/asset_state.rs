//! Defines and implements the issued asset state types

// FIXME: finish refactoring of this module, AssetState specifically (including calculation) - re-use AssetState from orchard,
// add tests here in zebra_consensus?

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use thiserror::Error;

use orchard::issuance::IssueAction;
pub use orchard::note::AssetBase;

use crate::transaction::Transaction;

#[cfg(test)]
use crate::serialization::ZcashSerialize;

use super::BurnItem;

/// The circulating supply and whether that supply has been finalized.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct AssetState {
    /// Indicates whether the asset is finalized such that no more of it can be issued.
    pub is_finalized: bool,

    /// The circulating supply that has been issued for an asset.
    pub total_supply: u64,
}

/// A change to apply to the issued assets map.
// TODO: Reference ZIP
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AssetStateChange {
    /// Whether the asset should be finalized such that no more of it can be issued.
    should_finalize: bool,
    /// Whether the asset has been issued in this change.
    includes_issuance: bool,
    /// The change in supply from newly issued assets or burned assets, if any.
    supply_change: SupplyChange,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// An asset supply change to apply to the issued assets map.
enum SupplyChange {
    /// An issuance that should increase the total supply of an asset
    Issuance(u64),

    /// A burn that should reduce the total supply of an asset.
    Burn(u64),
}

impl Default for SupplyChange {
    fn default() -> Self {
        Self::Issuance(0)
    }
}

// FIXME: can we reuse some functions from orchard crate?s
impl SupplyChange {
    /// Applies `self` to a provided `total_supply` of an asset.
    ///
    /// Returns the updated total supply after the [`SupplyChange`] has been applied.
    fn apply_to(self, total_supply: u64) -> Option<u64> {
        match self {
            SupplyChange::Issuance(amount) => total_supply.checked_add(amount),
            SupplyChange::Burn(amount) => total_supply.checked_sub(amount),
        }
    }

    /// Returns the [`SupplyChange`] amount as an [`i128`] where burned amounts
    /// are negative.
    fn as_i128(self) -> i128 {
        match self {
            SupplyChange::Issuance(amount) => i128::from(amount),
            SupplyChange::Burn(amount) => -i128::from(amount),
        }
    }

    /// Attempts to add another supply change to `self`.
    ///
    /// Returns true if successful or false if the result would be invalid.
    fn add(&mut self, rhs: Self) -> bool {
        if let Some(result) = self
            .as_i128()
            .checked_add(rhs.as_i128())
            .and_then(|signed| match signed {
                // Burn amounts MUST not be 0
                // TODO: Reference ZIP
                0.. => signed.try_into().ok().map(Self::Issuance),
                // FIXME: (-signed) - is this a correct fix?
                ..0 => (-signed).try_into().ok().map(Self::Burn),
            })
        {
            *self = result;
            true
        } else {
            false
        }
    }

    /// Returns true if this [`SupplyChange`] is an issuance.
    fn is_issuance(&self) -> bool {
        matches!(self, SupplyChange::Issuance(_))
    }
}

impl std::ops::Neg for SupplyChange {
    type Output = Self;

    fn neg(self) -> Self::Output {
        match self {
            Self::Issuance(amount) => Self::Burn(amount),
            Self::Burn(amount) => Self::Issuance(amount),
        }
    }
}

impl AssetState {
    /// Updates and returns self with the provided [`AssetStateChange`] if
    /// the change is valid, or returns None otherwise.
    fn apply_change(self, change: &AssetStateChange) -> Option<Self> {
        self.apply_finalization(change)?.apply_supply_change(change)
    }

    /// Updates the `is_finalized` field on `self` if the change is valid and
    /// returns `self`, or returns None otherwise.
    fn apply_finalization(mut self, change: &AssetStateChange) -> Option<Self> {
        if self.is_finalized && change.includes_issuance {
            None
        } else {
            self.is_finalized |= change.should_finalize;
            Some(self)
        }
    }

    /// Updates the `supply_change` field on `self` if the change is valid and
    /// returns `self`, or returns None otherwise.
    fn apply_supply_change(mut self, change: &AssetStateChange) -> Option<Self> {
        self.total_supply = change.supply_change.apply_to(self.total_supply)?;
        Some(self)
    }
}

impl AssetStateChange {
    /// Creates a new [`AssetStateChange`] from an asset base, supply change, and
    /// `should_finalize` flag.
    fn new(
        asset_base: AssetBase,
        supply_change: SupplyChange,
        should_finalize: bool,
    ) -> (AssetBase, Self) {
        (
            asset_base,
            Self {
                should_finalize,
                includes_issuance: supply_change.is_issuance(),
                supply_change,
            },
        )
    }

    /// Accepts a transaction and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the transaction to the chain state.
    fn from_transaction(tx: &Arc<Transaction>) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        Self::from_burns(tx.orchard_burns())
            .chain(Self::from_issue_actions(tx.orchard_issue_actions()))
    }

    /// Accepts an iterator of [`IssueAction`]s and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided issue actions to the chain state.
    fn from_issue_actions<'a>(
        actions: impl Iterator<Item = &'a IssueAction> + 'a,
    ) -> impl Iterator<Item = (AssetBase, Self)> + 'a {
        actions.flat_map(Self::from_issue_action)
    }

    /// Accepts an [`IssueAction`] and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided issue action to the chain state.
    fn from_issue_action(action: &IssueAction) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        let supply_changes = Self::from_notes(action.notes());
        let finalize_changes = action
            .is_finalized()
            .then(|| {
                action
                    .notes()
                    .iter()
                    .map(orchard::Note::asset)
                    .collect::<HashSet<AssetBase>>()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|asset_base| Self::new(asset_base, SupplyChange::Issuance(0), true));

        supply_changes.chain(finalize_changes)
    }

    /// Accepts an iterator of [`orchard::Note`]s and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided orchard notes to the chain state.
    fn from_notes(notes: &[orchard::Note]) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        notes.iter().copied().map(Self::from_note)
    }

    /// Accepts an [`orchard::Note`] and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided orchard note to the chain state.
    fn from_note(note: orchard::Note) -> (AssetBase, Self) {
        Self::new(
            note.asset(),
            SupplyChange::Issuance(note.value().inner()),
            false,
        )
    }

    /// Accepts an iterator of [`BurnItem`]s and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided asset burns to the chain state.
    fn from_burns(burns: &[BurnItem]) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        burns.iter().map(Self::from_burn)
    }

    /// Accepts an [`BurnItem`] and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided burn to the chain state.
    fn from_burn(burn: &BurnItem) -> (AssetBase, Self) {
        Self::new(burn.asset(), SupplyChange::Burn(burn.raw_amount()), false)
    }
}

// FIXME: reuse orhcard errors?
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum AssetStateError {
    #[error("invalid asset burn")]
    InvalidBurn,

    #[error("invalid asset issuance")]
    InvalidIssuance,
}

// TODO: Reference ZIP
/// A map of asset state changes for assets modified in a block or transaction set.
/// Contains (old_state, new_state) pairs for each modified asset.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct IssuedAssetChanges(HashMap<AssetBase, (Option<AssetState>, AssetState)>);

impl IssuedAssetChanges {
    /// Returns an iterator over asset bases and their (old, new) state pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&AssetBase, &(Option<AssetState>, AssetState))> {
        self.0.iter()
    }

    /// Validates asset burns and issuance in the given transactions and returns the state changes.
    ///
    /// For each modified asset, returns a tuple of (old_state, new_state).
    /// The old_state is retrieved using the provided `get_asset_state` function.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any burn attempts to burn more than the issued supply
    /// - Any issuance is invalid
    pub fn validate_and_get_changes(
        transactions: &[Arc<Transaction>],
        get_asset_state: impl Fn(&AssetBase) -> Option<AssetState>,
    ) -> Result<Self, AssetStateError> {
        let mut asset_changes = HashMap::new();

        // Burns need to be checked and asset state changes need to be applied per transaction, in case
        // the asset being burned was also issued in an earlier transaction in the same block.
        for transaction in transactions.iter() {
            // Check that no burn item attempts to burn more than the issued supply for an asset
            for burn in transaction.orchard_burns() {
                let asset_base = burn.asset();
                let asset_state = asset_changes
                    .get(&asset_base)
                    .map(|(_old, new)| *new) // Use the new state if already modified in this block
                    .or_else(|| get_asset_state(&asset_base))
                    .ok_or(AssetStateError::InvalidBurn)?;

                if asset_state.total_supply < burn.raw_amount() {
                    return Err(AssetStateError::InvalidBurn);
                }
            }

            for (asset_base, change) in AssetStateChange::from_transaction(transaction) {
                let old_state = asset_changes
                    .get(&asset_base)
                    .map(|(_old, new)| *new) // If already modified, use the current new state as old
                    .or_else(|| get_asset_state(&asset_base));

                let new_state = old_state
                    .unwrap_or_default()
                    .apply_change(&change)
                    .ok_or(AssetStateError::InvalidIssuance)?;

                // Store or update the change pair
                asset_changes
                    .entry(asset_base)
                    .and_modify(|(_old, new)| *new = new_state)
                    .or_insert((old_state, new_state));
            }
        }

        Ok(Self(asset_changes))
    }
}

/// Used in snapshot test for `getassetstate` RPC method.
// TODO: Replace with `AssetBase::random()` or a known value.
#[cfg(test)]
pub trait RandomAssetBase {
    /// Generates a ZSA random asset.
    ///
    /// This is only used in tests.
    fn random_serialized() -> String;
}

#[cfg(test)]
impl RandomAssetBase for AssetBase {
    fn random_serialized() -> String {
        let isk = orchard::keys::IssuanceAuthorizingKey::from_bytes(
            k256::NonZeroScalar::random(&mut rand_core::OsRng)
                .to_bytes()
                .into(),
        )
        .unwrap();
        let ik = orchard::keys::IssuanceValidatingKey::from(&isk);
        let asset_descr = b"zsa_asset".to_vec();
        AssetBase::derive(&ik, &asset_descr)
            .zcash_serialize_to_vec()
            .map(hex::encode)
            .expect("random asset base should serialize")
    }
}
