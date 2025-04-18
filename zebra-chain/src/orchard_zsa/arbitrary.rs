//! Randomised data generation for OrchardZSA types.

use proptest::prelude::*;

use orchard::{bundle::testing::BundleArb, issuance::testing::arb_signed_issue_bundle};

// FIXME: consider using another value, i.e. define MAX_BURN_ITEMS constant for that
use crate::transaction::arbitrary::MAX_ARBITRARY_ITEMS;

use super::{
    burn::{Burn, BurnItem, NoBurn},
    issuance::IssueData,
};

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
            .prop_map(|(asset_base, value)| BurnItem::from((asset_base, value)))
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for NoBurn {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        // FIXME: consider using this instead, for clarity: any::<()>().prop_map(|_| NoBurn).boxed()
        Just(Self).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Burn {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        prop::collection::vec(any::<BurnItem>(), 0..MAX_ARBITRARY_ITEMS)
            .prop_map(|inner| inner.into())
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
