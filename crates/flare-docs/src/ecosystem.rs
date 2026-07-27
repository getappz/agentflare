//! Which package registry a lookup targets.
//!
//! Kept as a small enum rather than a trait: there are three variants,
//! dispatch happens once per request, and the fetch shapes differ enough (a
//! single zstd-compressed JSON document, a manifest plus a tarball, or a
//! manifest plus a wheel) that a common trait would be a lowest-common-
//! denominator abstraction over genuinely different protocols.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ecosystem {
    /// Rust crates, documented via docs.rs rustdoc-JSON.
    #[default]
    Rust,
    /// npm packages, documented from their TypeScript declaration files.
    Npm,
    /// Python packages, documented from their PEP 561 type stubs (`.pyi`),
    /// with a typeshed (`types-<package>` on PyPI) fallback.
    Python,
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
            "python" | "py" | "pypi" | "pip" => Some(Self::Python),
            _ => None,
        }
    }

    /// Resolves the ecosystem for a request.
    ///
    /// An explicit argument always wins. Otherwise a scoped name (`@scope/pkg`)
    /// is unambiguously npm — no other supported registry uses that form.
    /// Everything else defaults to Rust, preserving the behaviour of every
    /// call written before npm (and later Python) support existed. Python
    /// package names have no comparable structural marker, so — like an
    /// unscoped npm package — they always need an explicit `ecosystem`.
    pub fn resolve(explicit: Option<&str>, package: &str) -> Result<Self, String> {
        if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
            return Self::parse(raw).ok_or_else(|| {
                format!("unknown ecosystem \"{raw}\" (expected rust|npm|python)")
            });
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
            Self::Python => crate::python::docs_id_path(package, version),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Npm => "npm",
            Self::Python => "python",
        }
    }

    /// Hint appended to a "not found" error, so an agent that guessed the
    /// wrong registry is told the fix instead of concluding the package does
    /// not exist.
    pub fn other_ecosystem_hint(&self, package: &str) -> String {
        match self {
            Self::Rust => format!(
                "\"{package}\" was not found on docs.rs — if it is a Node package, retry with ecosystem=\"npm\"; if it is a Python package, retry with ecosystem=\"python\""
            ),
            Self::Npm => format!(
                "\"{package}\" was not found on npm — if it is a Rust crate, retry with ecosystem=\"rust\"; if it is a Python package, retry with ecosystem=\"python\""
            ),
            Self::Python => format!(
                "\"{package}\" was not found on PyPI — if it is a Rust crate, retry with ecosystem=\"rust\"; if it is a Node package, retry with ecosystem=\"npm\""
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
        for s in ["python", "Py", "pypi", "PIP"] {
            assert_eq!(Ecosystem::parse(s), Some(Ecosystem::Python), "{s}");
        }
        assert_eq!(Ecosystem::parse("rubygems"), None);
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
    fn python_has_no_auto_detection_heuristic_and_needs_an_explicit_argument() {
        // Unlike npm's `@scope/pkg`, Python package names have no structural
        // marker — an unscoped name still defaults to Rust, same as an
        // unscoped npm package always has.
        assert_eq!(
            Ecosystem::resolve(None, "requests").unwrap(),
            Ecosystem::Rust
        );
        assert_eq!(
            Ecosystem::resolve(Some("python"), "requests").unwrap(),
            Ecosystem::Python
        );
    }

    #[test]
    fn an_unrecognised_ecosystem_is_rejected_rather_than_silently_defaulted() {
        // Silently treating ecosystem="rubygems" as Rust would return confusing
        // "crate not found" errors for a request that was never supported.
        let err = Ecosystem::resolve(Some("rubygems"), "nokogiri").unwrap_err();
        assert!(err.contains("rubygems"), "{err}");
        assert!(err.contains("rust|npm|python"), "{err}");
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
        assert_eq!(
            Ecosystem::Python.docs_id_path("requests", "2.32.3"),
            "pypi/requests/2.32.3"
        );
    }

    #[test]
    fn miss_hints_point_at_the_other_registries() {
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
        assert!(
            Ecosystem::Python
                .other_ecosystem_hint("serde")
                .contains("ecosystem=\"rust\"")
        );
        assert!(
            Ecosystem::Rust
                .other_ecosystem_hint("requests")
                .contains("ecosystem=\"python\"")
        );
    }
}
