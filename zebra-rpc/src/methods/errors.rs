//! Error conversions for Zebra's RPC methods.

use jsonrpsee_types::ErrorObject;

pub(crate) trait MapServerError<T, E> {
    fn map_server_error(self) -> std::result::Result<T, ErrorObject<'static>>;
}

pub(crate) trait OkOrServerError<T> {
    fn ok_or_server_error<S: ToString>(
        self,
        message: S,
    ) -> std::result::Result<T, ErrorObject<'static>>;
}

impl<T, E> MapServerError<T, E> for Result<T, E>
where
    E: ToString,
{
    fn map_server_error(self) -> Result<T, ErrorObject<'static>> {
        self.map_err(|error| ErrorObject::owned(0, error.to_string(), None::<()>))
    }
}

impl<T> OkOrServerError<T> for Option<T> {
    fn ok_or_server_error<S: ToString>(self, message: S) -> Result<T, ErrorObject<'static>> {
        self.ok_or(ErrorObject::owned(0, message.to_string(), None::<()>))
    }
}
