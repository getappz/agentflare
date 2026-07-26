//! Which package registry a lookup targets.
//!
//! Kept as a small enum rather than a trait: there are two variants, dispatch
//! happens once per request, and the fetch shapes differ enough (a single
//! zstd-compressed JSON document versus a manifest plus a tarball) that a
//! common trait would be a lowest-common-denominator abstraction over two
//! genuinely different protocols.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ecosystem {
    /// Rust crates, documented via docs.rs rustdoc-JSON.
    #[default]
    Rust,
    /// npm packages, documented from their TypeScript declaration files.
    Npm,
}

impl Ecosystem {
    /// Parses an explicit `ecosystem` argument. Accepts the common aliases an
    /// agent is likely to produce unprompted.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" | "cargo" | "crates" | "crates.io" | "docsrs" | "docs.rs" => {
                Some(Self::Rust)
            }
            "npm" | "node" | "nodejs" | "js" | "javascript" | "ts" | "typescript" => {
                Some(Self::Npm)
            }
            _ => None,
        }
    }

    /// Resolves the ecosystem for a request.
    ///
    /// An explicit argument always wins. Otherwise a scoped name (`@scope/pkg`)
    /// is unambiguously npm — no other supported registry uses that form.
    /// Everything else defaults to Rust, preserving the behaviour of every
    /// call written before npm support existed.
    pub fn resolve(explicit: Option<&str>, package: &str) -> Result<Self, String> {
        if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
            return Self::parse(raw)
                .ok_or_else(|| format!("unknown ecosystem \"{raw}\" (expected rust|npm)"));
        }
        if package.starts_with('@') {
            return Ok(Self::Npm);
        }
        Ok(Self::Rust)
    }

    /// Store path prefix for a package's cached documents.
    pub fn docs_id_path(&self, package: &str, version: &str) -> String {
        match self {
            Self::Rust => crate::rustdoc::docs_id_path(package, version),
            Self::Npm => crate::npm::docs_id_path(package, version),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Npm => "npm",
        }
    }

    /// Hint appended to a "not found" error, so an agent that guessed the
    /// wrong registry is told the fix instead of concluding the package does
    /// not exist.
    pub fn other_ecosystem_hint(&self, package: &str) -> String {
        match self {
            Self::Rust => format!(
                "\"{package}\" was not found on docs.rs — if it is a Node package, retry with ecosystem=\"npm\""
            ),
            Self::Npm => format!(
                "\"{package}\" was not found on npm — if it is a Rust crate, retry with ecosystem=\"rust\""
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_aliases_an_agent_is_likely_to_emit() {
        for s in ["rust", "RS", " cargo ", "crates.io", "docs.rs"] {
            assert_eq!(Ecosystem::parse(s), Some(Ecosystem::Rust), "{s}");
        }
        for s in ["npm", "Node", "nodejs", "js", "TypeScript"] {
            assert_eq!(Ecosystem::parse(s), Some(Ecosystem::Npm), "{s}");
        }
        assert_eq!(Ecosystem::parse("pypi"), None);
    }

    #[test]
    fn omitted_ecosystem_defaults_to_rust_for_back_compat() {
        // Every flare_docs call written before npm support omitted the field
        // and meant a crate; that must keep working unchanged.
        assert_eq!(Ecosystem::resolve(None, "serde").unwrap(), Ecosystem::Rust);
        assert_eq!(
            Ecosystem::resolve(Some(""), "tokio").unwrap(),
            Ecosystem::Rust
        );
    }

    #[test]
    fn scoped_names_are_unambiguously_npm() {
        assert_eq!(
            Ecosystem::resolve(None, "@types/node").unwrap(),
            Ecosystem::Npm
        );
        assert_eq!(
            Ecosystem::resolve(None, "@babel/core").unwrap(),
            Ecosystem::Npm
        );
    }

    #[test]
    fn an_explicit_argument_overrides_the_scoped_name_inference() {
        assert_eq!(
            Ecosystem::resolve(Some("rust"), "@types/node").unwrap(),
            Ecosystem::Rust
        );
    }

    #[test]
    fn an_unrecognised_ecosystem_is_rejected_rather_than_silently_defaulted() {
        // Silently treating ecosystem="pypi" as Rust would return confusing
        // "crate not found" errors for a request that was never supported.
        let err = Ecosystem::resolve(Some("pypi"), "requests").unwrap_err();
        assert!(err.contains("pypi"), "{err}");
        assert!(err.contains("rust|npm"), "{err}");
    }

    #[test]
    fn each_ecosystem_owns_a_distinct_store_prefix() {
        assert_eq!(
            Ecosystem::Rust.docs_id_path("serde", "latest"),
            "docsrs/serde/latest"
        );
        assert_eq!(
            Ecosystem::Npm.docs_id_path("hono", "4.6.3"),
            "npm/hono/4.6.3"
        );
    }

    #[test]
    fn miss_hints_point_at_the_other_registry() {
        assert!(
            Ecosystem::Rust
                .other_ecosystem_hint("express")
                .contains("ecosystem=\"npm\"")
        );
        assert!(
            Ecosystem::Npm
                .other_ecosystem_hint("serde")
                .contains("ecosystem=\"rust\"")
        );
    }
}
