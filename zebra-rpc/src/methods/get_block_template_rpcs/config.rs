//! Mining config

use serde::{Deserialize, Serialize};

use zebra_chain::transparent::Address;

/// Mining configuration section.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Miner transparent address
    pub miner_address: Option<Address>,
}
