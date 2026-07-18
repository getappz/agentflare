use crate::shell::Shell;
use serde::Serialize;
use std::path::{Path, PathBuf};

const BLOCK_START: &str = "# >>> agentflare alias >>>";
const BLOCK_END: &str = "# <<< agentflare alias <<<";
const FALLBACK_CHAIN: &[&str] = &["af", "agf", "afl", "agentf"];

#[derive(Serialize)]
struct JsonOutput {
    requested: String,
    installed: String,
    status: &'static str,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug)]
enum Status {
    Ok,
    FallbackUsed,
    AlreadyInstalled,
    UnknownShell,
    NoProfile,
    WriteError,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::FallbackUsed => "fallback_used",
            Self::AlreadyInstalled => "already_installed",
            Self::UnknownShell => "unknown_shell",
            Self::NoProfile => "no_profile",
            Self::WriteError => "error",
        }
    }
}

pub fn run(
    preferred: Option<String>,
    force: bool,
    print_only: bool,
    yes: bool,
    shell_name: Option<String>,
    profile_path: Option<String>,
    json: bool,
) {
    let preferred = preferred.as_deref().unwrap_or("af");

    let shell = match resolve_shell(shell_name.as_deref()) {
        Ok(s) => s,
        Err(name) => {
            let err = format!("unknown shell: {name}");
            if !json {
                crate::ui::error(&err);
                std::process::exit(1);
            }
            emit_json(JsonOutput {
                requested: preferred.to_string(),
                installed: String::new(),
                status: Status::UnknownShell.as_str(),
                profile: String::new(),
                snippet: None,
                error: Some(err),
            });
            return;
        }
    };

    let profile = match resolve_profile(profile_path.as_deref(), shell) {
        Ok(p) => p,
        Err(()) => {
            let err = format!("cannot determine profile path for {}", shell.name());
            if !json {
                crate::ui::error(&err);
                std::process::exit(1);
            }
            emit_json(JsonOutput {
                requested: preferred.to_string(),
                installed: String::new(),
                status: Status::NoProfile.as_str(),
                profile: String::new(),
                snippet: None,
                error: Some(err),
            });
            return;
        }
    };

    // Distinguish "file doesn't exist yet" (fine — we'll create it) from a real
    // read failure. Collapsing both to None via .ok() would treat an unreadable
    // existing profile as empty and then overwrite the user's real shell profile
    // with just the managed block, destroying its contents.
    let existing_content = match std::fs::read_to_string(&profile) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            let err = format!("cannot read profile {}: {e}", profile.display());
            if !json {
                crate::ui::error(&err);
                std::process::exit(1);
            }
            emit_json(JsonOutput {
                requested: preferred.to_string(),
                installed: String::new(),
                status: Status::WriteError.as_str(),
                profile: profile.to_string_lossy().into_owned(),
                snippet: None,
                error: Some(err),
            });
            return;
        }
    };

    if !force
        && let Some(ref content) = existing_content
        && let Some(existing_alias) = read_managed_block_alias(content)
        && existing_alias == shell.alias_line(preferred, "agentflare")
    {
        if !json && !print_only {
            println!(
                "alias '{preferred}' already installed in {}",
                profile.display()
            );
        }
        if json {
            emit_json(JsonOutput {
                requested: preferred.to_string(),
                installed: preferred.to_string(),
                status: Status::AlreadyInstalled.as_str(),
                profile: profile.to_string_lossy().into_owned(),
                snippet: None,
                error: None,
            });
        }
        return;
    }

    let name = resolve_name(preferred, force, shell, existing_content.as_deref());
    let alias_line = shell.alias_line(&name, "agentflare");

    let status = if name == preferred {
        Status::Ok
    } else {
        Status::FallbackUsed
    };

    if print_only {
        if json {
            emit_json(JsonOutput {
                requested: preferred.to_string(),
                installed: name,
                status: status.as_str(),
                profile: profile.to_string_lossy().into_owned(),
                snippet: Some(alias_line),
                error: None,
            });
        } else {
            println!("{alias_line}");
        }
        return;
    }

    if !json && !yes {
        let msg = if name != preferred {
            format!(
                "'{preferred}' is occupied — will use '{name}' instead. Add alias to {}?",
                profile.display()
            )
        } else {
            format!("Add alias '{name}' to {}?", profile.display())
        };
        if !crate::ui::confirm(&msg, true) {
            return;
        }
    }

    match write_managed_block(&profile, &alias_line, existing_content.as_deref()) {
        Ok(()) => {
            if !json {
                match status {
                    Status::Ok => println!(
                        "alias '{name}' added to {}. Restart your shell or run: source {}",
                        profile.display(),
                        profile.display()
                    ),
                    Status::FallbackUsed => println!(
                        "'{preferred}' was occupied, installed '{name}' instead in {}. Restart your shell or run: source {}",
                        profile.display(),
                        profile.display()
                    ),
                    _ => unreachable!(),
                }
            }
            if json {
                emit_json(JsonOutput {
                    requested: preferred.to_string(),
                    installed: name,
                    status: status.as_str(),
                    profile: profile.to_string_lossy().into_owned(),
                    snippet: None,
                    error: None,
                });
            }
        }
        Err(e) => {
            let err_msg = format!("cannot write to {}: {e}", profile.display());
            if !json {
                eprintln!("error: {err_msg}");
                std::process::exit(1);
            }
            emit_json(JsonOutput {
                requested: preferred.to_string(),
                installed: String::new(),
                status: Status::WriteError.as_str(),
                profile: profile.to_string_lossy().into_owned(),
                snippet: None,
                error: Some(err_msg),
            });
        }
    }
}

