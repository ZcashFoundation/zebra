//! A component owning the Tokio runtime.

use abscissa_core::{Component, FrameworkError};

use tokio::runtime::Runtime;

/// An Abscissa component which owns a Tokio runtime.
#[derive(Component, Debug)]
pub struct TokioComponent {
    pub rt: Runtime,
}

impl TokioComponent {
    pub fn new() -> Result<Self, FrameworkError> {
        Ok(Self {
            rt: Runtime::new().unwrap(),
        })
    }
}
