//! Trait for converting types to their byte representation for display or RPC use.
///
/// This trait provides methods to access and construct types from their
/// internal serialized byte order (typically little-endian) as well as their
/// big-endian byte order used for display and RPC.
pub trait BytesInDisplayOrder: Sized {
    /// Returns the bytes in the internal serialized order.
    fn bytes_in_serialized_order(&self) -> [u8; 32];

    /// Creates an instance from bytes in the internal serialized order.
    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self;

    /// Return the bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays transaction and block hashes in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut reversed_bytes = self.bytes_in_serialized_order();
        reversed_bytes.reverse();
        reversed_bytes
    }

    /// Convert bytes in big-endian byte-order into big-endian display order.
    ///
    /// Zebra displays transaction and block hashes in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    fn from_bytes_in_display_order(bytes_in_display_order: &[u8; 32]) -> Self {
        let mut internal_byte_order = *bytes_in_display_order;
        internal_byte_order.reverse();
        Self::from_bytes_in_serialized_order(internal_byte_order)
    }
}
