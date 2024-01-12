//! Functions for modifying byte arrays.

/// Increments `byte_array` by 1, interpreting it as a big-endian integer.
/// If the big-endian integer overflowed, sets all the bytes to zero, and returns `true`.
pub fn increment_big_endian(byte_array: &mut [u8]) -> bool {
    // Increment the last byte in the array that is less than u8::MAX, and clear any bytes after it
    // to increment the next value in big-endian (lexicographic) order.
    let is_wrapped_overflow = byte_array.iter_mut().rev().all(|v| {
        *v = v.wrapping_add(1);
        v == &0
    });

    is_wrapped_overflow
}
