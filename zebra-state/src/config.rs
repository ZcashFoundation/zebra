use serde::{Deserialize, Serialize};
use std::{convert::TryInto, path::PathBuf};
use tempdir::TempDir;
use zebra_chain::parameters::Network;

/// Configuration for the state service.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The root directory for storing cached data.
    ///
    /// Cached data includes any state that can be replicated from the network
    /// (e.g., the chain state, the blocks, the UTXO set, etc.). It does *not*
    /// include private data that cannot be replicated from the network, such as
    /// wallet data.  That data is not handled by `zebra-state`.
    ///
    /// Each network has a separate state, which is stored in "mainnet/state"
    /// and "testnet/state" subdirectories.
    ///
    /// The default directory is platform dependent, based on
    /// [`dirs::cache_dir()`](https://docs.rs/dirs/3.0.1/dirs/fn.cache_dir.html):
    ///
    /// |Platform | Value                                           | Example                            |
    /// | ------- | ----------------------------------------------- | ---------------------------------- |
    /// | Linux   | `$XDG_CACHE_HOME/zebra` or `$HOME/.cache/zebra` | /home/alice/.cache/zebra           |
    /// | macOS   | `$HOME/Library/Caches/zebra`                    | /Users/Alice/Library/Caches/zebra  |
    /// | Windows | `{FOLDERID_LocalAppData}\zebra`                 | C:\Users\Alice\AppData\Local\zebra |
    /// | Other   | `std::env::current_dir()/cache`                 |                                    |
    pub cache_dir: PathBuf,

    /// Whether to use an ephemeral database.
    ///
    /// Ephemeral databases are stored in a temporary directory.
    /// They are deleted when Zebra exits successfully.
    /// (If Zebra panics or crashes, the ephemeral database won't be deleted.)
    ///
    /// Set to `false` by default. If this is set to `true`, [`cache_dir`] is ignored.
    ///
    /// [`cache_dir`]: struct.Config.html#structfield.cache_dir
    pub ephemeral: bool,

    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    ///
    /// Set to `None` by default: Zebra continues syncing indefinitely.
    pub debug_stop_at_height: Option<u32>,
}

fn gen_temp_path(prefix: &str) -> PathBuf {
    TempDir::new(prefix)
        .expect("temporary directory is created successfully")
        .into_path()
}

impl Config {
    /// The ideal open file limit for Zebra
    const IDEAL_OPEN_FILE_LIMIT: usize = 1024;

    /// The minimum number of open files for Zebra to operate normally. Also used
    /// as the default open file limit, when the OS doesn't tell us how many
    /// files we can use.
    ///
    /// We want 100+ file descriptors for peers, and 100+ for the database.
    ///
    /// On Windows, the default limit is 512 high-level I/O files, and 8192
    /// low-level I/O files:
    /// https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/setmaxstdio?view=msvc-160#remarks
    const MIN_OPEN_FILE_LIMIT: usize = 512;

    /// The number of files used internally by Zebra.
    ///
    /// Zebra uses file descriptors for OS libraries (10+), polling APIs (10+),
    /// stdio (3), and other OS facilities (2+).
    const RESERVED_FILE_COUNT: usize = 48;

    /// Returns the path and database options for the finalized state database
    pub(crate) fn db_config(&self, network: Network) -> (PathBuf, rocksdb::Options) {
        let net_dir = match network {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        };

        let path = if self.ephemeral {
            gen_temp_path(&format!(
                "zebra-state-v{}-{}",
                crate::constants::DATABASE_FORMAT_VERSION,
                net_dir
            ))
        } else {
            self.cache_dir
                .join("state")
                .join(format!("v{}", crate::constants::DATABASE_FORMAT_VERSION))
                .join(net_dir)
        };

        let mut opts = rocksdb::Options::default();

        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let open_file_limit = Config::increase_open_file_limit();
        let db_file_limit = Config::get_db_open_file_limit(open_file_limit);
        // If the current limit is very large, set the DB limit using the ideal limit
        let db_file_limit = db_file_limit.try_into().unwrap_or_else(|_| {
            Config::get_db_open_file_limit(Config::IDEAL_OPEN_FILE_LIMIT)
                .try_into()
                .expect("ideal open file limit fits in a config int")
        });
        opts.set_max_open_files(db_file_limit);

        (path, opts)
    }

    /// Construct a config for an ephemeral in memory database
    pub fn ephemeral() -> Self {
        let mut config = Self::default();
        config.ephemeral = true;
        config
    }

    /// Calculate the database's share of `open_file_limit`
    fn get_db_open_file_limit(open_file_limit: usize) -> usize {
        // Give the DB half the files, and reserve half the files for peers
        (open_file_limit - Config::RESERVED_FILE_COUNT) / 2
    }

