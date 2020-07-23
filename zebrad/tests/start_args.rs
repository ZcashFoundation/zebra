use color_eyre::eyre::Result;
use std::time::Duration;
use zebra_test::prelude::*;

pub fn get_child_multi_args(args: &[&str]) -> Result<(zebra_test::command::TestChild, impl Drop)> {
    let (mut cmd, guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    Ok((
        cmd.args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2()
            .unwrap(),
        guard,
    ))
}

#[test]
fn start_args() -> Result<()> {
    zebra_test::init();

    // Any free argument is valid
    let (mut child, _guard) = get_child_multi_args(&["start", "argument"])?;
    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;
    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;
    output.stdout_contains(r"Initializing tracing endpoint")?;

    // unrecognized option `-f`
    let (child, _guard) = get_child_multi_args(&["start", "-f"])?;
    let output = child.wait_with_output()?;
    output.assert_failure()?;

    Ok(())
}
