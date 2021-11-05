use std::io;

/// Extends [`Write`] with methods for writing Zcash/Bitcoin types.
///
/// [`Write`]: https://doc.rust-lang.org/std/io/trait.Write.html
pub trait WriteZcashExt: io::Write {
    /// Convenience method to write exactly 32 u8's.
    #[inline]
    fn write_32_bytes(&mut self, bytes: &[u8; 32]) -> io::Result<()> {
        self.write_all(bytes)
    }

    /// Convenience method to write exactly 64 u8's.
    #[inline]
    fn write_64_bytes(&mut self, bytes: &[u8; 64]) -> io::Result<()> {
        self.write_all(bytes)
    }
}

/// Mark all types implementing `Write` as implementing the extension.
impl<W: io::Write + ?Sized> WriteZcashExt for W {}
