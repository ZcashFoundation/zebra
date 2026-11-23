//! Generate populated, local-only Regtest state for an unmodified `zebrad`.

use std::{env, fs, path::PathBuf, process::ExitCode};

use zebra_chain::{
    block::Height,
    parameters::{testnet::ConfiguredActivationHeights, Network},
};
use zebra_state::{mock, BoxError};

const USAGE: &str = "Usage: mock-state <new-directory> [height (default: 120)]";
const ACTIVATION_HEIGHT: u32 = 1;

// These messages are the CLI's user-facing output, not tracing diagnostics.
#[allow(clippy::print_stdout, clippy::print_stderr)]
fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(directory) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    if directory == "--help" || directory == "-h" {
        println!("Generate populated Regtest state for local development.\n{USAGE}");
        return ExitCode::SUCCESS;
    }
    let height = match args.next() {
        Some(height) => match height.to_str().and_then(|value| value.parse::<u32>().ok()) {
            Some(height) => Height(height),
            None => {
                eprintln!("Height must be a positive integer.\n{USAGE}");
                return ExitCode::FAILURE;
            }
        },
        None => Height(120),
    };
    if args.next().is_some() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    match generate(PathBuf::from(directory), height) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mock-state: {error}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::print_stdout)]
fn generate(directory: PathBuf, height: Height) -> Result<(), BoxError> {
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(ACTIVATION_HEIGHT),
            nu6_3: Some(ACTIVATION_HEIGHT),
            ..Default::default()
        }
        .into(),
    );
    let tip = mock::build(directory.clone(), &network, height)?;
    fs::write(
        directory.join("zebrad.toml"),
        format!(
            r#"[network]
network = {{ regtest = true, params = {{ activation_heights = {{ NU5 = {ACTIVATION_HEIGHT}, "NU6.3" = {ACTIVATION_HEIGHT} }} }} }}
listen_addr = "127.0.0.1:28233"
initial_testnet_peers = []

[state]
cache_dir = "."

[rpc]
cookie_dir = "."
listen_addr = "127.0.0.1:28232"
"#
        ),
    )?;

    println!("Mock Regtest tip: {} {tip}", height.0);
    println!("From {} run:", directory.display());
    println!("  zebrad --config zebrad.toml start");
    Ok(())
}
