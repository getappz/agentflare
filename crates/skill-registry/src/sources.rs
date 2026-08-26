use crate::SkillSpec;
use crate::SkillSource;
use crate::SourceReadError;
use crate::SourceWriteError;
use std::path::Path;
use std::path::PathBuf;

pub fn read_entry<P: AsRef<Path>>(path: P) -> Result<SkillSpec, SourceReadError> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)
        .map_err(|e| SourceReadError::Io { path: path.to_path_buf(), source: e })?;
    let spec: SkillSpec = toml::from_str(&content)
        .map_err(|e| SourceReadError::Parse { path: path.to_path_buf(), source: e })?;
    Ok(spec)
}

pub fn write_entry<P: AsRef<Path>>(path: P, spec: &SkillSpec) -> Result<(), SourceWriteError> {
    let path = path.as_ref();
    let content = toml::to_string_pretty(spec)
        .map_err(|e| SourceWriteError::Serialize { source: e })?;
    std::fs::write(path, content)
        .map_err(|e| SourceWriteError::Io { path: path.to_path_buf(), source: e })?;
    Ok(())
}

pub fn read_all_entries<P: AsRef<Path>>(dir: P) -> Result<Vec<SkillSpec>, SourceReadError> {
    let dir = dir.as_ref();
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| SourceReadError::Io { path: dir.to_path_buf(), source: e })?
    {
        let entry = entry.map_err(|e| SourceReadError::Io { path: dir.to_path_buf(), source: e })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            entries.push(read_entry(&path)?);
        }
    }
    Ok(entries)
}

pub fn source_from_path<P: AsRef<Path>>(path: P) -> SkillSource {
    SkillSource::File { path: path.as_ref().to_path_buf() }
}