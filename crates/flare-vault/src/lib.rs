#![allow(clippy::incompatible_msrv)]

pub mod crypto;
pub mod error;
pub mod inject;
pub mod paths;
pub mod session;
pub mod vault;

pub use error::{VaultError, VaultResult};
pub use vault::manager::{
    create_vault, get_secret_value, list_secret_names, merge_secrets, open_vault,
    open_vault_with_dek, read_vault_body, remove_secret_value, set_secret_value, write_vault_body,
    VaultPaths,
};
pub use vault::model::{SecretEntry, VaultBody, VaultFile};
