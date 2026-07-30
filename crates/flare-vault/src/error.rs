use thiserror::Error;

#[derive(Error, Debug)]
pub enum VaultError {
    #[error("vault not found at {0}")]
    NotFound(String),

    #[error("wrong passphrase or corrupt vault")]
    WrongPassphrase,

    #[error("secret '{0}' already exists")]
    SecretAlreadyExists(String),

    #[error("secret '{0}' not found")]
    SecretNotFound(String),

    #[error("vault is locked — unlock with the passphrase first")]
    Locked,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("vault already initialized at {0}")]
    AlreadyInitialized(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("{0}")]
    Other(String),
}

pub type VaultResult<T> = Result<T, VaultError>;
