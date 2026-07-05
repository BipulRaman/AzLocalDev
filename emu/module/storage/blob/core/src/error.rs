//! Error type for the blob store domain model.

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("container '{0}' already exists")]
    ContainerAlreadyExists(String),
    #[error("container '{0}' not found")]
    ContainerNotFound(String),
    #[error("blob '{0}' not found")]
    BlobNotFound(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