    /// Increase the open file limit for this process to `IDEAL_OPEN_FILE_LIMIT`.
    /// If that fails, try `MIN_OPEN_FILE_LIMIT`.
    ///
    /// If the current limit is above `IDEAL_OPEN_FILE_LIMIT`, leaves it
    /// unchanged.
    ///
    /// Returns the current limit, after any successful increases.
    ///
    /// # Panics
    ///
    /// If the open file limit can not be increased to `MIN_OPEN_FILE_LIMIT`.
    #[cfg(unix)]
    fn increase_open_file_limit() -> usize {
        use rlimit::{getrlimit, Resource};

        let (old_limit, hard_rlimit) = match getrlimit(Resource::NOFILE) {
            Ok((soft_limit, hard_rlimit)) => (soft_limit.try_into().ok(), Some(hard_rlimit)),
            Err(_) => (None, None),
        };

        // There's no API for reliably setting the soft limit to the lower of the
        // hard limit and a desired limit, because:
        //  * the returned hard limit can be invalid or unrepresentable, and
        //  * some OS versions (macOS) return larger values than the actual hard
        //    limit.
        // So we try setting the ideal limit, then the minimum limit.
        if let Ok(actual_limit) =
            Config::set_open_file_limit(Config::IDEAL_OPEN_FILE_LIMIT, hard_rlimit, old_limit)
        {
            return actual_limit;
        }

        // Try the hard limit or the minimum, whichever is greater
        let min_limit = if let Some(hard_limit) =
            hard_rlimit.map(TryInto::try_into).map(Result::ok).flatten()
        {
            std::cmp::max(Config::MIN_OPEN_FILE_LIMIT, hard_limit)
        } else {
            Config::MIN_OPEN_FILE_LIMIT
        };
        if let Ok(actual_limit) = Config::set_open_file_limit(min_limit, hard_rlimit, old_limit) {
            tracing::warn!(?actual_limit,
                           ?hard_rlimit,
                           ?old_limit,
                           min_limit = ?Config::MIN_OPEN_FILE_LIMIT,
                           ideal_limit = ?Config::IDEAL_OPEN_FILE_LIMIT,
                           "the maximum number of open files is below Zebra's ideal limit. Hint: Increase the open file limit to {} before launching Zebra",
                           Config::IDEAL_OPEN_FILE_LIMIT);
            return actual_limit;
        }

        panic!("open file limit too low: unable to set the number of open files to {}, the minimum number of files required by Zebra. Current soft limit is {:?} and hard limit is {:?}. Hint: Increase the open file limit to {} before launching Zebra",
               Config::MIN_OPEN_FILE_LIMIT,
               old_limit,
               hard_rlimit,
               Config::IDEAL_OPEN_FILE_LIMIT);
    }

    /// Increase the soft open file limit for this process to `new_limit`,
    /// and the hard open file limit to `hard_rlimit`.
    ///
    /// If `hard_rlimit` is `None`, also sets the hard limit to `new_limit`.
    ///
    /// If `old_limit` is already greater than or equal to `new_limit`,
    /// returns `Ok(old_limit)`.
    ///
    /// Otherwise, tries to set the limit. Returns `Ok(new_limit)` if the
    /// limit is set successfully.
    #[cfg(unix)]
    fn set_open_file_limit(
        new_limit: usize,
        hard_rlimit: Option<rlimit::Rlim>,
        old_limit: Option<usize>,
    ) -> Result<usize, ()> {
        use rlimit::{setrlimit, Resource};

        if let Some(old_limit) = old_limit {
            if old_limit >= new_limit {
                tracing::info!(?new_limit,
                               current_limit = ?old_limit,
                               ?hard_rlimit,
                               "the open file limit is at or above the specified limit");
                return Ok(old_limit);
            }
        }

        let new_rlimit = new_limit
            .try_into()
            .expect("new_limit is a valid rlimit value");
        if setrlimit(
            Resource::NOFILE,
            new_rlimit,
            hard_rlimit.unwrap_or(new_rlimit),
        )
        .is_ok()
        {
            tracing::info!(
                ?new_limit,
                ?old_limit,
                ?hard_rlimit,
                "set the open file limit for Zebra"
            );
            Ok(new_limit)
        } else {
            Err(())
        }
    }

    /// Assumes that Zebra can open at least the minimum number of files, and
    /// returns `MIN_OPEN_FILE_LIMIT`.
    ///
    /// Increasing the open file limit is not yet implemented on Windows. (And
    /// other non-unix platforms).
    #[cfg(not(unix))]
    fn increase_open_file_limit() -> usize {
        // On Windows, the default limit is 512 high-level I/O files, and 8192
        // low-level I/O files:
        // https://docs.microsoft.com/en-us/cpp/c-runtime-library/reference/setmaxstdio?view=msvc-160#remarks
        //
        // If we need more high-level I/O files on Windows, we could implement
        // support for `setmaxstdio` and `getmaxstdio` in the `rlimit` crate:
        // https://github.com/Nugine/rlimit/issues/16#issuecomment-723393017
        //
        // We should panic if `setmaxstdio` fails to set the minimum value,
        // and `getmaxstdio` is below the minimum value.

        tracing::info!(min_limit = ?Config::MIN_OPEN_FILE_LIMIT,
                       ideal_limit = ?Config::IDEAL_OPEN_FILE_LIMIT,
                       "assuming Zebra can open a minimum number of files");
        Config::MIN_OPEN_FILE_LIMIT
    }
}

impl Default for Config {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap().join("cache"))
            .join("zebra");

        Self {
            cache_dir,
            ephemeral: false,
            debug_stop_at_height: None,
        }
    }
}
