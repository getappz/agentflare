use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl Shell {
    pub fn detect() -> Option<Self> {
        if let Ok(shell_path) = std::env::var("SHELL") {
            let name = shell_path.rsplit('/').next().unwrap_or(&shell_path);
            if let Some(s) = Self::from_name(name) {
                return Some(s);
            }
        }
        if std::env::var("ZSH_VERSION").is_ok() {
            return Some(Self::Zsh);
        }
        if std::env::var("BASH_VERSION").is_ok() {
            return Some(Self::Bash);
        }
        if std::env::var("FISH_VERSION").is_ok() {
            return Some(Self::Fish);
        }
        if cfg!(windows) {
            return Some(Self::PowerShell);
        }
        Some(Self::Bash)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            "powershell" | "pwsh" => Some(Self::PowerShell),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::PowerShell => "powershell",
        }
    }

    pub fn profile_path(&self) -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        Some(match self {
            Self::Bash => home.join(".bashrc"),
            Self::Zsh => home.join(".zshrc"),
            Self::Fish => home.join(".config").join("fish").join("config.fish"),
            Self::PowerShell => {
                let ps7 = home
                    .join("Documents")
                    .join("PowerShell")
                    .join("Microsoft.PowerShell_profile.ps1");
                if ps7.parent().is_some_and(|p| p.exists()) {
                    ps7
                } else {
                    home.join("Documents")
                        .join("WindowsPowerShell")
                        .join("Microsoft.PowerShell_profile.ps1")
                }
            }
        })
    }

    pub fn alias_line(&self, name: &str, target: &str) -> String {
        match self {
            Self::Bash | Self::Zsh => format!("alias {name}='{target}'"),
            Self::Fish => format!("alias {name} '{target}'"),
            Self::PowerShell => format!("function {name} {{ {target} @args }}"),
        }
    }

    pub fn is_defined_in_profile(&self, profile_content: &str, name: &str) -> bool {
        let stripped = strip_managed_blocks(profile_content);
        match self {
            Self::Bash | Self::Zsh => stripped.contains(&format!("alias {name}=")),
            Self::Fish => stripped.contains(&format!("alias {name} ")),
            Self::PowerShell => {
                stripped.contains(&format!("function {name}"))
                    || stripped.contains(&format!("function {name}("))
            }
        }
    }
}

fn strip_managed_blocks(content: &str) -> String {
    let mut out = String::new();
    let mut in_block = false;
    for line in content.lines() {
        if line.contains(">>> agentflare alias >>>") {
            in_block = true;
            continue;
        }
        if line.contains("<<< agentflare alias <<<") {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_parses_all_variants() {
        assert_eq!(Shell::from_name("bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_name("BASH"), Some(Shell::Bash));
        assert_eq!(Shell::from_name("zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_name("fish"), Some(Shell::Fish));
        assert_eq!(Shell::from_name("powershell"), Some(Shell::PowerShell));
        assert_eq!(Shell::from_name("pwsh"), Some(Shell::PowerShell));
        assert_eq!(Shell::from_name("nushell"), None);
    }

    #[test]
    fn alias_line_per_shell() {
        assert_eq!(
            Shell::Bash.alias_line("af", "agentflare"),
            "alias af='agentflare'"
        );
        assert_eq!(
            Shell::Zsh.alias_line("af", "agentflare"),
            "alias af='agentflare'"
        );
        assert_eq!(
            Shell::Fish.alias_line("af", "agentflare"),
            "alias af 'agentflare'"
        );
        assert_eq!(
            Shell::PowerShell.alias_line("af", "agentflare"),
            "function af { agentflare @args }"
        );
    }

    #[test]
    fn is_defined_in_profile_detects_alias() {
        let content = "alias af='something'\nsome other stuff\n";
        assert!(Shell::Bash.is_defined_in_profile(content, "af"));
        assert!(!Shell::Bash.is_defined_in_profile(content, "agf"));
    }

    #[test]
    fn is_defined_in_profile_ignores_own_managed_block() {
        let content =
            "# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n";
        assert!(!Shell::Bash.is_defined_in_profile(content, "af"));
    }

    #[test]
    fn is_defined_in_profile_detects_alias_outside_block() {
        let content = "alias af='other'\n# >>> agentflare alias >>>\nalias af='agentflare'\n# <<< agentflare alias <<<\n";
        assert!(Shell::Bash.is_defined_in_profile(content, "af"));
    }

    #[test]
    fn powershell_detects_function() {
        let content = "function af { something @args }\n";
        assert!(Shell::PowerShell.is_defined_in_profile(content, "af"));
        assert!(!Shell::PowerShell.is_defined_in_profile(content, "agf"));
    }

    #[test]
    fn powershell_detects_function_with_parens() {
        let content = "function af($args) { something }\n";
        assert!(Shell::PowerShell.is_defined_in_profile(content, "af"));
    }

    #[test]
    fn fish_detects_alias() {
        let content = "alias af 'something'\n";
        assert!(Shell::Fish.is_defined_in_profile(content, "af"));
        assert!(!Shell::Fish.is_defined_in_profile(content, "agf"));
    }

    #[test]
    fn strip_managed_blocks_preserves_content_outside() {
        let content =
            "line1\n# >>> agentflare alias >>>\ninside\n# <<< agentflare alias <<<\nline2\n";
        let stripped = strip_managed_blocks(content);
        assert!(stripped.contains("line1"));
        assert!(stripped.contains("line2"));
        assert!(!stripped.contains("inside"));
    }

    #[test]
    fn name_returns_lowercase() {
        assert_eq!(Shell::Bash.name(), "bash");
        assert_eq!(Shell::PowerShell.name(), "powershell");
    }
}
