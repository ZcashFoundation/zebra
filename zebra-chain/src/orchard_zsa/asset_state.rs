//! Defines and implements the issued asset state types

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use orchard::issuance::IssueAction;
pub use orchard::note::AssetBase;

use crate::transaction::Transaction;

use super::BurnItem;

/// The circulating supply and whether that supply has been finalized.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetState {
    /// Indicates whether the asset is finalized such that no more of it can be issued.
    pub is_finalized: bool,

    /// The circulating supply that has been issued for an asset.
    pub total_supply: u64,
}

/// A change to apply to the issued assets map.
// TODO: Reference ZIP
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetStateChange {
    /// Whether the asset should be finalized such that no more of it can be issued.
    pub is_finalized: bool,
    /// The change in supply from newly issued assets or burned assets, if any.
    pub supply_change: SupplyChange,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// An asset supply change to apply to the issued assets map.
pub enum SupplyChange {
    Issuance(u64),
    Burn(u64),
}

impl Default for SupplyChange {
    fn default() -> Self {
        Self::Issuance(0)
    }
}

impl SupplyChange {
    fn apply_to(self, total_supply: u64) -> Option<u64> {
        match self {
            SupplyChange::Issuance(amount) => total_supply.checked_add(amount),
            SupplyChange::Burn(amount) => total_supply.checked_sub(amount),
        }
    }

    fn as_i128(self) -> i128 {
        match self {
            SupplyChange::Issuance(amount) => i128::from(amount),
            SupplyChange::Burn(amount) => -i128::from(amount),
        }
    }

    fn add(&mut self, rhs: Self) -> bool {
        if let Some(result) = self
            .as_i128()
            .checked_add(rhs.as_i128())
            .and_then(|signed| match signed {
                0.. => signed.try_into().ok().map(Self::Issuance),
                ..0 => signed.try_into().ok().map(Self::Burn),
            })
        {
            *self = result;
            true
        } else {
            false
        }
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
    pub fn apply_change(self, change: AssetStateChange) -> Option<Self> {
        self.apply_finalization(change)?.apply_supply_change(change)
    }

    fn apply_finalization(mut self, change: AssetStateChange) -> Option<Self> {
        if self.is_finalized && change.is_issuance() {
            None
        } else {
            self.is_finalized |= change.is_finalized;
            Some(self)
        }
    }

    fn apply_supply_change(mut self, change: AssetStateChange) -> Option<Self> {
        self.total_supply = change.supply_change.apply_to(self.total_supply)?;
        Some(self)
    }

    /// Reverts the provided [`AssetStateChange`].
    pub fn revert_change(&mut self, change: AssetStateChange) {
        *self = self
            .revert_finalization(change.is_finalized)
            .revert_supply_change(change)
            .expect("reverted change should be validated");
    }

    fn revert_finalization(mut self, is_finalized: bool) -> Self {
        self.is_finalized &= !is_finalized;
        self
    }

    fn revert_supply_change(mut self, change: AssetStateChange) -> Option<Self> {
        self.total_supply = (-change.supply_change).apply_to(self.total_supply)?;
        Some(self)
    }
}

impl From<HashMap<AssetBase, AssetState>> for IssuedAssets {
    fn from(issued_assets: HashMap<AssetBase, AssetState>) -> Self {
        Self(issued_assets)
    }
}

impl AssetStateChange {
    fn new(
        asset_base: AssetBase,
        supply_change: SupplyChange,
        is_finalized: bool,
    ) -> (AssetBase, Self) {
        (
            asset_base,
            Self {
                is_finalized,
                supply_change,
            },
        )
    }

    fn from_transaction(tx: &Arc<Transaction>) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        Self::from_burns(tx.orchard_burns())
            .chain(Self::from_issue_actions(tx.orchard_issue_actions()))
    }

    fn from_issue_actions<'a>(
        actions: impl Iterator<Item = &'a IssueAction> + 'a,
    ) -> impl Iterator<Item = (AssetBase, Self)> + 'a {
        actions.flat_map(Self::from_issue_action)
    }

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

    fn from_notes(notes: &[orchard::Note]) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        notes.iter().copied().map(Self::from_note)
    }

    fn from_note(note: orchard::Note) -> (AssetBase, Self) {
        Self::new(
            note.asset(),
            SupplyChange::Issuance(note.value().inner()),
            false,
        )
    }

    fn from_burns(burns: &[BurnItem]) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        burns.iter().map(Self::from_burn)
    }

    fn from_burn(burn: &BurnItem) -> (AssetBase, Self) {
        Self::new(burn.asset(), SupplyChange::Burn(burn.amount()), false)
    }

    /// Updates and returns self with the provided [`AssetStateChange`] if
    /// the change is valid, or returns None otherwise.
    pub fn apply_change(&mut self, change: AssetStateChange) -> bool {
        if self.is_finalized && change.is_issuance() {
            return false;
        }
        self.is_finalized |= change.is_finalized;
        self.supply_change.add(change.supply_change)
    }

    /// Returns true if the AssetStateChange is for an asset burn.
    pub fn is_burn(&self) -> bool {
        matches!(self.supply_change, SupplyChange::Burn(_))
    }

    /// Returns true if the AssetStateChange is for an asset burn.
    pub fn is_issuance(&self) -> bool {
        matches!(self.supply_change, SupplyChange::Issuance(_))
    }
}

