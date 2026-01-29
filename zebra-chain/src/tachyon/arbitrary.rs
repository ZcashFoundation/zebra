//! Proptest arbitrary implementations for Tachyon types.

use group::ff::Field;
use halo2::pasta::pallas;
use proptest::{arbitrary::any, prelude::*};
use reddsa::{orchard::SpendAuth, Signature};

use super::{
    action::{AuthorizedTachyaction, Tachyaction},
    commitment::ValueCommitment,
    epoch::EpochId,
    nullifier::Nullifier,
    proof::TransactionProof,
    tachygram::Tachygram,
    tree::Root,
};

impl Arbitrary for Tachygram {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<[u8; 32]>().prop_map(Tachygram::from_bytes).boxed()
    }
}

impl Arbitrary for EpochId {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<u32>().prop_map(EpochId::new).boxed()
    }
}

impl Arbitrary for Nullifier {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (any::<u64>(), any::<EpochId>())
            .prop_map(|(nf_val, epoch)| Nullifier::new(pallas::Base::from(nf_val), epoch))
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
            any::<Nullifier>(),
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

impl Arbitrary for TransactionProof {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        proptest::collection::vec(any::<u8>(), 0..256)
            .prop_map(|bytes| TransactionProof::new(bytes).unwrap())
            .boxed()
    }
}

impl Arbitrary for Root {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        any::<u64>()
            .prop_map(|val| Root::from_base(pallas::Base::from(val)))
            .boxed()
    }
}
