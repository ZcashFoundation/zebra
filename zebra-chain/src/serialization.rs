//! Serialization for Zcash.
//!
//! This module contains four traits: `ZcashSerialize` and `ZcashDeserialize`,
//! analogs of the Serde `Serialize` and `Deserialize` traits but intended for
//! consensus-critical Zcash serialization formats, and `WriteZcashExt` and
//! `ReadZcashExt`, extension traits for `io::Read` and `io::Write` with utility functions
//! for reading and writing data (e.g., the Bitcoin variable-integer format).

use std::io;
use std::net::{IpAddr, SocketAddr};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt};
use thiserror::Error;

/// A serialization error.
// XXX refine error types -- better to use boxed errors?
#[derive(Error, Debug)]
pub enum SerializationError {
    /// An underlying IO error.
    #[error("io error")]
    Io(#[from] io::Error),
    /// The data to be deserialized was malformed.
    // XXX refine errors
    #[error("parse error: {0}")]
    Parse(&'static str),
}

/// Consensus-critical serialization for Zcash.
///
/// This trait provides a generic serialization for consensus-critical
/// formats, such as network messages, transactions, blocks, etc. It is intended
/// for use only in consensus-critical contexts; in other contexts, such as
/// internal storage, it would be preferable to use Serde.
pub trait ZcashSerialize: Sized {
    /// Write `self` to the given `writer` using the canonical format.
    ///
    /// This function has a `zcash_` prefix to alert the reader that the
    /// serialization in use is consensus-critical serialization, rather than
    /// some other kind of serialization.
    ///
    /// Notice that the error type is [`std::io::Error`]; this indicates that
    /// serialization MUST be infallible up to errors in the underlying writer.
    /// In other words, any type implementing `ZcashSerialize` must make illegal
    /// states unrepresentable.
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), io::Error>;
}

/// Consensus-critical serialization for Zcash.
///
/// This trait provides a generic deserialization for consensus-critical
/// formats, such as network messages, transactions, blocks, etc. It is intended
/// for use only in consensus-critical contexts; in other contexts, such as
/// internal storage, it would be preferable to use Serde.
pub trait ZcashDeserialize: Sized {
    /// Try to read `self` from the given `reader`.
    ///
    /// This function has a `zcash_` prefix to alert the reader that the
    /// serialization in use is consensus-critical serialization, rather than
    /// some other kind of serialization.
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError>;
}

impl<T: ZcashSerialize> ZcashSerialize for Vec<T> {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_compactsize(self.len() as u64)?;
        for x in self {
            x.zcash_serialize(&mut writer)?;
        }
        Ok(())
    }
}

