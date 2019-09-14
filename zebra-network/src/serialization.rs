//! Serializat

use std::io;

use crate::types::{Magic, Version};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

/// A serialization error.
// XXX refine error types -- better to use boxed errors?
#[derive(Fail, Debug)]
pub enum SerializationError {
    /// An underlying IO error.
    #[fail(display = "io error {}", _0)]
    IoError(io::Error),
    /// The data to be deserialized was malformed.
    #[fail(display = "parse error")]
    ParseError,
}

// Allow upcasting io::Error to SerializationError
impl From<io::Error> for SerializationError {
    fn from(e: io::Error) -> Self {
        Self::IoError(e)
    }
}

/// Extends [`Write`] with methods for writing Zcash/Bitcoin types.
///
/// [`Write`]: https://doc.rust-lang.org/std/io/trait.Write.html
pub trait WriteZcashExt: io::Write {
    /// Writes a `u64` using the Bitcoin `CompactSize` encoding.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use zebra_network::serialization::WriteZcashExt;
    ///
    /// let mut buf = Vec::new();
    /// buf.write_compactsize(0x12).unwrap();
    /// assert_eq!(buf, b"\x12");
    ///
    /// let mut buf = Vec::new();
    /// buf.write_compactsize(0xfd).unwrap();
    /// assert_eq!(buf, b"\xfd\xfd\x00");
    ///
    /// let mut buf = Vec::new();
    /// buf.write_compactsize(0xaafd).unwrap();
    /// assert_eq!(buf, b"\xfd\xfd\xaa");
    ///
    /// let mut buf = Vec::new();
    /// buf.write_compactsize(0xbbaafd).unwrap();
    /// assert_eq!(buf, b"\xfe\xfd\xaa\xbb\x00");
    ///
    /// let mut buf = Vec::new();
    /// buf.write_compactsize(0x22ccbbaafd).unwrap();
    /// assert_eq!(buf, b"\xff\xfd\xaa\xbb\xcc\x22\x00\x00\x00");
    /// ```
    #[inline]
    fn write_compactsize(&mut self, n: u64) -> io::Result<()> {
        match n {
            0x0000_0000..=0x0000_00fc => self.write_u8(n as u8),
            0x0000_00fd..=0x0001_0000 => {
                self.write_u8(0xfd)?;
                self.write_u16::<LittleEndian>(n as u16)
            }
            0x0001_0000..=0xffff_ffff => {
                self.write_u8(0xfe)?;
                self.write_u32::<LittleEndian>(n as u32)
            }
            _ => {
                self.write_u8(0xff)?;
                self.write_u64::<LittleEndian>(n)
            }
        }
    }
}

/// Mark all types implementing `Write` as implementing the extension.
impl<W: io::Write + ?Sized> WriteZcashExt for W {}

/// Extends [`Read`] with methods for writing Zcash/Bitcoin types.
///
/// [`Read`]: https://doc.rust-lang.org/std/io/trait.Read.html
pub trait ReadZcashExt: io::Read {
    /// Reads a `u64` using the Bitcoin `CompactSize` encoding.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use zebra_network::serialization::ReadZcashExt;
    ///
    /// use std::io::Cursor;
    /// assert_eq!(
    ///     0x12,
    ///     Cursor::new(b"\x12")
    ///         .read_compactsize().unwrap()
    /// );
    /// assert_eq!(
    ///     0xfd,
    ///     Cursor::new(b"\xfd\xfd\x00")
    ///         .read_compactsize().unwrap()
    /// );
    /// assert_eq!(
    ///     0xaafd,
    ///     Cursor::new(b"\xfd\xfd\xaa")
    ///         .read_compactsize().unwrap()
    /// );
    /// assert_eq!(
    ///     0xbbaafd,
    ///     Cursor::new(b"\xfe\xfd\xaa\xbb\x00")
    ///         .read_compactsize().unwrap()
    /// );
    /// assert_eq!(
    ///     0x22ccbbaafd,
    ///     Cursor::new(b"\xff\xfd\xaa\xbb\xcc\x22\x00\x00\x00")
    ///         .read_compactsize().unwrap()
    /// );
    /// ```
    #[inline]
    fn read_compactsize(&mut self) -> Result<u64, SerializationError> {
        let flag_byte = self.read_u8()?;
        match flag_byte {
            0xff => Ok(self.read_u64::<LittleEndian>()?),
            0xfe => Ok(self.read_u32::<LittleEndian>()? as u64),
            0xfd => Ok(self.read_u16::<LittleEndian>()? as u64),
            n => Ok(n as u64),
        }
    }
}

/// Mark all types implementing `Read` as implementing the extension.
impl<R: io::Read + ?Sized> ReadZcashExt for R {}
