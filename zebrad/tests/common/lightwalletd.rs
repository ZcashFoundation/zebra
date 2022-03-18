//! `lightwalletd`-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{net::SocketAddr, path::Path};

use zebra_test::{
    command::{TestChild, TestDirExt},
    prelude::*,
};

use super::launch::ZebradTestDirExt;

/// Extension trait for methods on `tempfile::TempDir` for using it as a test
/// directory for `zebrad`.
pub trait LightWalletdTestDirExt: ZebradTestDirExt
where
    Self: AsRef<Path> + Sized,
{
    /// Spawn `lightwalletd` with `args` as a child process in this test directory,
    /// potentially taking ownership of the tempdir for the duration of the
    /// child process.
    ///
    /// By default, launch a working test instance with logging, and avoid port conflicts.
    ///
    /// # Panics
    ///
    /// If there is no lightwalletd config in the test directory.
    fn spawn_lightwalletd_child(self, args: &[&str]) -> Result<TestChild<Self>>;

    /// Create a config file and use it for all subsequently spawned `lightwalletd` processes.
    /// Returns an error if the config already exists.
    ///
    /// If needed:
    ///   - recursively create directories for the config
    fn with_lightwalletd_config(self, zebra_rpc_listener: SocketAddr) -> Result<Self>;
}

impl<T> LightWalletdTestDirExt for T
where
    Self: TestDirExt + AsRef<Path> + Sized,
{
    fn spawn_lightwalletd_child(self, extra_args: &[&str]) -> Result<TestChild<Self>> {
        let dir = self.as_ref().to_owned();
        let default_config_path = dir.join("lightwalletd-zcash.conf");

        assert!(
            default_config_path.exists(),
            "lightwalletd requires a config"
        );

        // By default, launch a working test instance with logging,
        // and avoid port conflicts.
        let mut args: Vec<_> = vec![
            // the fake zcashd conf we just wrote
            "--zcash-conf-path",
            default_config_path
                .as_path()
                .to_str()
                .expect("Path is valid Unicode"),
            // the lightwalletd cache directory
            //
            // TODO: create a sub-directory for lightwalletd
            "--data-dir",
            dir.to_str().expect("Path is valid Unicode"),
            // log to standard output
            //
            // TODO: if lightwalletd needs to run on Windows,
            //       work out how to log to the terminal on all platforms
            "--log-file",
            "/dev/stdout",
            // let the OS choose a random available wallet client port
            "--grpc-bind-addr",
            "127.0.0.1:0",
            "--http-bind-addr",
            "127.0.0.1:0",
            // don't require a TLS certificate for the HTTP server
            "--no-tls-very-insecure",
        ];
        args.extend_from_slice(extra_args);

        self.spawn_child_with_command("lightwalletd", &args)
    }

    fn with_lightwalletd_config(self, zebra_rpc_listener: SocketAddr) -> Result<Self> {
        use std::fs;

        let lightwalletd_config = format!(
            "\
            rpcbind={}\n\
            rpcport={}\n\
            ",
            zebra_rpc_listener.ip(),
            zebra_rpc_listener.port(),
        );

        let dir = self.as_ref();
        fs::create_dir_all(dir)?;

        let config_file = dir.join("lightwalletd-zcash.conf");
        fs::write(config_file, lightwalletd_config.as_bytes())?;

        Ok(self)
    }
}
