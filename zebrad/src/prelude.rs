//! Application-local prelude: conveniently import types/functions/macros
//! which are generally useful and should be available everywhere.

/// Application state accessors
pub use crate::application::APPLICATION;

/// Commonly used Abscissa traits
pub use abscissa_core::{Application, Command, Runnable};
