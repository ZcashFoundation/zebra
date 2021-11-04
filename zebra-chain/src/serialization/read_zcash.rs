use std::{
    io,
    net::{IpAddr, Ipv6Addr, SocketAddr},
};

use byteorder::{BigEndian, ReadBytesExt};

/// Extends [`Read`] with methods for writing Zcash/Bitcoin types.
///
/// [`Read`]: https://doc.rust-lang.org/std/io/trait.Read.html
pub trait ReadZcashExt: io::Read {
    /// Read an IP address in Bitcoin format.
    #[inline]
    fn read_ip_addr(&mut self) -> io::Result<IpAddr> {
        let mut octets = [0u8; 16];
        self.read_exact(&mut octets)?;
        let v6_addr = Ipv6Addr::from(octets);

        Ok(canonical_ip_addr(&v6_addr))
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

/// Transform a Zcash-deserialized IPv6 address into a canonical Zebra IP address.
///
/// Zcash uses IPv6-mapped IPv4 addresses in its network protocol. Zebra converts
/// those addresses to `Ipv4Addr`s, for maximum compatibility with systems that
/// don't understand IPv6.
pub fn canonical_ip_addr(v6_addr: &Ipv6Addr) -> IpAddr {
    use IpAddr::*;

    // TODO: replace with `to_ipv4_mapped` when that stabilizes
    // https://github.com/rust-lang/rust/issues/27709
    match v6_addr.to_ipv4() {
        // workaround for unstable `to_ipv4_mapped`
        Some(v4_addr) if v4_addr.to_ipv6_mapped() == *v6_addr => V4(v4_addr),
        Some(_) | None => V6(*v6_addr),
    }
}

/// Transform a `SocketAddr` into a canonical Zebra `SocketAddr`, converting
/// IPv6-mapped IPv4 addresses, and removing IPv6 scope IDs and flow information.
///
/// See [`canonical_ip_addr`] for detailed info on IPv6-mapped IPv4 addresses.
pub fn canonical_socket_addr(socket_addr: impl Into<SocketAddr>) -> SocketAddr {
    use SocketAddr::*;

    let mut socket_addr = socket_addr.into();
    if let V6(v6_socket_addr) = socket_addr {
        let canonical_ip = canonical_ip_addr(v6_socket_addr.ip());
        // creating a new SocketAddr removes scope IDs and flow information
        socket_addr = SocketAddr::new(canonical_ip, socket_addr.port());
    }

    socket_addr
}
