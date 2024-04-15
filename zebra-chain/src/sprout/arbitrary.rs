use proptest::{array, prelude::*};

use crate::{
    amount::{Amount, NonNegative},
    primitives::ZkSnarkProof,
};

use super::{commitment, joinsplit, note, tree, JoinSplit};

impl<P: ZkSnarkProof + Arbitrary + 'static> Arbitrary for JoinSplit<P> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Amount<NonNegative>>(),
            any::<Amount<NonNegative>>(),
            any::<tree::Root>(),
            array::uniform2(any::<note::Nullifier>()),
            array::uniform2(any::<commitment::NoteCommitment>()),
            array::uniform32(any::<u8>()),
            any::<joinsplit::RandomSeed>(),
            array::uniform2(any::<note::Mac>()),
            any::<P>(),
            array::uniform2(any::<note::EncryptedNote>()),
        )
            .prop_map(
                |(
                    vpub_old,
                    vpub_new,
                    anchor,
                    nullifiers,
                    commitments,
                    ephemeral_key_bytes,
                    random_seed,
                    vmacs,
                    zkproof,
                    enc_ciphertexts,
                )| {
                    Self {
                        vpub_old,
                        vpub_new,
                        anchor,
                        nullifiers,
                        commitments,
                        ephemeral_key: x25519_dalek::PublicKey::from(ephemeral_key_bytes),
                        random_seed,
                        vmacs,
                        zkproof,
                        enc_ciphertexts,
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
