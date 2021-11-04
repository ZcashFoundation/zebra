use std::{
    io,
    net::{IpAddr, SocketAddr},
};

use byteorder::{BigEndian, WriteBytesExt};

/// Extends [`Write`] with methods for writing Zcash/Bitcoin types.
///
/// [`Write`]: https://doc.rust-lang.org/std/io/trait.Write.html
pub trait WriteZcashExt: io::Write {
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
