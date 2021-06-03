//! Common [`zebra_test`] types, traits, and functions.

pub use crate::command::{test_cmd, CommandExt, TestChild};
pub use std::process::Stdio;

pub use color_eyre;
pub use color_eyre::eyre;
pub use eyre::Result;
pub use pretty_assertions::{assert_eq, assert_ne};
pub use proptest::prelude::*;