/// An `issued_asset` map
// TODO: Reference ZIP
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssuedAssets(HashMap<AssetBase, AssetState>);

impl IssuedAssets {
    /// Creates a new [`IssuedAssets`].
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Returns an iterator of the inner HashMap.
    pub fn iter(&self) -> impl Iterator<Item = (&AssetBase, &AssetState)> {
        self.0.iter()
    }

    fn update<'a>(&mut self, issued_assets: impl Iterator<Item = (AssetBase, AssetState)> + 'a) {
        for (asset_base, asset_state) in issued_assets {
            self.0.insert(asset_base, asset_state);
        }
    }
}

impl IntoIterator for IssuedAssets {
    type Item = (AssetBase, AssetState);

    type IntoIter = std::collections::hash_map::IntoIter<AssetBase, AssetState>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// A map of changes to apply to the issued assets map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssuedAssetsChange(HashMap<AssetBase, AssetStateChange>);

impl IssuedAssetsChange {
    fn new() -> Self {
        Self(HashMap::new())
    }

    fn update<'a>(
        &mut self,
        changes: impl Iterator<Item = (AssetBase, AssetStateChange)> + 'a,
    ) -> bool {
        for (asset_base, change) in changes {
            if !self.0.entry(asset_base).or_default().apply_change(change) {
                return false;
            }
        }

        true
    }

    /// Accepts a slice of [`Arc<Transaction>`]s.
    ///
    /// Returns an [`IssuedAssetsChange`] representing all of the changes to the issued assets
    /// map that should be applied for the provided transactions.
    pub fn from_transactions(transactions: &[Arc<Transaction>]) -> Option<Self> {
        let mut issued_assets_change = Self::new();

        for transaction in transactions {
            if !issued_assets_change.update(AssetStateChange::from_transaction(transaction)) {
                return None;
            }
        }

        Some(issued_assets_change)
    }

    /// Consumes self and accepts a closure for looking up previous asset states.
    ///
    /// Applies changes in self to the previous asset state.
    ///
    /// Returns an [`IssuedAssets`] with the updated asset states.
    pub fn apply_with(self, f: impl Fn(AssetBase) -> AssetState) -> IssuedAssets {
        let mut issued_assets = IssuedAssets::new();

        issued_assets.update(self.0.into_iter().map(|(asset_base, change)| {
            (
                asset_base,
                f(asset_base)
                    .apply_change(change)
                    .expect("must be valid change"),
            )
        }));

        issued_assets
    }
}

impl std::ops::Add for IssuedAssetsChange {
    type Output = Self;

    fn add(mut self, mut rhs: Self) -> Self {
        if self.0.len() > rhs.0.len() {
            self.update(rhs.0.into_iter());
            self
        } else {
            rhs.update(self.0.into_iter());
            rhs
        }
    }
}

impl IntoIterator for IssuedAssetsChange {
    type Item = (AssetBase, AssetStateChange);

    type IntoIter = std::collections::hash_map::IntoIter<AssetBase, AssetStateChange>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
