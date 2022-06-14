//! Common [`zebra_test`](crate) types, traits, and functions.

pub use crate::command::{test_cmd, CommandExt, TestChild};
pub use std::process::Stdio;

pub use color_eyre;
pub use color_eyre::eyre;
pub use eyre::Result;
pub use proptest::prelude::*;
