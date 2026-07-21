use color_eyre::eyre::Result;

use crate::common::{lightwalletd::lwd_integration_test, test_type::TestType::FullSyncFromGenesis};

/// Make sure `lightwalletd` can fully sync from genesis using Zebra.
///
/// This test only runs when:
/// - `TEST_LIGHTWALLETD` is set,
/// - a persistent cached state is configured (e.g., via `ZEBRA_STATE__CACHE_DIR`), and
/// - Zebra is compiled with `--features=lightwalletd-grpc-tests`.
#[test]
#[ignore]
fn lwd_sync_full() -> Result<()> {
    lwd_integration_test(FullSyncFromGenesis {
        allow_lightwalletd_cached_state: false,
    })
}
