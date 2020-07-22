//! `revhex` subcommand - reverses hex endianness.

#![allow(clippy::never_loop)]

use abscissa_core::{Command, Options, Runnable};
use std::io::stdin;

/// `revhex` subcommand
#[derive(Command, Debug, Default, Options)]
pub struct RevhexCmd {
    /// The hex string whose endianness will be reversed.
    ///
    /// When input is "-" or empty, reads lines from standard input, and
    /// reverses each line.
    #[options(free)]
    input: String,
}

impl Runnable for RevhexCmd {
    /// Print endian-reversed hex string.
    fn run(&self) {
        if self.input.is_empty() || self.input == "-" {
            // "-" is a typical command-line argument for "read standard input"
            let mut input = String::new();
            // Unlike similar C APIs, read_line returns Ok(0) on EOF.
            // We can distinguish EOF from an empty line, because the newline is
            // included in the buffer, so empty lines return Ok(1).
            while stdin().read_line(&mut input).unwrap_or(0) > 0 {
                println!("{}", zebra_chain::utils::byte_reverse_hex(&input.trim()));
            }
        } else {
            println!("{}", zebra_chain::utils::byte_reverse_hex(&self.input));
        }
    }
}
