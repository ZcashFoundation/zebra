//! In-place format upgrades and format validity checks for the Zebra state database.

pub use zebra_db::service::finalized_state::disk_format::upgrade::*;

#[cfg(test)]
mod tests;

pub mod block_info_and_address_received;
