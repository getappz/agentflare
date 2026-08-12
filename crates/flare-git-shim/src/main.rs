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
//!
//! Agent-only, same as `agentflare-shim`: every deny this shim can produce
//! exists to protect agentflare's own orchestration from AGENT-driven git
//! calls, not to restrict ordinary interactive human use. `main` bails out
//! to real git immediately when `classify::agent_invocation_detected()` is
//! false, before any classify/scope-check/audit work runs at all.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Tiered bypass, escape hatches for the dogfooding period (and beyond).
/// All three skip classification entirely and exec the real binary
/// unconditionally -- a misclassification must never be able to block
/// someone mid-work with no way out short of uninstalling the shim. Still
/// audited (as a distinct disposition), so a bypass is visible after the
/// fact, not silent.
const BYPASS_ENV: &str = "AGENTFLARE_GIT_BYPASS"; // one-shot: set at all -> bypass
const BYPASS_AGENT_ENV: &str = "AGENTFLARE_GIT_BYPASS_AGENT"; // bypass iff it equals AGENTFLARE_AGENT
const BYPASS_UNTIL_ENV: &str = "AGENTFLARE_GIT_BYPASS_UNTIL"; // bypass iff now < this unix epoch

/// `AGENTFLARE_GIT_SNAPSHOTS=0`/`off` disables the automatic pre-destructive
/// snapshot; any other value (or unset) leaves it enabled. Snapshotting is
/// a pure safety net (never blocks the underlying op even on failure), so
/// the default here is ON -- unlike the reference project, which defaults
/// it off in raw shim mode and on only inside its own launched sessions;
/// this shim has no separate "launched session" mode, so ON is the safer
/// default for a shim installed directly on someone's daily-driver PATH.
const SNAPSHOTS_ENV: &str = "AGENTFLARE_GIT_SNAPSHOTS";

/// Escape hatch for the canonical-repo HEAD-detach guard (see
/// `deny_canonical_detach_reason`) -- set to allow an agent-invoked
/// checkout/switch that would detach HEAD in the canonical (non-worktree)
/// checkout.
const ALLOW_CANONICAL_MUTATE_ENV: &str = "AGENTFLARE_GIT_ALLOW_CANONICAL_MUTATE";

/// Global flags that redirect git to operate on a different repo than the
/// one resolved via cwd (`-C`, `--git-dir`, `--work-tree`) -- denied
/// outright rather than classified, since this shim's policy resolves the
/// target repo from cwd and has no safe way to re-resolve against a
/// caller-supplied override.
const ESCAPE_HATCH_FLAGS: &[&str] = &["-C", "--git-dir", "--work-tree"];

/// Global flags that consume the following argument as their value, so
/// subcommand detection can skip past both tokens.
const GLOBAL_FLAGS_WITH_VALUE: &[&str] = &[
    "-c",
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
];

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
            || ESCAPE_HATCH_FLAGS
                .iter()
                .any(|f| a.starts_with(&format!("{f}=")))
        {
            escape_hatch = true;
        }
        i += if GLOBAL_FLAGS_WITH_VALUE.contains(&a.as_str()) {
            2
        } else {
            1
        };
    }
    (None, escape_hatch)
}

/// `true` if any bypass condition is currently active.
fn bypass_active() -> bool {
    if agentflare_shim::is_set(BYPASS_ENV) {
        return true;
    }
    if let Ok(target_agent) = env::var(BYPASS_AGENT_ENV)
        && !target_agent.is_empty()
        && env::var("AGENTFLARE_AGENT").ok().as_deref() == Some(target_agent.as_str())
    {
        return true;
    }
    if let Ok(until) = env::var(BYPASS_UNTIL_ENV)
        && let Ok(until_epoch) = until.parse::<u64>()
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now < until_epoch {
            return true;
        }
    }
    false
}

/// `false` only when explicitly disabled via `AGENTFLARE_GIT_SNAPSHOTS=0`/`off`.
fn snapshots_enabled() -> bool {
    match env::var(SNAPSHOTS_ENV) {
        Ok(v) => v != "0" && !v.eq_ignore_ascii_case("off"),
        Err(_) => true,
    }
}

