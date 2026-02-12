//! Trait for converting types to their byte representation for display or RPC use.
///
/// This trait provides methods to access and construct types from their
/// internal serialized byte order (typically little-endian) as well as their
/// big-endian byte order used for display and RPC.
///
/// Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
pub trait BytesInDisplayOrder<
    const SHOULD_REVERSE_BYTES_IN_DISPLAY_ORDER: bool = false,
    const BYTE_LEN: usize = 32,
>: Sized
{
    /// Returns the bytes in the internal serialized order.
    fn bytes_in_serialized_order(&self) -> [u8; BYTE_LEN];

    /// Creates an instance from bytes in the internal serialized order.
    fn from_bytes_in_serialized_order(bytes: [u8; BYTE_LEN]) -> Self;

    /// Alias for [`BytesInDisplayOrder::bytes_in_serialized_order`].
    fn bytes(&self) -> [u8; BYTE_LEN] {
        self.bytes_in_serialized_order()
    }

    /// Converts the bytes in the internal serialized order to the expected type.
    fn bytes_into<T: From<[u8; BYTE_LEN]>>(&self) -> T {
        self.bytes_in_serialized_order().into()
    }

    /// Return the bytes in big-endian byte-order suitable for printing out byte by byte.
    fn bytes_in_display_order(&self) -> [u8; BYTE_LEN] {
        let mut reversed_bytes = self.bytes_in_serialized_order();
        if SHOULD_REVERSE_BYTES_IN_DISPLAY_ORDER {
            reversed_bytes.reverse();
        }
        reversed_bytes
    }

    /// Convert bytes in big-endian byte-order into big-endian display order.
    fn from_bytes_in_display_order(bytes_in_display_order: &[u8; BYTE_LEN]) -> Self {
        let mut internal_byte_order = *bytes_in_display_order;
        if SHOULD_REVERSE_BYTES_IN_DISPLAY_ORDER {
            internal_byte_order.reverse();
        }
        Self::from_bytes_in_serialized_order(internal_byte_order)
    }
}
