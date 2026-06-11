//! Tests for [`LoadTrackedClient`].

use zebra_chain::block::Height;

use crate::peer::{ClientTestHarness, LoadTrackedClient};

/// Check that `remote_height` starts at the handshake height, rises
/// monotonically with delivered blocks, and never drops below either signal.
#[test]
fn remote_height_rises_with_delivered_blocks() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (client, _harness) = ClientTestHarness::build()
        .with_start_height(Height(50))
        .finish();
    let client: LoadTrackedClient = client.into();

    // Before any blocks are delivered, the handshake height is all we know.
    assert_eq!(client.remote_height(), Height(50));

    // A delivered block above the handshake height raises the live height.
    client.record_delivered_height(Height(100));
    assert_eq!(client.remote_height(), Height(100));

    // The live height is monotonic: a lower delivered block doesn't lower it.
    client.record_delivered_height(Height(80));
    assert_eq!(client.remote_height(), Height(100));
}

/// Check that the handshake height is used when delivered blocks are below it.
#[test]
fn remote_height_keeps_handshake_height_floor() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let (client, _harness) = ClientTestHarness::build()
        .with_start_height(Height(200))
        .finish();
    let client: LoadTrackedClient = client.into();

    // Delivering a historic block doesn't lower the height below the handshake height.
    client.record_delivered_height(Height(10));
    assert_eq!(client.remote_height(), Height(200));
}
