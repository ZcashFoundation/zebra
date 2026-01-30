//! Tachyon payment addresses.

/// A Tachyon payment address.
///
/// Tachyon addresses enable oblivious synchronization, allowing wallets
/// to delegate sync to untrusted services without revealing private information.
///
/// This is a stub type for future implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    // Future fields: diversifier, pk_d, etc.
    _private: (),
}
