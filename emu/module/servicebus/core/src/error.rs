use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("entity not found")]
    EntityNotFound,
    #[error("no message available")]
    NoMessage,
    #[error("lock token not found or expired")]
    LockLost,
    #[error("sequence number not found")]
    SequenceNotFound,
    #[error("actor channel closed")]
    ActorGone,
}

pub type CoreResult<T> = Result<T, CoreError>;
