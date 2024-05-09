//! Definitions of Zebra chain constants, including:
//! - slow start interval,
//! - slow start shift

use crate::block::Height;

/// An initial period from Genesis to this Height where the block subsidy is gradually incremented. [What is slow-start mining][slow-mining]
///
/// [slow-mining]: https://z.cash/support/faq/#what-is-slow-start-mining
pub const SLOW_START_INTERVAL: Height = Height(20_000);

/// `SlowStartShift()` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// This calculation is exact, because `SLOW_START_INTERVAL` is divisible by 2.
pub const SLOW_START_SHIFT: Height = Height(SLOW_START_INTERVAL.0 / 2);
