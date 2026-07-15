#[derive(Debug, thiserror::Error)]
pub enum CavemanError {
    #[error("source file not found: {0}")]
    NotFound(String),
    #[error("file too large to compress safely (max 500KB): {0}")]
    TooLarge(String),
    #[error("refusing to compress {0}: filename looks sensitive")]
    Sensitive(String),
    #[error("refusing to compress: {0} is empty or whitespace-only")]
    Empty(String),
    #[error("backup already exists: {0}")]
    BackupExists(String),
    #[error("backup write verification failed: {0}")]
    BackupVerifyFailed(String),
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("compression aborted: LLM returned an empty response")]
    EmptyResponse,
    #[error("compression aborted: output identical to input")]
    IdenticalOutput,
    #[error("validation failed after {0} attempts: {1:?}")]
    ValidationFailed(u32, Vec<String>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
