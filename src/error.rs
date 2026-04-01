use std::io;

#[derive(Debug, thiserror::Error)]
pub enum OpenSlideError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Format error: {0}")]
    Format(String),

    #[error("Decode error: {0}")]
    Decode(String),

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}

pub type Result<T> = std::result::Result<T, OpenSlideError>;
