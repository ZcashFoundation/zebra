//! SHA256d, a.k.a., double SHA2, a.k.a., 2 SHA 2 Furious

use std::{fmt, io::prelude::*};

use sha2::{Digest, Sha256};

/// An `io::Write` instance that produces a SHA256d output.
#[derive(Default)]
pub struct Writer {
    hash: Sha256,
}

impl Writer {
    /// Consume the Writer and produce the hash result.
    pub fn finish(self) -> [u8; 32] {
        let result1 = self.hash.finalize();
        let result2 = Sha256::digest(&result1);
        let mut buffer = [0u8; 32];
        buffer[0..32].copy_from_slice(&result2[0..32]);
        buffer
    }
}

impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hash.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A 4-byte checksum using truncated double-SHA256 (two rounds of SHA256).
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Checksum(pub [u8; 4]);

impl<'a> From<&'a [u8]> for Checksum {
    fn from(bytes: &'a [u8]) -> Self {
        let hash1 = Sha256::digest(bytes);
        let hash2 = Sha256::digest(&hash1);
        let mut checksum = [0u8; 4];
        checksum[0..4].copy_from_slice(&hash2[0..4]);
        Self(checksum)
    }
}

impl fmt::Debug for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Sha256dChecksum")
            .field(&hex::encode(self.0))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256d_checksum() {
        let _init_guard = zebra_test::init();

        // https://en.bitcoin.it/wiki/Protocol_documentation#Hashes
        let input = b"hello";
        let checksum = Checksum::from(&input[..]);
        let expected = Checksum([0x95, 0x95, 0xc9, 0xdf]);
        assert_eq!(checksum, expected);
    }

    #[test]
    fn sha256d_checksum_debug() {
        let _init_guard = zebra_test::init();

        let input = b"hello";
        let checksum = Checksum::from(&input[..]);

        assert_eq!(format!("{checksum:?}"), "Sha256dChecksum(\"9595c9df\")");
    }
}
