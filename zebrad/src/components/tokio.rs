//! A component owning the Tokio runtime.

use abscissa_core::{Component, FrameworkError};

use color_eyre::Report;
use std::future::Future;
use tokio::runtime::Runtime;

/// An Abscissa component which owns a Tokio runtime.
///
/// The runtime is stored as an `Option` so that when it's time to enter an async
/// context by calling `block_on` with a "root future", the runtime can be taken
/// independently of Abscissa's component locking system. Otherwise whatever
/// calls `block_on` holds an application lock for the entire lifetime of the
/// async context.
#[derive(Component, Debug)]
pub struct TokioComponent {
    pub rt: Option<Runtime>,
}

impl TokioComponent {
    pub fn new() -> Result<Self, FrameworkError> {
        Ok(Self {
            rt: Some(Runtime::new().unwrap()),
        })
    }
}

/// Zebrad's handler for various signals
async fn signal_handler() -> Result<(), Report> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}

/// Extension trait to centralize entry point for runnable subcommands that
/// depend on tokio
pub(crate) trait RuntimeRun {
    fn run(&mut self, fut: impl Future<Output = Result<(), Report>>);
}

impl RuntimeRun for Runtime {
    fn run(&mut self, fut: impl Future<Output = Result<(), Report>>) {
        let result = self.block_on(async move {
            tokio::select! {
                result = fut => result,
                result = signal_handler() => result,
            }
        });

        match result {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {:?}", e);
                std::process::exit(1);
            }
        }
    }
}
