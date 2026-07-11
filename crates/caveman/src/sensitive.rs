//! Sensitive-filename denylist — refuse to compress files that almost
//! certainly hold secrets or PII before their contents ever reach an LLM.

use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

static SENSITIVE_BASENAME_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)^(
            \.env(\..+)?
            |\.netrc
            |credentials(\..+)?
            |secrets?(\..+)?
            |passwords?(\..+)?
            |id_(rsa|dsa|ecdsa|ed25519)(\.pub)?
            |authorized_keys
            |known_hosts
            |.*\.(pem|key|p12|pfx|crt|cer|jks|keystore|asc|gpg)
        )$",
    )
    .unwrap()
});

const SENSITIVE_PATH_COMPONENTS: &[&str] = &[".ssh", ".aws", ".gnupg", ".kube", ".docker"];
const SENSITIVE_NAME_TOKENS: &[&str] = &[
    "secret",
    "credential",
    "password",
    "passwd",
    "apikey",
    "accesskey",
    "token",
    "privatekey",
];

pub fn is_sensitive_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if SENSITIVE_BASENAME_REGEX.is_match(name) {
        return true;
    }
    let has_sensitive_component = path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| SENSITIVE_PATH_COMPONENTS.contains(&s.to_lowercase().as_str()))
    });
    if has_sensitive_component {
        return true;
    }
    let normalized: String = name
        .to_lowercase()
        .chars()
        .filter(|c| !"_- .".contains(*c))
        .collect();
    SENSITIVE_NAME_TOKENS
        .iter()
        .any(|tok| normalized.contains(tok))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_dotenv() {
        assert!(is_sensitive_path(&PathBuf::from("/repo/.env")));
    }

    #[test]
    fn matches_credentials_json() {
        assert!(is_sensitive_path(&PathBuf::from("credentials.json")));
    }

    #[test]
    fn matches_api_key_with_hyphen_or_underscore() {
        assert!(is_sensitive_path(&PathBuf::from("api-key.txt")));
        assert!(is_sensitive_path(&PathBuf::from("api_key.txt")));
    }

    #[test]
    fn matches_ssh_path_component() {
        assert!(is_sensitive_path(&PathBuf::from(
            "/home/user/.ssh/id_ed25519"
        )));
    }

    #[test]
    fn ordinary_file_does_not_match() {
        assert!(!is_sensitive_path(&PathBuf::from("README.md")));
        assert!(!is_sensitive_path(&PathBuf::from("SKILL.md")));
    }
}
