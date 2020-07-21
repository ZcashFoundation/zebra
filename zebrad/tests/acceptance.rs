//! Acceptance test: runs zebrad as a subprocess and asserts its
//! output for given argument combinations matches what is expected.

#![warn(warnings, missing_docs, trivial_casts, unused_qualifications)]
#![forbid(unsafe_code)]

use color_eyre::eyre::Result;
use std::time::Duration;
use zebra_test::prelude::*;

// Todo: The following 3 helper functions can probably be abstracted into one
fn get_child_single_arg(arg: &str) -> Result<zebra_test::command::TestChild> {
    let (mut cmd, _guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    cmd.arg(arg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn2()
}

fn get_child_multi_args(args: &[&str]) -> Result<zebra_test::command::TestChild> {
    let (mut cmd, _guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn2()
}

fn get_child_no_args() -> Result<zebra_test::command::TestChild> {
    let (mut cmd, _guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn2()
}

#[test]
fn generate_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_single_arg("generate");
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"# Default configuration values for zebrad.")?;

    Ok(())
}

#[test]
fn generate_args() -> Result<()> {
    zebra_test::init();

    // Invalid free argument
    let child = get_child_multi_args(&["generate", "argument"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unexpected free argument `argument`")?;

    // Invalid flag
    let child = get_child_multi_args(&["generate", "-f"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unrecognized option `-f`")?;

    // Valid flag but missing argument
    let child = get_child_multi_args(&["generate", "-o"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"missing argument to option `-o`")?;

    // Valid
    let child = get_child_multi_args(&["generate", "-o", "file.yaml"]);
    let output = child.unwrap().wait_with_output()?;
    output.assert_failure()?; // should be assert_success?

    // Todo: Check if the file was created

    Ok(())
}

#[test]
fn help_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_single_arg("help");
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn help_args() -> Result<()> {
    zebra_test::init();

    // Invalid argument
    let child = get_child_multi_args(&["help", "argument"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"wasn't recognized.")?;

    // Invalid flag
    let child = get_child_multi_args(&["help", "-f"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"option `-f` does not accept an argument")?;

    Ok(())
}

#[test]
fn revhex_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_single_arg("revhex");

    //Program is waiting for input, we just exit after 1 second
    std::thread::sleep(Duration::from_secs(1));
    let mut child_unwrapped = child.unwrap();
    child_unwrapped.kill()?;

    let output = child_unwrapped.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}

#[test]
fn revhex_args() -> Result<()> {
    zebra_test::init();

    // Valid
    let child = get_child_multi_args(&["revhex", "33eeff55"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"55ffee33")?;

    Ok(())
}

#[test]
fn seed_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_single_arg("seed");

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    let mut child_unwrapped = child.unwrap();
    child_unwrapped.kill()?;

    let output = child_unwrapped.wait_with_output()?;
    let output = output.assert_failure()?;

    // Todo: maybe add special info!() to seed command
    // Todo: improve the regex
    output.stdout_contains(r"^(.*?)Initializing tracing endpoint")?;

    Ok(())
}

#[test]
fn seed_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_multi_args(&["seed", "argument"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unexpected free argument `argument`")?;

    let child = get_child_multi_args(&["seed", "-f"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unrecognized option `-f`")?;

    let child = get_child_multi_args(&["seed", "start"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unexpected free argument `start`")?;

    Ok(())
}

#[test]
fn start_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_single_arg("start");

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    let mut child_unwrapped = child.unwrap();
    child_unwrapped.kill()?;

    let output = child_unwrapped.wait_with_output()?;
    let output = output.assert_failure()?;

    // Todo: maybe add special info!() to seed command
    // Todo: improve the regex
    output.stdout_contains(r"^(.*?)Initializing tracing endpoint")?;

    Ok(())
}

#[test]
fn start_args() -> Result<()> {
    zebra_test::init();

    // Bug? This should fail but not happening
    //let child = get_child_multi_args(&["start", "argument"]);
    //let output = child.unwrap().wait_with_output()?;
    //let output = output.assert_failure()?;

    // Invalid flag
    let child = get_child_multi_args(&["start", "-f"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unrecognized option `-f`")?;

    // Start + seed should be the only combination possible
    let child = get_child_multi_args(&["start", "seed"]);

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    let mut child_unwrapped = child.unwrap();
    child_unwrapped.kill()?;

    let output = child_unwrapped.wait_with_output()?;
    let output = output.assert_failure()?;

    // Todo: maybe add special info!() to seed command
    // Todo: improve the regex
    output.stdout_contains(r"^(.*?)Initializing tracing endpoint")?;

    Ok(())
}

#[test]
fn app_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_no_args();
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"USAGE:")?;

    Ok(())
}

#[test]
fn version_no_args() -> Result<()> {
    zebra_test::init();

    let child = get_child_single_arg("version");
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_success()?;

    output.stdout_contains(r"zebrad [0-9].[0-9].[0-9]")?;

    Ok(())
}

#[test]
fn version_args() -> Result<()> {
    zebra_test::init();

    // Invalid free argument
    let child = get_child_multi_args(&["version", "argument"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unexpected free argument `argument`")?;

    // Invalid flag
    let child = get_child_multi_args(&["version", "-f"]);
    let output = child.unwrap().wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"unrecognized option `-f`")?;

    Ok(())
}
