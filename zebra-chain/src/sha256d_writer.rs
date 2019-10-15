//! A Writer for Sha256d-related (two rounds of SHA256) types.

use std::io::prelude::*;

use sha2::{Digest, Sha256};

/// A type that lets you write out SHA256d (double-SHA256, as in two rounds).
#[derive(Default)]
pub struct Sha256dWriter {
    hash: Sha256,
}

impl Sha256dWriter {
    /// Consume the Writer and produce the hash result.
    pub fn finish(self) -> [u8; 32] {
        let result1 = self.hash.result();
        let result2 = Sha256::digest(&result1);
        let mut buffer = [0u8; 32];
        buffer[0..32].copy_from_slice(&result2[0..32]);
        buffer
    }
}

impl Write for Sha256dWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hash.input(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        let mut length = 0;

        while length != buf.len() {
            length += buf.len();
            self.hash.input(buf);
        }

        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
