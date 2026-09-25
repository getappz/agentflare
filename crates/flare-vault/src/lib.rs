#![allow(clippy::incompatible_msrv)]

// `insecure-fast-kdf` swaps the Argon2id defaults for the cheapest parameters
// Argon2 accepts so vault tests run in milliseconds. It exists for test builds
// only; refuse to compile a release binary with it on.
#[cfg(all(feature = "insecure-fast-kdf", not(debug_assertions)))]
compile_error!(
    "flare-vault feature `insecure-fast-kdf` is test-only and must not be enabled in a release build"
);

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
