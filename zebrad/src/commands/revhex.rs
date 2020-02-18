//! `revhex` subcommand - reverses hex endianness.

#![allow(clippy::never_loop)]

use abscissa_core::{Command, Options, Runnable};

/// `revhex` subcommand
#[derive(Command, Debug, Default, Options)]
pub struct RevhexCmd {
    /// The hex string whose endianness will be reversed.
    #[options(free)]
    input: String,
}

impl Runnable for RevhexCmd {
    /// Print endian-reversed hex string.
    fn run(&self) {
        println!(
            "{}",
            String::from_utf8(
                self.input
                    .as_bytes()
                    .chunks(2)
                    .rev()
                    .map(|c| c.iter())
                    .flatten()
                    .cloned()
                    .collect::<Vec<u8>>(),
            )
            .expect("input should be ascii")
        );
    }
}
