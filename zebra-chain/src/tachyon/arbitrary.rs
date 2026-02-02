//! Proptest arbitrary implementations for Tachyon types.

use group::prime::PrimeCurveAffine;
use halo2::pasta::pallas;
use proptest::{arbitrary::any, prelude::*};
use reddsa::Signature;
use tachyon::primitives::Fp;

use crate::transaction;

use super::{
    accumulator::Anchor,
    action::{AuthorizedTachyaction, Tachyaction},
    commitment::ValueCommitment,
    nullifier::FlavoredNullifier,
    proof::AggregateProof,
    shielded_data::AggregateData,
    tachygram::Tachygram,
};

impl Arbitrary for Tachygram {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<[u8; 32]>().prop_map(Tachygram::from_bytes).boxed()
    }
}

impl Arbitrary for FlavoredNullifier {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<u64>(), any::<u64>())
            .prop_map(|(nf_val, epoch_val)| {
                FlavoredNullifier::new(
                    tachyon::Nullifier::from_field(Fp::from(nf_val)),
                    tachyon::Epoch::new(epoch_val),
                )
            })
            .boxed()
    }
}

impl Arbitrary for ValueCommitment {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        // Generate a random point on the curve by using identity or generator
        // For testing purposes, we use simple deterministic values
        any::<bool>()
            .prop_map(|use_identity| {
                if use_identity {
                    ValueCommitment(pallas::Affine::identity())
                } else {
                    ValueCommitment(pallas::Affine::generator())
                }
            })
            .boxed()
    }
}

impl Arbitrary for Tachyaction {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<ValueCommitment>(),
            any::<FlavoredNullifier>(),
            any::<u64>(),
            any::<[u8; 32]>(),
        )
            .prop_map(|(cv, nullifier, cm_x_val, rk_bytes)| Tachyaction {
                cv,
                nullifier,
                cm_x: pallas::Base::from(cm_x_val),
                rk: rk_bytes.into(),
            })
            .boxed()
    }
}

impl Arbitrary for AuthorizedTachyaction {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<Tachyaction>(), any::<[u8; 64]>())
            .prop_map(|(action, sig_bytes)| AuthorizedTachyaction {
                action,
                spend_auth_sig: Signature::from(sig_bytes),
            })
            .boxed()
    }
}

impl Arbitrary for AggregateProof {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        proptest::collection::vec(any::<u8>(), 0..256)
            .prop_map(|bytes| AggregateProof::new(bytes).unwrap())
            .boxed()
    }
}

impl Arbitrary for AggregateData {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            any::<AggregateProof>(),
            proptest::collection::vec(any::<[u8; 32]>(), 0..10),
        )
            .prop_map(|(proof, tx_hashes)| {
                let covered = tx_hashes
                    .into_iter()
                    .map(transaction::Hash)
                    .collect();
                AggregateData::new(proof, covered)
            })
            .boxed()
    }
}

impl Arbitrary for Anchor {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<u64>()
            .prop_map(|val| Anchor::from_base(pallas::Base::from(val)))
            .boxed()
    }
}
