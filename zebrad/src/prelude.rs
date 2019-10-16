//! Application-local prelude: conveniently import types/functions/macros
//! which are generally useful and should be available everywhere.

/// Application state accessors
pub use crate::application::{app_config, app_reader, app_writer};

/// Commonly used Abscissa traits
pub use abscissa_core::{Application, Command, Runnable};

// These are disabled because we use tracing.
// Logging macros
//pub use abscissa_core::log::{debug, error, info, log, log_enabled, trace, warn};

/// Type alias to make working with tower traits easier.
///
/// Note: the 'static lifetime bound means that the *type* cannot have any
/// non-'static lifetimes, (e.g., when a type contains a borrow and is
/// parameterized by 'a), *not* that the object itself has 'static lifetime.
pub(crate) type BoxedStdError = Box<dyn std::error::Error + Send + Sync + 'static>;
