//! PATH shim impersonating `git`: classifies every invocation via
//! `flare_git_core::classify` before deciding whether to exec the real
//! binary, closing the gap where a raw `git commit`/`git checkout` run via
//! Bash bypasses agentflare's tool-call-level PreToolUse guard entirely.
//! Reuses `agentflare_shim`'s generic resolve-real-binary/exec/propagate-
//! exit-code core -- the same plumbing the lean-ctx shim uses, applied
//! here to a different dispatch target.
//!
//! Installed onto PATH as `git`/`git.exe` in the same shim directory
//! `agentflare-shim` already uses (see `agentflare git install-shim`).

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::exit;

use agentflare_shim::{path_without_shim_dir, run_real, tool_name_from_exe};
use flare_git_core::{audit, branch, classify, snapshot};

/// Recursion-depth backstop: incremented via an inherited env var on every
/// invocation. If this shim (or anything it spawns) ever resolves "git"
/// back to itself for any reason, this caps the blast radius at a handful
/// of processes instead of an unbounded spawn storm -- see
/// `flare_git_core::shell::git_binary`'s doc comment for the incident this
/// guards against. Independent of that fix: this backstop still applies if
/// some *other* internal call anywhere ever resolves "git" incorrectly.
const RECURSION_ENV: &str = "FLARE_GIT_SHIM_DEPTH";
const MAX_RECURSION_DEPTH: u32 = 3;

/// Escape hatch for the dogfooding period (and beyond): set to skip
/// classification entirely and exec the real binary unconditionally. A
/// misclassification must never be able to block someone mid-work with no
/// way out short of uninstalling the shim. Still audited (as a distinct
/// disposition), so a bypass is visible after the fact, not silent.
const BYPASS_ENV: &str = "AGENTFLARE_GIT_BYPASS";

/// Global flags that redirect git to operate on a different repo than the
/// one resolved via cwd (`-C`, `--git-dir`, `--work-tree`) -- denied
/// outright rather than classified, since this shim's policy resolves the
/// target repo from cwd and has no safe way to re-resolve against a
/// caller-supplied override.
const ESCAPE_HATCH_FLAGS: &[&str] = &["-C", "--git-dir", "--work-tree"];

/// Global flags that consume the following argument as their value, so
/// subcommand detection can skip past both tokens.
const GLOBAL_FLAGS_WITH_VALUE: &[&str] = &["-c", "-C", "--git-dir", "--work-tree", "--namespace", "--exec-path"];

/// Finds the subcommand token's index, skipping global flags (and their
/// values, for flags that take one). Also reports whether an escape-hatch
/// flag appeared before the subcommand.
fn parse_global_flags(args: &[String]) -> (Option<usize>, bool) {
    let mut i = 0;
    let mut escape_hatch = false;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            return (Some(i), escape_hatch);
        }
        if ESCAPE_HATCH_FLAGS.contains(&a.as_str())
            || ESCAPE_HATCH_FLAGS.iter().any(|f| a.starts_with(&format!("{f}=")))
        {
            escape_hatch = true;
        }
        i += if GLOBAL_FLAGS_WITH_VALUE.contains(&a.as_str()) { 2 } else { 1 };
    }
    (None, escape_hatch)
}

fn main() {
    let depth: u32 = env::var(RECURSION_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if depth >= MAX_RECURSION_DEPTH {
        eprintln!(
            "flare-git-shim: recursion guard tripped (depth {depth}) -- refusing to spawn further. This should never happen; please report it."
        );
        exit(1);
    }
    // SAFETY: single-threaded at this point in main(), before any spawn.
    unsafe {
        env::set_var(RECURSION_ENV, (depth + 1).to_string());
    }

    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("flare-git-shim: failed to determine executable path: {e}");
            exit(1);
        }
    };
    let Some(tool) = tool_name_from_exe(&exe) else {
        eprintln!("flare-git-shim: failed to determine tool name from executable path");
        exit(1);
    };
    let shim_dir: PathBuf = exe.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let filtered_path = path_without_shim_dir(&shim_dir);

    // Only actually policing `git` -- if this binary ever ends up
    // hardlinked/copied under another name (it shouldn't be), fall
    // straight through rather than guessing at a policy for it.
    if tool != "git" {
        run_real(&tool, filtered_path.as_ref(), &args);
    }

    let cwd = env::current_dir().unwrap_or_default();
    let Some(repo_root) = branch::repo_toplevel(&cwd) else {
        // Not inside a git repo at all -- nothing to classify (e.g. `git
        // init`, `git clone` into a fresh directory, `git --version`).
        run_real(&tool, filtered_path.as_ref(), &args);
    };

    if agentflare_shim::is_set(BYPASS_ENV) {
        if let Some(audit_path) = audit::default_path() {
            let bypass_event = classify::Event {
                subcommand: "*".to_string(),
                args: args.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
                disposition: classify::Disposition::SilentExempt,
            };
            let _ = audit::log_event(&audit_path, &bypass_event);
        }
        run_real(&tool, filtered_path.as_ref(), &args);
    }

    let str_args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    let (subcommand_idx, escape_hatch) = parse_global_flags(&str_args);

    if escape_hatch {
        eprintln!(
            "agentflare git shim: denied — this invocation uses -C/--git-dir/--work-tree to target a different repository, which this shim cannot classify safely."
        );
        exit(1);
    }

    let Some(idx) = subcommand_idx else {
        run_real(&tool, filtered_path.as_ref(), &args); // e.g. bare `git --version`
    };
    let subcommand = str_args[idx].clone();
    let rest: Vec<String> = str_args[idx + 1..].to_vec();

    let event = classify::classify(&repo_root, &subcommand, &rest);

    if let Some(audit_path) = audit::default_path() {
        let _ = audit::log_event(&audit_path, &event);
    }

    match &event.disposition {
        classify::Disposition::Deny { reason } => {
            eprintln!("agentflare git shim: denied — {reason}");
            exit(1);
        }
        classify::Disposition::RedirectToWorktree { path } => {
            // Unreachable in v1 (see classify.rs's doc comment) -- fail
            // closed with a clear message rather than pretending to
            // execute a redirect this policy version never produces.
            eprintln!(
                "agentflare git shim: internal error — classify() returned RedirectToWorktree({}), which this shim version does not implement executing.",
                path.display()
            );
            exit(1);
        }
        classify::Disposition::Passthrough | classify::Disposition::SilentExempt => {
            if classify::is_destructive(&subcommand, &rest) {
                let reason = format!("pre-{subcommand} snapshot ({})", rest.join(" "));
                match snapshot::snapshot_before(&repo_root, &reason) {
                    Ok(id) => eprintln!(
                        "agentflare git shim: snapshotted before destructive '{subcommand}' (id {}) -- restorable if this goes wrong.",
                        id.0
                    ),
                    Err(e) => eprintln!(
                        "agentflare git shim: warning -- snapshot before destructive '{subcommand}' failed: {e}"
                    ),
                }
            }
            run_real(&tool, filtered_path.as_ref(), &args);
        }
    }
}
