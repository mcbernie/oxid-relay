//! Core error types shared across the workspace.

use thiserror::Error;

/// Convenience result alias for core operations.
pub type Result<T> = std::result::Result<T, CoreError>;

/// Errors that can occur inside the core domain logic.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A mail was rejected because it failed validation.
    #[error("invalid mail: {0}")]
    InvalidMail(String),

    /// The requested mail was not found in the queue.
    #[error("mail not found: {0}")]
    NotFound(String),

    /// A transport failed to deliver a mail.
    #[error("transport error: {0}")]
    Transport(String),

    /// The queue backend reported an error.
    #[error("queue error: {0}")]
    Queue(String),
}
