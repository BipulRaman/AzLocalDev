//! Error type for the queue store domain model.

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("queue '{0}' already exists")]
    QueueAlreadyExists(String),
    #[error("queue '{0}' not found")]
    QueueNotFound(String),
    #[error("message '{0}' not found")]
    MessageNotFound(String),
    #[error("pop receipt mismatch for message '{0}'")]
    PopReceiptMismatch(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
