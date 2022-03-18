//! `lightwalletd`-specific shared code for the `zebrad` acceptance tests.
//!
//! # Warning
//!
//! Test functions in this file will not be run.
//! This file is only for test library code.

use std::{env, net::SocketAddr, path::Path};

use zebra_test::{
    command::{TestChild, TestDirExt},
    net::random_known_port,
    prelude::*,
};
use zebrad::config::ZebradConfig;

use super::{config::default_test_config, launch::ZebradTestDirExt};

/// The name of the env var that enables Zebra lightwalletd integration tests.
/// These tests need a `lightwalletd` binary in the test machine's path.
///
/// We use a constant so that the compiler detects typos.
///
/// # Note
///
/// This environmental variable is used to enable the lightwalletd tests.
/// But the network tests are *disabled* by their environmental variables.
const ZEBRA_TEST_LIGHTWALLETD: &str = "ZEBRA_TEST_LIGHTWALLETD";

/// Should we skip Zebra lightwalletd integration tests?
#[allow(clippy::print_stderr)]
pub fn zebra_skip_lightwalletd_tests() -> bool {
    // TODO: check if the lightwalletd binary is in the PATH?
    //       (this doesn't seem to be implemented in the standard library)
    //
    // See is_command_available in zebra-test/tests/command.rs for one way to do this.

    if env::var_os(ZEBRA_TEST_LIGHTWALLETD).is_none() {
        // This message is captured by the test runner, use
        // `cargo test -- --nocapture` to see it.
        eprintln!(
            "Skipped lightwalletd integration test, \
             set the 'ZEBRA_TEST_LIGHTWALLETD' environmental variable to run the test",
        );
        return true;
    }

    false
}

/// Returns a `zebrad` config with a random known RPC port.
pub fn random_known_rpc_port_config() -> Result<ZebradConfig> {
    // [Note on port conflict](#Note on port conflict)
    let listen_port = random_known_port();
    let listen_ip = "127.0.0.1".parse().expect("hard-coded IP is valid");
    let zebra_rpc_listener = SocketAddr::new(listen_ip, listen_port);

    // Write a configuration that has the rpc listen_addr option set
    // TODO: split this config into another function?
    let mut config = default_test_config()?;
    config.rpc.listen_addr = Some(zebra_rpc_listener);

    Ok(config)
}

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
