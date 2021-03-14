use thiserror::Error;

pub type Result<T, E = SeamareError> = std::result::Result<T, E>;

#[derive(Error, Debug)]
pub enum SeamareError {
    #[error("loading {path:?} failed, reason: {detail:?}")]
    IO {
        path: String,
        detail: std::io::Error,
    },
    #[error("Build package graph failed")]
    Guppy(#[from] guppy::Error),
}
