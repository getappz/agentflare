// engram (github.com/Gentleman-Programming/engram) is installed through mise's
// `github:` backend, which downloads the project's prebuilt release asset
// (engram_<ver>_<os>_<arch>.tar.gz/.zip) and verifies it against the release's
// own checksums.txt — no Go toolchain, no compile, no npm. It's the only mise
// backend we ship: unlike `go:`/`npm:`, it pulls in no language toolchain of
// its own. mise installs off-PATH, so callers wire an MCP server against the
// returned absolute path rather than expecting a bare `engram` to resolve —
// which also survives GUI-launched hosts that don't inherit ~/.local/bin on
// PATH.

/// mise backend spec for the engram binary. mise auto-scores the right release
/// asset per OS/arch, so no pattern is needed. See the module header.
const MISE_SPEC: &str = "github:Gentleman-Programming/engram";

/// Install engram through mise and return the absolute path to the binary.
/// Used where the binary must be referenced by full path (e.g. registering an
/// MCP server) rather than resolved off PATH.
pub fn install_via_mise(mise: &str) -> Result<String, String> {
    crate::mise_install::install_tool(mise, MISE_SPEC, "engram")
}

/// Whether engram is installed and resolvable through mise. Used instead of a
/// bare-PATH `engram version` check, since mise installs the binary off-PATH.
pub fn installed_via_mise() -> bool {
    crate::mise_install::mise_bin()
        .and_then(|m| crate::mise_install::which_tool(&m, "engram"))
        .is_some()
}
