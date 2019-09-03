//! An HTTP endpoint for dynamically setting tracing filters.

use crate::error::Error;

use abscissa_core::{Component, FrameworkError};

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Component, Debug, Default)]
pub struct TracingEndpoint {
    /// TODO: unify with the config objects - need to determine how to
    /// get the TracingEndpoint to read the application config, notify
    /// on config changes, etc.
    filter: String,
}

impl TracingEndpoint {
    /// Create a tracing endpoint
    pub fn new() -> Result<Self, FrameworkError> {
        println!("hello from the tracing endpoint");

        Ok(Self { filter: "info".to_owned() })
    }
}

