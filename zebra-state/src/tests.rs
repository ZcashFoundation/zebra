use std::ffi::OsStr;

use zebra_chain::{block, parameters::Network};

use super::*;

#[test]
fn test_path_mainnet() {
    test_path(Network::Mainnet);
}

#[test]
fn test_path_testnet() {
    test_path(Network::Testnet);
}

/// Check the sled path for `network`.
fn test_path(network: Network) {
    zebra_test::init();

    let config = Config::default();
    // we can't do many useful tests on this value, because it depends on the
    // local environment and OS.
    let sled_config = config.sled_config(network);
    let mut path = sled_config.get_path();
    assert_eq!(path.file_name(), Some(OsStr::new("state")));
    assert!(path.pop());
    match network {
        Network::Mainnet => assert_eq!(path.file_name(), Some(OsStr::new("mainnet"))),
        Network::Testnet => assert_eq!(path.file_name(), Some(OsStr::new("testnet"))),
    }
}

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
    for (height, min_height) in BLOCK_LOCATOR_CASES.iter().cloned() {
        let locator = util::block_locator_heights(block::Height(height));

        assert!(!locator.is_empty(), "locators must not be empty");
        if (height - min_height) > 1 {
            assert!(
                locator.len() > 2,
                "non-trivial locators must have some intermediate heights"
            );
        }

        assert_eq!(
            locator[0],
            block::Height(height),
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
            block::Height(min_height),
            "locators must end with the specified final height"
        );
        assert!(height - final_height.0 <= constants::MAX_BLOCK_REORG_HEIGHT.0,
                    format!("locator for {} must not be more than the maximum reorg height {} below the tip, but {} is {} blocks below the tip",
                         height,
                         constants::MAX_BLOCK_REORG_HEIGHT.0,
                         final_height.0,
                         height - final_height.0));
    }
}
