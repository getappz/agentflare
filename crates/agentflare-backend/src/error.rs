#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("duplicate: {0}")]
    Duplicate(String),
    #[error("invalid state transition: {0}")]
    InvalidTransition(String),
    #[error("validation: {0}")]
    Validation(String),
    #[error(transparent)]
    Database(rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        match &e {
            rusqlite::Error::SqliteFailure(err, _) => {
                if err.code == rusqlite::ErrorCode::ConstraintViolation {
                    let ext = err.extended_code;
                    if ext == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        || ext == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                    {
                        return Error::Duplicate(e.to_string());
                    }
                }
                Error::Database(e)
            }
            _ => Error::Database(e),
        }
    }
}
