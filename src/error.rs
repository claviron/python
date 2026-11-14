//! What the embedded interpreter and its applications fail with.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "invalid ASGI target '{target}': expected non-empty module:attribute"
    )]
    InvalidTarget { target: String },
    #[error("Python module path does not exist: {0}")]
    MissingModulePath(PathBuf),
    #[error("Python virtual environment does not exist: {0}")]
    MissingVirtualEnvironment(PathBuf),
    #[error("Python application failed: {0}")]
    Python(String),
    #[error("Python application thread stopped before reporting startup")]
    StartupChannelClosed,
    #[error("Python application thread panicked")]
    ThreadPanicked,
    #[error("Python application is no longer accepting requests")]
    ApplicationStopped,
    #[error("Python application stopped before completing the HTTP response")]
    ResponseChannelClosed,
    #[error("invalid ASGI HTTP response: {0}")]
    InvalidHttpResponse(String),
}
