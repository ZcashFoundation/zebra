//! Tests for the nonce registry.

use super::NonceRegistry;
use crate::protocol::external::types::Nonce;

/// Lookups see registered nonces.
#[tokio::test]
async fn register_and_contains() {
    let registry = NonceRegistry::spawn(8);
    let nonce = Nonce(42);

    assert!(!registry.contains(nonce).await.expect("registry is running"));
    registry.register(nonce).await.expect("registry is running");
    // Commands are handled in order, so a following lookup sees the registration.
    assert!(registry.contains(nonce).await.expect("registry is running"));
}

/// The set is bounded: the oldest nonces age out past the limit.
#[tokio::test]
async fn registry_evicts_the_oldest_past_the_limit() {
    let registry = NonceRegistry::spawn(3);

    for value in 1..=4 {
        registry
            .register(Nonce(value))
            .await
            .expect("registry is running");
    }

    assert!(
        !registry
            .contains(Nonce(1))
            .await
            .expect("registry is running"),
        "evicted"
    );
    for value in 2..=4 {
        assert!(registry
            .contains(Nonce(value))
            .await
            .expect("registry is running"));
    }
}
