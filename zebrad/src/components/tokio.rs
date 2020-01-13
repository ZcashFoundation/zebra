//! A component owning the Tokio runtime.

use abscissa_core::{Component, FrameworkError};

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
