//! Error type for the table store domain model.

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("table '{0}' already exists")]
    TableAlreadyExists(String),
    #[error("table '{0}' not found")]
    TableNotFound(String),
    #[error("entity already exists")]
    EntityAlreadyExists,
    #[error("entity not found")]
    EntityNotFound,
    #[error("etag mismatch")]
    ETagMismatch,
}

pub type CoreResult<T> = Result<T, CoreError>;
