/// A service combinator that sends requests to a first service, then retries
/// processing on a second fallback service if the first service errors.
///
/// TODO: similar code exists in linkerd and could be upstreamed into tower
pub mod future;
mod service;

pub use self::service::Fallback;
pub use either::Either;
