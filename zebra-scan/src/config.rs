//! Configuration for blockchain scanning tasks.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use zebra_state::Config as DbConfig;

use crate::storage::SaplingScanningKey;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
/// Configuration for scanning.
pub struct Config {
    /// The sapling keys to scan for and the birthday height of each of them.
    //
    // TODO: allow keys without birthdays
    pub sapling_keys_to_scan: IndexMap<SaplingScanningKey, u32>,

    /// The scanner results database config.
    //
    // TODO: Remove fields that are only used by the state to create a common database config.
    #[serde(flatten)]
    db_config: DbConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sapling_keys_to_scan: IndexMap::new(),
            db_config: DbConfig::default(),
        }
    }
}

impl Config {
    /// Returns a config for a temporary database that is deleted when it is dropped.
    pub fn ephemeral() -> Self {
        Self {
            db_config: DbConfig::ephemeral(),
            ..Self::default()
        }
    }

    /// Returns the database-specific config.
    pub fn db_config(&self) -> &DbConfig {
        &self.db_config
    }
}