impl<T: ZcashDeserialize> ZcashDeserialize for Vec<T> {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let len = reader.read_compactsize()?;
        // We're given len, so we could preallocate. But blindly preallocating
        // without a size bound can allow DOS attacks, and there's no way to
        // pass a size bound in a ZcashDeserialize impl, so instead we allocate
        // as we read from the reader. (The maximum block and transaction sizes
        // limit the eventual size of these allocations.)
        let mut vec = Vec::new();
        for _ in 0..len {
            vec.push(T::zcash_deserialize(&mut reader)?);
        }
        Ok(vec)
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
    /// use zebra_chain::serialization::WriteZcashExt;
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
            0x0000_00fd..=0x0000_ffff => {
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

    /// Write an `IpAddr` in Bitcoin format.
    #[inline]
    fn write_ip_addr(&mut self, addr: IpAddr) -> io::Result<()> {
        use std::net::IpAddr::*;
        let v6_addr = match addr {
            V4(ref v4) => v4.to_ipv6_mapped(),
            V6(v6) => v6,
        };
        self.write_all(&v6_addr.octets())
    }

    /// Write a `SocketAddr` in Bitcoin format.
    #[inline]
    fn write_socket_addr(&mut self, addr: SocketAddr) -> io::Result<()> {
        self.write_ip_addr(addr.ip())?;
        self.write_u16::<BigEndian>(addr.port())
    }

    /// Write a string in Bitcoin format.
    #[inline]
    fn write_string(&mut self, string: &str) -> io::Result<()> {
        self.write_compactsize(string.len() as u64)?;
        self.write_all(string.as_bytes())
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
    /// use zebra_chain::serialization::ReadZcashExt;
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
        use SerializationError::Parse;
        let flag_byte = self.read_u8()?;
        match flag_byte {
            n @ 0x00..=0xfc => Ok(n as u64),
            0xfd => match self.read_u16::<LittleEndian>()? {
                n @ 0x0000_00fd..=0x0000_ffff => Ok(n as u64),
                _ => Err(Parse("non-canonical compactsize")),
            },
            0xfe => match self.read_u32::<LittleEndian>()? {
                n @ 0x0001_0000..=0xffff_ffff => Ok(n as u64),
                _ => Err(Parse("non-canonical compactsize")),
            },
            0xff => match self.read_u64::<LittleEndian>()? {
                n @ 0x1_0000_0000..=0xffff_ffff_ffff_ffff => Ok(n),
                _ => Err(Parse("non-canonical compactsize")),
            },
        }
    }

    /// Read an IP address in Bitcoin format.
    #[inline]
    fn read_ip_addr(&mut self) -> io::Result<IpAddr> {
        use std::net::{IpAddr::*, Ipv6Addr};

        let mut octets = [0u8; 16];
        self.read_exact(&mut octets)?;
        let v6_addr = Ipv6Addr::from(octets);

        match v6_addr.to_ipv4() {
            Some(v4_addr) => Ok(V4(v4_addr)),
            None => Ok(V6(v6_addr)),
        }
    }

    /// Read a Bitcoin-encoded `SocketAddr`.
    #[inline]
    fn read_socket_addr(&mut self) -> io::Result<SocketAddr> {
        let ip_addr = self.read_ip_addr()?;
        let port = self.read_u16::<BigEndian>()?;
        Ok(SocketAddr::new(ip_addr, port))
    }

    /// Read a Bitcoin-encoded UTF-8 string.
    #[inline]
    fn read_string(&mut self) -> Result<String, SerializationError> {
        let len = self.read_compactsize()?;
        let mut buf = vec![0; len as usize];
        self.read_exact(&mut buf)?;
        String::from_utf8(buf).map_err(|_| SerializationError::Parse("invalid utf-8"))
    }

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

#[cfg(test)]
#[allow(clippy::unnecessary_operation)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    use std::io::Cursor;

    proptest! {
        // The tests below are cheap so we can run them a lot.
        #![proptest_config(ProptestConfig::with_cases(100_000))]

        #[test]
        fn compactsize_write_then_read_round_trip(s in 0u64..0x2_0000u64) {
            // Maximum encoding size of a compactsize is 9 bytes.
            let mut buf = [0u8; 8+1];
            Cursor::new(&mut buf[..]).write_compactsize(s).unwrap();
            let expect_s = Cursor::new(&buf[..]).read_compactsize().unwrap();
            prop_assert_eq!(s, expect_s);
        }

        #[test]
        fn compactsize_read_then_write_round_trip(bytes in prop::array::uniform9(0u8..)) {
            // Only do the test if the bytes were valid.
            if let Ok(s) = Cursor::new(&bytes[..]).read_compactsize() {
                // The compactsize encoding is variable-length, so we may not even
                // read all of the input bytes, and therefore we can't expect that
                // the encoding will reproduce bytes that were never read. Instead,
                // copy the input bytes, and overwrite them with the encoding of s,
                // so that if the encoding is different, we'll catch it on the part
                // that's written.
                let mut expect_bytes = bytes;
                Cursor::new(&mut expect_bytes[..]).write_compactsize(s).unwrap();
                prop_assert_eq!(bytes, expect_bytes);
            }
        }
    }
}
