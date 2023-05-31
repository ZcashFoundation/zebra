//! Cache directory configuration for zebra-network.

use std::path::{Path, PathBuf};

use zebra_chain::parameters::Network;

/// A cache directory config field.
///
/// This cache directory configuration field is optional.
/// It defaults to being enabled with the default config path,
/// but also allows a custom path to be set.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CacheDir {
    /// Whether the cache directory is enabled with the default path (`true`),
    /// or disabled (`false`).
    Enabled(bool),

    /// Enable the cache directory and use a custom path.
    CustomPath(PathBuf),
}

impl CacheDir {
    /// Returns a `CacheDir` enabled with the default path.
    pub fn enabled() -> Self {
        Self::Enabled(true)
    }

    /// Returns a disabled `CacheDir`.
    pub fn disabled() -> Self {
        Self::Enabled(false)
    }

    /// Returns a custom `CacheDir` enabled with `path`.
    pub fn custom_path(path: impl AsRef<Path>) -> Self {
        Self::CustomPath(path.as_ref().to_owned())
    }

    /// Returns the peer cache file path for `network`, if enabled.
    pub fn peer_cache_file_path(&self, network: Network) -> Option<PathBuf> {
        Some(
            self.cache_dir()?
                .join("network")
                .join(format!("{}.peers", network.lowercase_name())),
        )
    }

    /// Returns the `zebra-network` base cache directory, if enabled.
    pub fn cache_dir(&self) -> Option<PathBuf> {
        match self {
            Self::Enabled(should_cache) => should_cache.then(|| {
                dirs::cache_dir()
                    .unwrap_or_else(|| std::env::current_dir().unwrap().join("cache"))
                    .join("zebra")
            }),

            Self::CustomPath(cache_dir) => Some(cache_dir.to_owned()),
        }
    }
}

impl Default for CacheDir {
    fn default() -> Self {
        Self::enabled()
    }
}
