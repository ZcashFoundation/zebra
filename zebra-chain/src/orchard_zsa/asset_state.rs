//! Defines and implements the issued asset state types

use std::{collections::HashMap, sync::Arc};

use orchard::issuance::IssueAction;
pub use orchard::note::AssetBase;

use crate::block::Block;

use super::BurnItem;

/// The circulating supply and whether that supply has been finalized.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetState {
    /// Indicates whether the asset is finalized such that no more of it can be issued.
    pub is_finalized: bool,

    /// The circulating supply that has been issued for an asset.
    pub total_supply: u128,
}

/// A change to apply to the issued assets map.
// TODO: Reference ZIP
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetStateChange {
    /// Whether the asset should be finalized such that no more of it can be issued.
    pub is_finalized: bool,
    /// The change in supply from newly issued assets or burned assets.
    pub supply_change: i128,
}

impl AssetState {
    fn with_change(mut self, change: AssetStateChange) -> Self {
        self.is_finalized |= change.is_finalized;
        self.total_supply = self
            .total_supply
            .checked_add_signed(change.supply_change)
            .expect("burn amounts must not be greater than initial supply");
        self
    }
}

impl AssetStateChange {
    fn from_note(is_finalized: bool, note: orchard::Note) -> (AssetBase, Self) {
        (
            note.asset(),
            Self {
                is_finalized,
                supply_change: note.value().inner().into(),
            },
        )
    }

    fn from_notes(
        is_finalized: bool,
        notes: &[orchard::Note],
    ) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        notes
            .iter()
            .map(move |note| Self::from_note(is_finalized, *note))
    }

    fn from_issue_actions<'a>(
        actions: impl Iterator<Item = &'a IssueAction> + 'a,
    ) -> impl Iterator<Item = (AssetBase, Self)> + 'a {
        actions.flat_map(|action| Self::from_notes(action.is_finalized(), action.notes()))
    }

    fn from_burn(burn: &BurnItem) -> (AssetBase, Self) {
        (
            burn.asset(),
            Self {
                is_finalized: false,
                supply_change: -i128::from(burn.amount()),
            },
        )
    }

    fn from_burns(burns: &[BurnItem]) -> impl Iterator<Item = (AssetBase, Self)> + '_ {
        burns.iter().map(Self::from_burn)
    }
}

impl std::ops::AddAssign for AssetStateChange {
    fn add_assign(&mut self, rhs: Self) {
        self.is_finalized |= rhs.is_finalized;
        self.supply_change += rhs.supply_change;
    }
}

/// An `issued_asset` map
// TODO: Reference ZIP
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssuedAssets(HashMap<AssetBase, AssetState>);

impl IssuedAssets {
    fn new() -> Self {
        Self(HashMap::new())
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedAssetsChange(HashMap<AssetBase, AssetStateChange>);

impl IssuedAssetsChange {
    fn new() -> Self {
        Self(HashMap::new())
    }

    fn update<'a>(&mut self, changes: impl Iterator<Item = (AssetBase, AssetStateChange)> + 'a) {
        for (asset_base, change) in changes {
            *self.0.entry(asset_base).or_default() += change;
        }
    }

    /// Accepts a reference to an [`Arc<Block>`].
    ///
    /// Returns a tuple, ([`IssuedAssetsChange`], [`IssuedAssetsChange`]), where
    /// the first item is from burns and the second one is for issuance.
    pub fn from_block(block: &Arc<Block>) -> (Self, Self) {
        let mut burn_change = Self::new();
        let mut issuance_change = Self::new();

        for transaction in &block.transactions {
            burn_change.update(AssetStateChange::from_burns(transaction.orchard_burns()));
            issuance_change.update(AssetStateChange::from_issue_actions(
                transaction.orchard_issue_actions(),
            ));
        }

        (burn_change, issuance_change)
    }

    /// Consumes self and accepts a closure for looking up previous asset states.
    ///
    /// Applies changes in self to the previous asset state.
    ///
    /// Returns an [`IssuedAssets`] with the updated asset states.
    pub fn apply_with(self, f: impl Fn(AssetBase) -> AssetState) -> IssuedAssets {
        let mut issued_assets = IssuedAssets::new();

        issued_assets.update(
            self.0
                .into_iter()
                .map(|(asset_base, change)| (asset_base, f(asset_base).with_change(change))),
        );

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