fn emit_json(output: JsonOutput) {
    println!("{}", serde_json::to_string(&output).unwrap_or_default());
}

fn resolve_shell(shell_name: Option<&str>) -> Result<Shell, String> {
    match shell_name {
        Some(name) => Shell::from_name(name).ok_or_else(|| name.to_string()),
        None => Shell::detect().ok_or_else(|| "could not detect shell".to_string()),
    }
}

fn resolve_profile(profile_path: Option<&str>, shell: Shell) -> Result<PathBuf, ()> {
    match profile_path {
        Some(p) => Ok(PathBuf::from(p)),
        None => shell.profile_path().ok_or(()),
    }
}

#[cfg(test)]
fn is_no(response: &str) -> bool {
    let r = response.to_lowercase();
    r == "n" || r == "no"
}

fn resolve_name(
    preferred: &str,
    force: bool,
    shell: Shell,
    profile_content: Option<&str>,
) -> String {
    if force {
        return preferred.to_string();
    }

    let mut candidates: Vec<&str> = vec![preferred];
    for &name in FALLBACK_CHAIN {
        if name != preferred {
            candidates.push(name);
        }
    }
    if preferred != "agentflare" {
        candidates.push("agentflare");
    }

    for name in candidates {
        let on_path = agent_registry::detect::find_binary(&[name]).is_some();
        let in_profile = profile_content.is_some_and(|c| shell.is_defined_in_profile(c, name));
        if !on_path && !in_profile {
            return name.to_string();
        }
    }

    "agentflare".to_string()
}

