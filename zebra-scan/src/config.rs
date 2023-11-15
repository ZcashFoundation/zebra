//! Configuration for blockchain scanning tasks.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::storage::SaplingScanningKey;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
/// Configuration for scanning.
pub struct Config {
    /// The sapling keys to scan for and the birthday height of each of them.
    // TODO: any value below sapling activation as the birthday height should default to sapling activation.
    pub sapling_keys_to_scan: IndexMap<SaplingScanningKey, u32>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sapling_keys_to_scan: IndexMap::new(),
        }
    }
}
