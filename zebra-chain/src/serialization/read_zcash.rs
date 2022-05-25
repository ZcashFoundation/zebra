use std::io;

/// Extends [`Read`] with methods for writing Zcash/Bitcoin types.
///
/// [`Read`]: <https://doc.rust-lang.org/std/io/trait.Read.html>
pub trait ReadZcashExt: io::Read {
    /// Convenience method to read a `[u8; 4]`.
    #[inline]
    fn read_4_bytes(&mut self) -> io::Result<[u8; 4]> {
        let mut bytes = [0; 4];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Convenience method to read a `[u8; 12]`.
    #[inline]
    fn read_12_bytes(&mut self) -> io::Result<[u8; 12]> {
        let mut bytes = [0; 12];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Convenience method to read a `[u8; 32]`.
    #[inline]
    fn read_32_bytes(&mut self) -> io::Result<[u8; 32]> {
        let mut bytes = [0; 32];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Convenience method to read a `[u8; 64]`.
    #[inline]
    fn read_64_bytes(&mut self) -> io::Result<[u8; 64]> {
        let mut bytes = [0; 64];
        self.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

/// Mark all types implementing `Read` as implementing the extension.
impl<R: io::Read + ?Sized> ReadZcashExt for R {}
