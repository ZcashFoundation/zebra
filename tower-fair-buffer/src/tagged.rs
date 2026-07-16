//! A request envelope that pairs a request with an optional caller key.

/// A request tagged with the caller it came from.
///
/// [`FairBuffer`](crate::FairBuffer) uses the `key` to track each caller's
/// recent request count, which decides the request's place in the queue:
///
/// - `Some(key)`: an external request, prioritized by the caller's recent
///   request count, and shed before quieter callers' requests when the buffer
///   is full.
/// - `None`: an internal request, always processed with priority 0 (before all
///   external requests), and never shed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tagged<K, R> {
    /// The key identifying the caller, or `None` for internal requests.
    pub key: Option<K>,

    /// The wrapped request.
    pub request: R,
}

impl<K, R> Tagged<K, R> {
    /// Returns an internal request, which has priority 0 and is never shed.
    pub fn internal(request: R) -> Self {
        Tagged { key: None, request }
    }

    /// Returns an external request, prioritized by `key`'s recent request count.
    pub fn from_peer(key: K, request: R) -> Self {
        Tagged {
            key: Some(key),
            request,
        }
    }
}