/// Deny reason for canonical-checkout mutation guards (HEAD detach AND
/// branch-creation), or `None` to let the op through. Scoped tightly on
/// purpose: agent-invoked (self-reported via env markers, same as
/// `agentflare-shim`'s own gate) AND an agentflare-tracked project AND the
/// canonical (non-worktree) checkout AND the command would actually move
/// HEAD off its current branch (either by detaching, or by creating a new
/// branch and checking it out). Interactive human use, any use inside an
/// isolated worktree, and any project agentflare doesn't track, are all
/// completely unaffected.
fn deny_canonical_detach_reason(
    repo_root: &Path,
    subcommand: &str,
    args: &[String],
) -> Option<String> {
    if agentflare_shim::is_set(ALLOW_CANONICAL_MUTATE_ENV) {
        return None;
    }
    if !classify::agent_invocation_detected() {
        return None;
    }
    if !agentflare_shim::in_scoped_project(repo_root, dirs::home_dir().as_deref()) {
        return None;
    }
    if branch::is_linked_worktree(repo_root) {
        return None; // agent worktrees are exactly where this is expected
    }
    if classify::is_branch_create(subcommand, args) {
        return Some(format!(
            "this would create a new branch in the canonical checkout (not an isolated worktree) while agent-invoked -- feature work belongs in a worktree. Call `item(action=\"claim\", id=<item>)` to provision one, or set {ALLOW_CANONICAL_MUTATE_ENV}=1 to override."
        ));
    }
    if !classify::would_detach_head(repo_root, subcommand, args) {
        return None;
    }
    Some(format!(
        "this would detach HEAD in the canonical checkout (not an isolated worktree) while agent-invoked -- set {ALLOW_CANONICAL_MUTATE_ENV}=1 to override, or work in an isolated worktree instead."
    ))
}

#[derive(serde::Deserialize)]
struct ScopeCheckResult {
    deny: bool,
    reason: Option<String>,
}

