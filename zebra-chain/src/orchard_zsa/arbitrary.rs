//! Randomised data generation for Orchard ZSA types.

use proptest::prelude::*;

use orchard::{bundle::testing::BundleArb, issuance::testing::arb_signed_issue_bundle};

// FIXME: consider using another value, i.e. define MAX_BURN_ITEMS constant for that
use crate::{
    orchard::{OrchardFlavorExt, OrchardVanilla, OrchardZSA},
    transaction::arbitrary::MAX_ARBITRARY_ITEMS,
};

use super::{burn::BurnItem, issuance::IssueData};

pub(crate) trait ArbitraryBurn: OrchardFlavorExt {
    fn arbitrary_burn() -> BoxedStrategy<Self::BurnType>;
    // FIXME: remove the following lines
    //   where
    //       Self: Sized
}

impl ArbitraryBurn for OrchardVanilla {
    fn arbitrary_burn() -> BoxedStrategy<Self::BurnType> {
        Just(Default::default()).boxed()
    }
}

impl ArbitraryBurn for OrchardZSA {
    fn arbitrary_burn() -> BoxedStrategy<Self::BurnType> {
        prop::collection::vec(any::<BurnItem>(), 0..MAX_ARBITRARY_ITEMS).boxed()
    }
}

impl Arbitrary for BurnItem {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        // FIXME: move arb_asset_to_burn out of BundleArb in orchard
        // as it does not depend on flavor (we pinned it here OrchardVanilla
        // just for certainty, as there's no difference, which flavor to use)
        // FIXME: consider to use BurnItem(asset_base, value.try_into().expect("Invalid value for Amount"))
        // instead of filtering non-convertable values
        // FIXME: should we filter/protect from including native assets into burn here?
        BundleArb::<orchard::orchard_flavor::OrchardVanilla>::arb_asset_to_burn()
            .prop_filter_map("Conversion to Amount failed", |(asset_base, value)| {
                BurnItem::try_from((asset_base, value)).ok()
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for IssueData {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        arb_signed_issue_bundle(MAX_ARBITRARY_ITEMS)
            .prop_map(|bundle| bundle.into())
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
