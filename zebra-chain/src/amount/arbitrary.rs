//! Randomised test case generation for amounts.

use proptest::prelude::*;

use crate::amount::*;

impl<C> Arbitrary for Amount<C>
where
    C: Constraint + std::fmt::Debug,
{
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        C::valid_range().prop_map(|v| Self(v, PhantomData)).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
