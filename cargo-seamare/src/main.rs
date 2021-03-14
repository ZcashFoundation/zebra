mod lint_engine;

use camino::Utf8PathBuf;
use lint_engine::{LintEngine, LintEngineBuilder};
use seamare::prelude::{CoreContext, Result, SeamareError};
use std::env;

fn main() -> Result<()> {
    run_lint_engine()
}

fn run_lint_engine() -> Result<()> {
    // basic progress:
    // check if it's running inside a cargo project, build context can handle this.
    let path = env::current_dir()?;
    let path = Utf8PathBuf::from_path_buf(path).map_err(|e| SeamareError::NotValidUtf8Path(e))?;
    let context = CoreContext::new(path.as_path());
    // Initialize a lint engine, load linter.
    // Run this lint engine.
    Ok(())
}
