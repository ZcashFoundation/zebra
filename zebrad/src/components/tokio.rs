//! A component owning the Tokio runtime.

use abscissa_core::{Component, FrameworkError};

use super::tracing::cleanup_tracing;
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

    pub fn start_signal_handler(&self) {
        self.rt
            .as_ref()
            .expect("this option is only to work around locks, runtime should be here")
            .spawn(signal_handler());
    }
}

/// Zebrad's handler for various signals
async fn signal_handler() {
    tokio::signal::ctrl_c().await.unwrap();
    cleanup_tracing();
    std::process::exit(1);
}
