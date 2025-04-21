//! Randomised data generation for OrchardZSA types.

use proptest::prelude::*;

use orchard::{bundle::testing::BundleArb, issuance::testing::arb_signed_issue_bundle};

use crate::transaction::arbitrary::MAX_ARBITRARY_ITEMS;

use super::{
    burn::{Burn, BurnItem, NoBurn},
    issuance::IssueData,
};

impl Arbitrary for BurnItem {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        BundleArb::<orchard::orchard_flavor::OrchardVanilla>::arb_asset_to_burn()
            .prop_map(|(asset_base, value)| BurnItem::from((asset_base, value)))
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for NoBurn {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
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
