mod lint_engine;

use anyhow::anyhow;
use camino::Utf8PathBuf;
use lint_engine::LintEngineConfig;
use seamare::prelude::{CoreContext, SeamareError};
use seamare_lints::DirectDepDups;
use std::env;

type Result<T> = anyhow::Result<T>;

fn main() -> Result<()> {
    run_lint_engine()
}

fn run_lint_engine() -> Result<()> {
    // basic progress:
    // check if it's running inside a cargo project, build context can handle this.
    let path = env::current_dir()?;
    let path = Utf8PathBuf::from_path_buf(path).map_err(|e| SeamareError::NotValidUtf8Path(e))?;
    let context = CoreContext::new(path.as_path())?;
    // Initialize a lint engine, load linter.
    let engine = LintEngineConfig::new(&context)
        .with_project_linters(&[&DirectDepDups])
        .build();

    // Run this lint engine.
    let results = engine.run()?;
    for (source, message) in &results.messages {
        println!(
            "[{}] [{}] [{}]: {}\n",
            message.level(),
            source.name(),
            source.kind(),
            message.message()
        );
    }

    if !results.messages.is_empty() {
        Err(anyhow!("there were lint errors"))
    } else {
        Ok(())
    }
}
