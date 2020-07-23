use color_eyre::eyre::Result;
use std::time::Duration;
use zebra_test::prelude::*;

pub fn get_child_single_arg(arg: &str) -> Result<(zebra_test::command::TestChild, impl Drop)> {
    let (mut cmd, guard) = test_cmd(env!("CARGO_BIN_EXE_zebrad"))?;

    Ok((
        cmd.arg(arg)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn2()
            .unwrap(),
        guard,
    ))
}

#[test]
fn start_no_args() -> Result<()> {
    zebra_test::init();

    let (mut child, _guard) = get_child_single_arg("start")?;

    // Run the program and kill it at 1 second
    std::thread::sleep(Duration::from_secs(1));
    child.kill()?;

    let output = child.wait_with_output()?;
    let output = output.assert_failure()?;

    output.stdout_contains(r"Starting zebrad")?;

    Ok(())
}
