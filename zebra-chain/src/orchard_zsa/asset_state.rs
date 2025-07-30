//! Defines and implements the issued asset state types

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use orchard::issuance::IssueAction;
pub use orchard::note::AssetBase;

use crate::{serialization::ZcashSerialize, transaction::Transaction};

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
pub struct AssetStateChange {
    /// Whether the asset should be finalized such that no more of it can be issued.
    pub should_finalize: bool,
    /// Whether the asset has been issued in this change.
    pub includes_issuance: bool,
    /// The change in supply from newly issued assets or burned assets, if any.
    pub supply_change: SupplyChange,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// An asset supply change to apply to the issued assets map.
pub enum SupplyChange {
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
    pub fn is_issuance(&self) -> bool {
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
    pub fn apply_change(self, change: AssetStateChange) -> Option<Self> {
        self.apply_finalization(change)?.apply_supply_change(change)
    }

    /// Updates the `is_finalized` field on `self` if the change is valid and
    /// returns `self`, or returns None otherwise.
    fn apply_finalization(mut self, change: AssetStateChange) -> Option<Self> {
        if self.is_finalized && change.includes_issuance {
            None
        } else {
            self.is_finalized |= change.should_finalize;
            Some(self)
        }
    }

    /// Updates the `supply_change` field on `self` if the change is valid and
    /// returns `self`, or returns None otherwise.
    fn apply_supply_change(mut self, change: AssetStateChange) -> Option<Self> {
        self.total_supply = change.supply_change.apply_to(self.total_supply)?;
        Some(self)
    }

    /// Reverts the provided [`AssetStateChange`].
    pub fn revert_change(&mut self, change: AssetStateChange) {
        *self = self
            .revert_finalization(change.should_finalize)
            .revert_supply_change(change)
            .expect("reverted change should be validated");
    }

    /// Reverts the changes to `is_finalized` from the provied [`AssetStateChange`].
    fn revert_finalization(mut self, should_finalize: bool) -> Self {
        self.is_finalized &= !should_finalize;
        self
    }

    /// Reverts the changes to `supply_change` from the provied [`AssetStateChange`].
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
    fn from_burns<'a>(
        burns: impl Iterator<Item = &'a BurnItem> + 'a,
    ) -> impl Iterator<Item = (AssetBase, Self)> + 'a {
        burns.map(Self::from_burn)
    }

    /// Accepts an [`BurnItem`] and returns an iterator of asset bases and issued asset state changes
    /// that should be applied to those asset bases when committing the provided burn to the chain state.
    fn from_burn(burn: &BurnItem) -> (AssetBase, Self) {
        Self::new(burn.asset(), SupplyChange::Burn(burn.raw_amount()), false)
    }

    /// Updates and returns self with the provided [`AssetStateChange`] if
    /// the change is valid, or returns None otherwise.
    pub fn apply_change(&mut self, change: AssetStateChange) -> bool {
        if self.should_finalize && change.includes_issuance {
            return false;
        }
        self.should_finalize |= change.should_finalize;
        self.includes_issuance |= change.includes_issuance;
        self.supply_change.add(change.supply_change)
    }
}

/// An map of issued asset states by asset base.
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

    /// Extends inner [`HashMap`] with updated asset states from the provided iterator
    fn extend<'a>(&mut self, issued_assets: impl Iterator<Item = (AssetBase, AssetState)> + 'a) {
        self.0.extend(issued_assets);
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
    /// Creates a new [`IssuedAssetsChange`].
    fn new() -> Self {
        Self(HashMap::new())
    }

    /// Applies changes in the provided iterator to an [`IssuedAssetsChange`].
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

    /// Accepts a [`Arc<Transaction>`].
    ///
    /// Returns an [`IssuedAssetsChange`] representing all of the changes to the issued assets
    /// map that should be applied for the provided transaction, or `None` if the change would be invalid.
    pub fn from_transaction(transaction: &Arc<Transaction>) -> Option<Self> {
        let mut issued_assets_change = Self::new();

        if !issued_assets_change.update(AssetStateChange::from_transaction(transaction)) {
            return None;
        }

        Some(issued_assets_change)
    }

    /// Accepts a slice of [`Arc<Transaction>`]s.
    ///
    /// Returns an [`IssuedAssetsChange`] representing all of the changes to the issued assets
    /// map that should be applied for the provided transactions.
    pub fn from_transactions(transactions: &[Arc<Transaction>]) -> Option<Arc<[Self]>> {
        transactions.iter().map(Self::from_transaction).collect()
    }

    /// Consumes self and accepts a closure for looking up previous asset states.
    ///
    /// Applies changes in self to the previous asset state.
    ///
    /// Returns an [`IssuedAssets`] with the updated asset states.
    pub fn apply_with(self, f: impl Fn(AssetBase) -> AssetState) -> IssuedAssets {
        let mut issued_assets = IssuedAssets::new();

        issued_assets.extend(self.0.into_iter().map(|(asset_base, change)| {
            (
                asset_base,
                f(asset_base)
                    .apply_change(change)
                    .expect("must be valid change"),
            )
        }));

        issued_assets
    }

    /// Iterates over the inner [`HashMap`] of asset bases and state changes.
    pub fn iter(&self) -> impl Iterator<Item = (AssetBase, AssetStateChange)> + '_ {
        self.0.iter().map(|(&base, &state)| (base, state))
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

impl From<Arc<[IssuedAssetsChange]>> for IssuedAssetsChange {
    fn from(change: Arc<[IssuedAssetsChange]>) -> Self {
        change
            .iter()
            .cloned()
            .reduce(|a, b| a + b)
            .unwrap_or_default()
    }
}

/// Used in snapshot test for `getassetstate` RPC method.
// TODO: Replace with `AssetBase::random()` or a known value.
pub trait RandomAssetBase {
    /// Generates a ZSA random asset.
    ///
    /// This is only used in tests.
    fn random_serialized() -> String;
}

impl RandomAssetBase for AssetBase {
    fn random_serialized() -> String {
        let isk = orchard::keys::IssuanceAuthorizingKey::from_bytes(
            k256::NonZeroScalar::random(&mut rand_core::OsRng)
                .to_bytes()
                .into(),
        )
        .unwrap();
        let ik = orchard::keys::IssuanceValidatingKey::from(&isk);

        let asset_descr: [u8; 32] = *b"zsa_asset\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

        AssetBase::derive(&ik, &asset_descr)
            .zcash_serialize_to_vec()
            .map(hex::encode)
            .expect("random asset base should serialize")
    }
}
