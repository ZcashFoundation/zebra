//! Cache directory configuration for zebra-network.

use std::path::{Path, PathBuf};

use zebra_chain::{common::default_cache_dir, parameters::Network};

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
    IsEnabled(bool),

    /// Enable the cache directory and use a custom path.
    CustomPath(PathBuf),
}

impl From<bool> for CacheDir {
    fn from(value: bool) -> Self {
        CacheDir::IsEnabled(value)
    }
}

impl From<PathBuf> for CacheDir {
    fn from(value: PathBuf) -> Self {
        CacheDir::CustomPath(value)
    }
}

impl CacheDir {
    /// Returns a `CacheDir` enabled with the default path.
    pub fn default_path() -> Self {
        Self::IsEnabled(true)
    }

    /// Returns a disabled `CacheDir`.
    pub fn disabled() -> Self {
        Self::IsEnabled(false)
    }

    /// Returns a custom `CacheDir` enabled with `path`.
    pub fn custom_path(path: impl AsRef<Path>) -> Self {
        Self::CustomPath(path.as_ref().to_owned())
    }

    /// Returns `true` if this `CacheDir` is enabled with the default or a custom path.
    pub fn is_enabled(&self) -> bool {
        match self {
            CacheDir::IsEnabled(is_enabled) => *is_enabled,
            CacheDir::CustomPath(_) => true,
        }
    }

    /// Returns the peer cache file path for `network`, if enabled.
    pub fn peer_cache_file_path(&self, network: &Network) -> Option<PathBuf> {
        Some(
            self.cache_dir()?
                .join("network")
                .join(format!("{}.peers", network.lowercase_name())),
        )
    }

    /// Returns the `zebra-network` base cache directory, if enabled.
    pub fn cache_dir(&self) -> Option<PathBuf> {
        match self {
            Self::IsEnabled(is_enabled) => is_enabled.then(default_cache_dir),
            Self::CustomPath(cache_dir) => Some(cache_dir.to_owned()),
        }
    }
}

impl Default for CacheDir {
    fn default() -> Self {
        Self::default_path()
    }
}