fn read_managed_block_alias(content: &str) -> Option<String> {
    let start = content.find(BLOCK_START)?;
    let after_start = &content[start..];
    let end_rel = after_start.find(BLOCK_END)?;
    let body = &after_start[BLOCK_START.len()..end_rel].trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

fn write_managed_block(
    profile: &Path,
    alias_line: &str,
    existing: Option<&str>,
) -> std::io::Result<()> {
    if let Some(parent) = profile.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = existing.unwrap_or("");
    let new_block = format!("{BLOCK_START}\n{alias_line}\n{BLOCK_END}\n");

    if let Some(start) = content.find(BLOCK_START)
        && let Some(end_rel) = content[start..].find(BLOCK_END)
    {
        let mut end = start + end_rel + BLOCK_END.len();
        if content[end..].starts_with('\n') {
            end += 1;
        }
        let new_content = content[..start].to_string() + &new_block + &content[end..];
        crate::atomic_fs::write_bytes_with_fallback(profile, new_content.as_bytes(), None)
            .map_err(std::io::Error::other)?;
        return Ok(());
    }

    let mut new_content = content.to_string();
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&new_block);
    crate::atomic_fs::write_bytes_with_fallback(profile, new_content.as_bytes(), None)
        .map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static ALIAS_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_profile(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agentflare-test-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn resolve_name_returns_preferred_when_free() {
        // preferred not on PATH, not in profile
        let name = resolve_name("myalias", false, Shell::Bash, None);
        assert_eq!(name, "myalias");
    }

    #[test]
    fn resolve_name_falls_back_when_occupied_in_profile() {
        let content = "alias myalias='something'\n";
        let name = resolve_name("myalias", false, Shell::Bash, Some(content));
        // Should fall through to agf (assuming agf is not on PATH or in profile)
        assert!(FALLBACK_CHAIN.contains(&name.as_str()) || name == "agentflare");
    }

    #[test]
    fn resolve_name_force_bypasses_check() {
        let content = "alias af='something'\n";
        let name = resolve_name("af", true, Shell::Bash, Some(content));
        assert_eq!(name, "af");
    }

    #[test]
    fn read_managed_block_alias_finds_content() {
        let content =
            "# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n";
        assert_eq!(
            read_managed_block_alias(content),
            Some("alias af='agentflare'".to_string())
        );
    }

    #[test]
    fn read_managed_block_alias_returns_none_when_no_block() {
        assert_eq!(read_managed_block_alias("some stuff"), None);
    }

    #[test]
    fn write_managed_block_inserts_new_block() {
        let _guard = ALIAS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let profile = temp_profile("test_insert.sh");
        write_managed_block(&profile, "alias af='agentflare'", None).unwrap();
        let content = std::fs::read_to_string(&profile).unwrap();
        assert!(content.contains(BLOCK_START));
        assert!(content.contains("alias af='agentflare'"));
        assert!(content.contains(BLOCK_END));
    }

    #[test]
    fn write_managed_block_replaces_existing_block() {
        let _guard = ALIAS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let profile = temp_profile("test_replace.sh");
        std::fs::write(
            &profile,
            "# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n",
        )
        .unwrap();

        write_managed_block(
            &profile,
            "alias agf='agentflare'",
            Some(&std::fs::read_to_string(&profile).unwrap()),
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile).unwrap();
        assert!(!content.contains("alias af='agentflare'"));
        assert!(content.contains("alias agf='agentflare'"));
        assert!(content.contains(BLOCK_START));
        assert!(content.contains(BLOCK_END));
    }

    #[test]
    fn write_managed_block_idempotent() {
        let _guard = ALIAS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let profile = temp_profile("test_idempotent.sh");
        let original =
            "# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n";
        std::fs::write(&profile, original).unwrap();

        write_managed_block(
            &profile,
            "alias af='agentflare'",
            Some(&std::fs::read_to_string(&profile).unwrap()),
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile).unwrap();
        // After replace, the block is still there with same content
        assert!(content.contains("alias af='agentflare'"));
    }

    #[test]
    fn write_managed_block_preserves_content_after_block() {
        let _guard = ALIAS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let profile = temp_profile("test_after.sh");
        std::fs::write(
            &profile,
            "# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n# some other stuff\n",
        )
        .unwrap();

        write_managed_block(
            &profile,
            "alias agf='agentflare'",
            Some(&std::fs::read_to_string(&profile).unwrap()),
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile).unwrap();
        assert!(content.contains("# some other stuff"));
        assert!(!content.contains("alias af='agentflare'"));
        assert!(content.contains("alias agf='agentflare'"));
    }

    #[test]
    fn write_managed_block_preserves_content_before_block() {
        let _guard = ALIAS_FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let profile = temp_profile("test_before.sh");
        let original = "export PATH=...\n# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n";
        std::fs::write(&profile, original).unwrap();

        write_managed_block(
            &profile,
            "alias agf='agentflare'",
            Some(&std::fs::read_to_string(&profile).unwrap()),
        )
        .unwrap();

        let content = std::fs::read_to_string(&profile).unwrap();
        assert!(content.starts_with("export PATH=..."));
    }

    #[test]
    fn is_no_recognizes_variations() {
        assert!(is_no("n"));
        assert!(is_no("N"));
        assert!(is_no("no"));
        assert!(is_no("NO"));
        assert!(!is_no("y"));
        assert!(!is_no("yes"));
        assert!(!is_no(""));
    }
}