/// Path-scope enforcement (item #234): shells out to `agentflare git
/// scope-check`, since this shim has no direct DB access to live claims.
/// Deliberately FAIL-CLOSED here, unlike the rest of this crate's fail-open
/// default -- any error resolving scope (binary missing, bad JSON,
/// non-zero exit) is treated as a deny. The existing bypass envs
/// (`AGENTFLARE_GIT_BYPASS` and friends, checked earlier in `main`) remain
/// the escape hatch for a broken/missing `agentflare` binary, same as any
/// other misclassification.
fn scope_check_deny_reason(subcommand: &str) -> Option<String> {
    let mut cmd = std::process::Command::new("agentflare");
    cmd.args(["git", "scope-check", "--subcommand", subcommand]);
    // Runs on every git invocation through this shim; without this flag,
    // whenever the invoking parent has no inherited console (e.g. spawned by
    // an IDE/editor extension), Windows auto-allocates one for this child —
    // visible as a brief flashing terminal window.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return Some(format!(
                "scope-check could not run ('agentflare' on PATH?): {e}"
            ));
        }
    };
    if !output.status.success() {
        return Some(format!(
            "scope-check exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: ScopeCheckResult = match serde_json::from_str(stdout.trim()) {
        Ok(r) => r,
        Err(e) => return Some(format!("scope-check returned unparseable output: {e}")),
    };
    if result.deny {
        Some(
            result
                .reason
                .unwrap_or_else(|| "scope-check denied (no reason given)".to_string()),
        )
    } else {
        None
    }
}

/// Hands off to the real git binary, first clearing `RECURSION_ENV`.
///
/// The depth counter must stay incremented for the remainder of *this*
/// process (so any internal `git` shell-outs this shim's own classify/
/// branch/snapshot logic makes still count toward the backstop) but once
/// we've decided to exec the real binary for the caller's actual command,
/// that subtree is a fresh, independent invocation -- e.g. an unrelated
/// `git` call later in the same long-lived test/build process, or a git
/// hook the real binary spawns. Without this reset those get charged
/// against a counter left over from a previous, unrelated hop, which is
/// exactly what tripped the guard on legitimate `cargo test --workspace`
/// runs (item #379).
fn exec_real(tool: &str, filtered_path: Option<&OsString>, args: &[OsString]) -> ! {
    // SAFETY: single-threaded at this point, immediately before exec.
    unsafe {
        env::remove_var(RECURSION_ENV);
    }
    run_real(tool, filtered_path, args)
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
        exec_real(&tool, filtered_path.as_ref(), &args);
    }

    // Agent-only gate, same catalog `agentflare-shim`'s own dispatcher and
    // `classify::agent_invocation_detected` already use -- this shim's whole
    // policy (protected-branch checkout/switch/delete/rename, trust-root
    // push, plumbing block, worktree block, scope-check) exists to protect
    // agentflare's own orchestration from AGENT-driven git calls. A regular
    // human interactively running git in a tracked repo must see zero
    // restrictions from this shim, not just the narrower canonical-detach
    // guard inside `classify` that already carried this same check alone.
    if !classify::agent_invocation_detected() {
        run_real(&tool, filtered_path.as_ref(), &args);
    }

    let cwd = env::current_dir().unwrap_or_default();
    let Some(repo_root) = branch::repo_toplevel(&cwd) else {
        // Not inside a git repo at all -- nothing to classify (e.g. `git
        // init`, `git clone` into a fresh directory, `git --version`).
        exec_real(&tool, filtered_path.as_ref(), &args);
    };

    if bypass_active() {
        if let Some(audit_path) = audit::default_path("git.jsonl") {
            let bypass_event = classify::Event {
                subcommand: "*".to_string(),
                args: args
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
                disposition: classify::Disposition::SilentExempt,
            };
            let _ = audit::log_event(&audit_path, &bypass_event);
        }
        exec_real(&tool, filtered_path.as_ref(), &args);
    }

    let str_args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (subcommand_idx, escape_hatch) = parse_global_flags(&str_args);

    if escape_hatch {
        eprintln!(
            "agentflare git shim: denied — this invocation uses -C/--git-dir/--work-tree to target a different repository, which this shim cannot classify safely."
        );
        exit(1);
    }

    let Some(idx) = subcommand_idx else {
        exec_real(&tool, filtered_path.as_ref(), &args); // e.g. bare `git --version`
    };
    let subcommand = str_args[idx].clone();
    let rest: Vec<String> = str_args[idx + 1..].to_vec();

    if let Some(reason) = deny_canonical_detach_reason(&repo_root, &subcommand, &rest) {
        let event = classify::Event {
            subcommand: subcommand.clone(),
            args: rest.clone(),
            disposition: classify::Disposition::Deny {
                reason: reason.clone(),
            },
        };
        if let Some(audit_path) = audit::default_path("git.jsonl") {
            let _ = audit::log_event(&audit_path, &event);
        }
        eprintln!("agentflare git shim: denied — {reason}");
        exit(1);
    }

    let event = classify::classify(&repo_root, &subcommand, &rest);

    if let Some(audit_path) = audit::default_path("git.jsonl") {
        let _ = audit::log_event(&audit_path, &event);
    }

    // Stranded-canonical-checkout recovery (item #441 / vent #386 residual):
    // a merged worktree leaves the canonical checkout unable to `git switch`
    // back to the default branch -- that's denied by classify_pure's
    // protected-branch guard. ALLOW_CANONICAL_MUTATE is the escape hatch for
    // exactly this class of canonical-checkout mutation, so when it's set in
    // the canonical checkout, downgrade a checkout/switch deny to passthrough.
    // Still audited (the original event was already logged above).
    let canonical_recovery = agentflare_shim::is_set(ALLOW_CANONICAL_MUTATE_ENV)
        && !branch::is_linked_worktree(&repo_root)
        && matches!(subcommand.as_str(), "checkout" | "switch");

    match &event.disposition {
        classify::Disposition::Deny { reason } if !canonical_recovery => {
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
        classify::Disposition::Deny { .. }
        | classify::Disposition::Passthrough
        | classify::Disposition::SilentExempt => {
            if matches!(subcommand.as_str(), "commit" | "push")
                && let Some(reason) = scope_check_deny_reason(&subcommand)
            {
                let scope_event = classify::Event {
                    subcommand: subcommand.clone(),
                    args: rest.clone(),
                    disposition: classify::Disposition::Deny {
                        reason: reason.clone(),
                    },
                };
                if let Some(audit_path) = audit::default_path("git.jsonl") {
                    let _ = audit::log_event(&audit_path, &scope_event);
                }
                eprintln!("agentflare git shim: denied — {reason}");
                exit(1);
            }
            if snapshots_enabled() && classify::is_destructive(&subcommand, &rest) {
                let reason = format!("pre-{subcommand} snapshot ({})", rest.join(" "));
                match snapshot::snapshot_before(&repo_root, &reason) {
                    Ok(id) => eprintln!(
                        "agentflare git shim: snapshotted before destructive '{subcommand}' (id {}) -- restore with `agentflare git snapshot restore`.",
                        id.0
                    ),
                    Err(e) => eprintln!(
                        "agentflare git shim: warning -- snapshot before destructive '{subcommand}' failed: {e}"
                    ),
                }
            }
            if let Some(msg) =
                classify::reset_soft_divergence_warning(&repo_root, &subcommand, &rest)
            {
                eprintln!("agentflare git shim: warning -- {msg}");
            }
            exec_real(&tool, filtered_path.as_ref(), &args);
        }
    }
}
