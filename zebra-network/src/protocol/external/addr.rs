//! Zcash node address types and serialization for Zcash network messages.
//!
//! Zcash has 3 different node address formats:
//! - `AddrV1`: the format used in `addr` (v1) messages,
//! - [`AddrInVersion`]: the format used in `version` messages, and
//! - `AddrV2`: the format used in `addrv2` messages.

pub mod canonical;
pub mod in_version;

pub(crate) mod v1;
pub(crate) mod v2;

pub use canonical::canonical_socket_addr;
pub use in_version::AddrInVersion;

// These types and functions should only be visible in the `external` module,
// so that they don't leak outside the serialization code.

pub(super) use v1::AddrV1;
pub(super) use v2::AddrV2;

#[allow(unused_imports)]
#[cfg(any(test, feature = "proptest-impl"))]
pub(super) use v1::{ipv6_mapped_socket_addr, ADDR_V1_SIZE};

// TODO: write tests for addrv2 deserialization
#[allow(unused_imports)]
#[cfg(any(test, feature = "proptest-impl"))]
pub(super) use v2::ADDR_V2_MIN_SIZE;
