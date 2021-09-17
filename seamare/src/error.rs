use std::path::PathBuf;
use thiserror::Error;

pub type Result<T, E = SeamareError> = std::result::Result<T, E>;

#[derive(Error, Debug)]
pub enum SeamareError {
    #[error("Loading path failed")]
    IO(#[source] std::io::Error),
    #[error("unable to build package graph")]
    Guppy(#[source] guppy::Error),
    #[error("Detect invalid UTF-8 path, get {0}")]
    NotValidUtf8Path(PathBuf),
}
