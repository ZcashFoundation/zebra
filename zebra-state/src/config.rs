use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Configuration for networking code.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The root directory for the state storage
    pub path: PathBuf,
}

impl Config {
    pub(crate) fn sled_config(&self) -> sled::Config {
        sled::Config::default().path(&self.path)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./.zebra-state"),
        }
    }
}
