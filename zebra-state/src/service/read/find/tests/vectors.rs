//! Fixed test vectors for "find" read requests.

use zebra_chain::block::Height;

use crate::{constants, service::read::find::block_locator_heights};

/// Block heights, and the expected minimum block locator height
static BLOCK_LOCATOR_CASES: &[(u32, u32)] = &[
    (0, 0),
    (1, 0),
    (10, 0),
    (98, 0),
    (99, 0),
    (100, 1),
    (101, 2),
    (1000, 901),
    (10000, 9901),
];

/// Check that the block locator heights are sensible.
#[test]
fn test_block_locator_heights() {
    let _init_guard = zebra_test::init();

    for (height, min_height) in BLOCK_LOCATOR_CASES.iter().cloned() {
        let locator = block_locator_heights(Height(height));

        assert!(!locator.is_empty(), "locators must not be empty");
        if (height - min_height) > 1 {
            assert!(
                locator.len() > 2,
                "non-trivial locators must have some intermediate heights"
            );
        }

        assert_eq!(
            locator[0],
            Height(height),
            "locators must start with the tip height"
        );

        // Check that the locator is sorted, and that it has no duplicates
        // TODO: replace with dedup() and is_sorted_by() when sorting stabilises.
        assert!(locator.windows(2).all(|v| match v {
            [a, b] => a.0 > b.0,
            _ => unreachable!("windows returns exact sized slices"),
        }));

        let final_height = locator[locator.len() - 1];
        assert_eq!(
            final_height,
            Height(min_height),
            "locators must end with the specified final height"
        );
        assert!(
            height - final_height.0 <= constants::MAX_BLOCK_REORG_HEIGHT,
            "locator for {} must not be more than the maximum reorg height {} below the tip, \
             but {} is {} blocks below the tip",
            height,
            constants::MAX_BLOCK_REORG_HEIGHT,
            final_height.0,
            height - final_height.0
        );
    }
}
