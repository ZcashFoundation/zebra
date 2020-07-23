//! Utility functions for chain data.

/// Returns the hexadecimal-encoded string `s` in byte-reversed order.
pub fn byte_reverse_hex(s: &str) -> String {
    String::from_utf8(
        s.as_bytes()
            .chunks(2)
            .rev()
            .map(|c| c.iter())
            .flatten()
            .cloned()
            .collect::<Vec<u8>>(),
    )
    .expect("input should be ascii")
}
