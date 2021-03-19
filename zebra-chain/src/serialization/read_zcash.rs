use std::io;
use std::net::{IpAddr, SocketAddr};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt};

use super::SerializationError;

/// Extends [`Read`] with methods for writing Zcash/Bitcoin types.
///
/// [`Read`]: https://doc.rust-lang.org/std/io/trait.Read.html
pub trait ReadZcashExt: io::Read {
    /// Reads a `u64` using the Bitcoin `CompactSize` encoding.
    ///
    /// # Security
    ///
    /// Deserialized sizes must be validated before being used.
    ///
    /// Preallocating vectors using untrusted `CompactSize`s allows memory
    /// denial of service attacks. Valid sizes must be less than
    /// `MAX_BLOCK_BYTES / min_serialized_item_bytes` (or a lower limit
    /// specified by the Zcash consensus rules or Bitcoin network protocol).
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
