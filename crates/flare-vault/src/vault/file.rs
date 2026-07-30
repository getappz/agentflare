use crate::error::VaultResult;
use fs2::FileExt;
use std::io::{Read, Write};
use std::path::Path;

// File locking here only prevents torn reads/writes of a single file
// operation (and `write_vault_file`'s atomic rename means concurrent writers
// never corrupt the file). It does NOT make a read-modify-write cycle
// (read body, mutate, write body) atomic across processes -- two processes
// racing a `set_secret`/`remove_secret` at the same time can still lose one
// update. Acceptable for a local single-user CLI vault; would need real
// cross-process locking (held for the whole cycle) if this ever grows a
// concurrent-writer use case.

pub fn read_vault_file(path: &Path) -> VaultResult<Vec<u8>> {
    let file = std::fs::File::open(path)
        .map_err(|e| crate::error::VaultError::Other(format!("open vault for read: {e}")))?;
    file.lock_shared()
        .map_err(|e| crate::error::VaultError::Other(format!("lock vault for read: {e}")))?;
    let mut buf = Vec::new();
    let mut file = file;
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

pub fn write_vault_file(path: &Path, data: &[u8]) -> VaultResult<()> {
    let tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(|e| crate::error::VaultError::Other(format!("create temp file: {e}")))?;

    let file = std::fs::File::create(tmp.path())
        .map_err(|e| crate::error::VaultError::Other(format!("create vault temp: {e}")))?;
    file.lock_exclusive()
        .map_err(|e| crate::error::VaultError::Other(format!("lock vault temp: {e}")))?;
    let mut file = file;
    file.write_all(data)?;
    file.flush()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    }

    tmp.persist(path)
        .map_err(|e| crate::error::VaultError::Other(format!("rename temp to vault: {e}")))?;

    Ok(())
}

pub fn vault_file_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.vault");
        let data = b"hello vault";
        write_vault_file(&path, data).unwrap();
        assert!(vault_file_exists(&path));
        let read = read_vault_file(&path).unwrap();
        assert_eq!(read, data);
    }

    #[test]
    fn missing_file_error() {
        let path = dirs::home_dir()
            .unwrap()
            .join("__nonexistent_vault_test__.vault");
        let err = read_vault_file(&path);
        assert!(err.is_err());
    }
}
