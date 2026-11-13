//! Mainnet funding recipient schedule tests.

use super::*;
use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters};

#[test]
fn zip_2008_preserves_the_activation_period_and_expired_streams() -> color_eyre::Result<()> {
    for (activation, first_rotated) in [
        (None, 36),
        (Some(3_543_000), 12),
        (Some(3_566_400), 12),
        (Some(4_406_400), 36),
        (Some(4_410_000), 36),
    ] {
        let network = Parameters::build()
            .with_activation_heights(ConfiguredActivationHeights {
                nu7: activation,
                ..Network::Mainnet.activation_list().into()
            })?
            .with_funding_streams(Vec::new())
            .to_network()?;
        for (index, address) in post_nu6_1_funding_stream_fpf_addresses(&network)
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                address,
                if index < first_rotated {
                    "t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"
                } else {
                    "t1MkHnkxVjNpNbCrSs3AJ8J7ZSp6NTYiUcG"
                },
                "incorrect ZIP 2008 recipient at index {index}, activation {activation:?}",
            );
        }
    }
    Ok(())
}
