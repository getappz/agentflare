//! Build/cache directory detection: a directory is an artifact only when
//! its project marker sits beside it, it holds no tracked files, and
//! nothing is building in the project.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{Item, Kind, ScanInput, Skipped, par_map};
use crate::shell::{run_in, run_in_ok};
use crate::worktree::dir_size;
use agentflare_config::paths;
use flare_process::cwd::LiveProc;

pub struct Rule {
    pub kind: &'static str,
    /// File names beside the directory; `*.ext` matches by suffix. Empty
    /// means the directory name alone is unmistakable.
    pub markers: &'static [&'static str],
    pub dirs: &'static [&'static str],
}

pub const RULES: &[Rule] = &[
    Rule {
        kind: "rust",
        markers: &["Cargo.toml"],
        dirs: &["target"],
    },
    Rule {
        kind: "node",
        markers: &["package.json"],
        dirs: &[
            "node_modules",
            "dist",
            "build",
            "out",
            ".next",
            ".nuxt",
            ".turbo",
            ".svelte-kit",
            ".parcel-cache",
            "coverage",
        ],
    },
    Rule {
        kind: "python",
        markers: &[],
        dirs: &["__pycache__"],
    },
    Rule {
        kind: "python",
        markers: &["pyproject.toml", "setup.py", "requirements.txt"],
        dirs: &[
            ".venv",
            ".pytest_cache",
            ".mypy_cache",
            ".ruff_cache",
            ".tox",
        ],
    },
    Rule {
        kind: "jvm",
        markers: &["pom.xml"],
        dirs: &["target"],
    },
    Rule {
        kind: "jvm",
        markers: &["build.gradle", "build.gradle.kts"],
        dirs: &["build", ".gradle"],
    },
    Rule {
        kind: "dotnet",
        markers: &["*.csproj", "*.sln"],
        dirs: &["bin", "obj"],
    },
    Rule {
        kind: "swift",
        markers: &["Package.swift"],
        dirs: &[".build"],
    },
    Rule {
        kind: "zig",
        markers: &["build.zig"],
        dirs: &["zig-cache", ".zig-cache", "zig-out"],
    },
    Rule {
        kind: "dart",
        markers: &["pubspec.yaml"],
        dirs: &[".dart_tool", "build"],
    },
    Rule {
        kind: "elixir",
        markers: &["mix.exs"],
        dirs: &["_build", "deps"],
    },
    Rule {
        kind: "terraform",
        markers: &["*.tf"],
        dirs: &[".terraform"],
    },
];

/// Names that are also ordinary source directories: these additionally
/// have to be git-ignored.
const AMBIGUOUS: &[&str] = &["dist", "build", "out", "bin", "obj", "coverage"];

/// An idle shell sitting in a project is not a build.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "nu",
    "pwsh",
    "powershell",
    "cmd",
    "tmux",
    "screen",
];

/// Every kind name, for validating `--artifacts=<kinds>`.
#[must_use]
pub fn kinds() -> Vec<&'static str> {
    let mut k: Vec<&str> = RULES.iter().map(|r| r.kind).collect();
    k.dedup();
    k
}

fn has_marker(parent: &Path, markers: &[&str]) -> bool {
    if markers.is_empty() {
        return true;
    }
    let (suffixes, exact): (Vec<&str>, Vec<&str>) =
        markers.iter().partition(|m| m.starts_with('*'));
    if exact.iter().any(|m| parent.join(m).is_file()) {
        return true;
    }
    !suffixes.is_empty()
        && std::fs::read_dir(parent)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                suffixes.iter().any(|s| name.ends_with(&s[1..]))
            })
}

fn match_kind(parent: &Path, name: &str, kinds: &[String]) -> Option<&'static str> {
    RULES
        .iter()
        .filter(|r| kinds.is_empty() || kinds.iter().any(|k| k == r.kind))
        .find(|r| r.dirs.contains(&name) && has_marker(parent, r.markers))
        .map(|r| r.kind)
}

/// Artifact directories under `root`, by name and marker only. Never
/// descends into a match, into `.git`, or into a trash directory; never
/// follows a symlink or leaves `root`'s filesystem.
pub(super) fn find_dirs(root: &Path, kinds: &[String]) -> Vec<(PathBuf, &'static str)> {
    let mut found = Vec::new();
    let mut walk = walkdir::WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
        .into_iter();
    while let Some(entry) = walk.next() {
        let Ok(entry) = entry else { continue };
        // `file_type()` is the entry's own type: a symlink to a directory is
        // not a directory here, so it is neither matched nor entered.
        if !entry.file_type().is_dir() || entry.depth() == 0 {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name == ".git" || name == ".trash" {
            walk.skip_current_dir();
            continue;
        }
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        if let Some(kind) = match_kind(parent, &name, kinds) {
            // Its own repository (a deploy clone in `dist/`, say) can hold
            // commits that exist nowhere else: never an artifact.
            if std::fs::symlink_metadata(entry.path().join(".git")).is_err() {
                found.push((entry.path().to_path_buf(), kind));
            }
            walk.skip_current_dir();
        }
    }
    found
}

/// A process that is plausibly building in `project`: cwd inside it, not
/// under a nested `.worktrees` (those are separate checkouts), not a shell.
pub(super) fn builder<'a>(project: &Path, live: &'a [LiveProc]) -> Option<&'a LiveProc> {
    let nested = project.join(".worktrees");
    live.iter().find(|p| {
        p.cwd.starts_with(project)
            && !p.cwd.starts_with(&nested)
            && !SHELLS.contains(&p.name.trim_end_matches(".exe"))
    })
}

/// Newest mtime of the directory and its direct children: cheap, and a
/// build touches the top level.
fn newest_mtime(dir: &Path) -> Option<SystemTime> {
    let modified = |m: std::io::Result<std::fs::Metadata>| m.and_then(|m| m.modified()).ok();
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| modified(e.metadata()))
        .chain(modified(std::fs::metadata(dir)))
        .max()
}

fn human_age(age: Duration) -> String {
    match age.as_secs() / 86_400 {
        0 => format!("{}h old", age.as_secs() / 3600),
        days => format!("{days}d old"),
    }
}

/// Artifact candidates under the scan root, after the safety filters.
pub(super) fn scan(input: &ScanInput, kinds: &[String]) -> (Vec<Item>, Vec<Skipped>) {
    let root = paths::canonical(input.scan_root);
    let label = |p: &Path| {
        p.strip_prefix(&root)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut skipped = Vec::new();
    let mut kept: Vec<(PathBuf, &'static str, Duration)> = Vec::new();

    for (path, kind) in find_dirs(&root, kinds) {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            continue;
        };
        // Asked of the nearest enclosing repo, so a nested repo answers for
        // its own files.
        let in_repo = run_in_ok(parent, &["rev-parse", "--is-inside-work-tree"]);
        if in_repo && !run_in(parent, &["ls-files", "--", name]).is_ok_and(|o| o.is_empty()) {
            continue; // holds tracked files (or git could not say): not an artifact
        }
        if AMBIGUOUS.contains(&name)
            && !(in_repo && run_in_ok(parent, &["check-ignore", "-q", "--", name]))
        {
            continue;
        }
        let age = newest_mtime(&path)
            .and_then(|t| t.elapsed().ok())
            .unwrap_or_default();
        if input.opts.older_than.is_some_and(|min| age < min) {
            continue;
        }
        let user = builder(parent, input.live)
            .or_else(|| input.live.iter().find(|p| p.cwd.starts_with(&path)));
        if let Some(p) = user {
            skipped.push(Skipped {
                label: label(&path),
                reason: format!("in use: {} (pid {})", p.name, p.pid),
            });
            continue;
        }
        kept.push((path, kind, age));
    }

    let sizes = par_map(&kept, |(path, _, _)| dir_size(path));
    let items = kept
        .into_iter()
        .zip(sizes)
        .filter(|(_, size)| *size >= input.opts.min_size)
        .map(|((path, kind, age), size)| Item {
            id: format!("artifact:{}", label(&path)),
            kind: Kind::Artifact,
            label: label(&path),
            path: Some(path),
            branch: None,
            sha: None,
            size_bytes: size,
            reason: format!("{kind} · {}", human_age(age)),
        })
        .collect();
    (items, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::{CleanOptions, NoPrLookup, ScanInput, Skipped};
    use crate::shell::run_in;
    use crate::shell::test_support::init_repo_with_branch;
    use flare_process::cwd::LiveProc;
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use std::time::Duration;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    fn labels(
        root: &Path,
        repo: Option<&Path>,
        opts: &CleanOptions,
        live: &[LiveProc],
    ) -> (Vec<String>, Vec<Skipped>) {
        let (claimed, states) = (HashSet::new(), HashMap::new());
        let input = ScanInput {
            repo_root: repo,
            scan_root: root,
            opts,
            prs: &NoPrLookup,
            pr_lookup_available: true,
            claimed_items: &claimed,
            item_states: &states,
            live,
        };
        let (items, skipped) = scan(&input, opts.artifacts.as_deref().unwrap_or(&[]));
        let mut l: Vec<String> = items.into_iter().map(|i| i.label).collect();
        l.sort();
        (l, skipped)
    }

    fn all() -> CleanOptions {
        CleanOptions {
            artifacts: Some(Vec::new()),
            ..Default::default()
        }
    }

    #[test]
    fn marker_is_required_and_matches_are_not_descended_into() {
        let d = tempfile::TempDir::new().unwrap();
        let r = d.path();
        touch(&r.join("app/package.json"));
        touch(&r.join("app/node_modules/dep/node_modules/inner/index.js"));
        touch(&r.join("svc/Cargo.toml"));
        touch(&r.join("svc/target/debug/x"));
        touch(&r.join("notes/target/readme.txt")); // no Cargo.toml beside it
        touch(&r.join("py/pkg/__pycache__/m.pyc")); // needs no marker
        let (l, _) = labels(r, None, &all(), &[]);
        assert_eq!(l, ["app/node_modules", "py/pkg/__pycache__", "svc/target"]);
    }

    #[test]
    fn tracked_and_unignored_ambiguous_dirs_are_never_candidates() {
        let repo = init_repo_with_branch("master");
        let r = &repo.path;
        touch(&r.join("package.json"));
        touch(&r.join("dist/bundle.js")); // committed below
        touch(&r.join("build/out.js")); // untracked, not ignored
        touch(&r.join("coverage/lcov.info")); // ignored
        touch(&r.join("node_modules/a/index.js")); // unambiguous, untracked
        std::fs::write(r.join(".gitignore"), "coverage/\nnode_modules/\n").unwrap();
        run_in(r, &["add", "package.json", "dist", ".gitignore"]).unwrap();
        run_in(r, &["commit", "-m", "files"]).unwrap();
        let (l, _) = labels(r, Some(r), &all(), &[]);
        assert_eq!(l, ["coverage", "node_modules"]);
    }

    #[test]
    fn ambiguous_names_outside_a_repo_are_never_candidates() {
        let d = tempfile::TempDir::new().unwrap();
        touch(&d.path().join("package.json"));
        touch(&d.path().join("dist/x.js"));
        touch(&d.path().join("node_modules/a/i.js"));
        assert_eq!(labels(d.path(), None, &all(), &[]).0, ["node_modules"]);
    }

    // Review Focus 3
    #[cfg(unix)]
    #[test]
    fn symlinked_artifact_dir_is_ignored() {
        let d = tempfile::TempDir::new().unwrap();
        touch(&d.path().join("store/real/i.js"));
        touch(&d.path().join("app/package.json"));
        std::os::unix::fs::symlink(d.path().join("store"), d.path().join("app/node_modules"))
            .unwrap();
        assert!(labels(d.path(), None, &all(), &[]).0.is_empty());
        assert!(d.path().join("store/real/i.js").exists());
    }

    // Review Focus 4
    #[test]
    fn nested_repo_tracked_dist_is_safe() {
        let outer = init_repo_with_branch("master");
        let inner = outer.path.join("vendor/lib");
        std::fs::create_dir_all(&inner).unwrap();
        run_in(&inner, &["init", "-b", "main"]).unwrap();
        run_in(&inner, &["config", "user.email", "t@t"]).unwrap();
        run_in(&inner, &["config", "user.name", "T"]).unwrap();
        touch(&inner.join("package.json"));
        touch(&inner.join("dist/lib.js"));
        run_in(&inner, &["add", "."]).unwrap();
        run_in(&inner, &["commit", "-m", "ship dist"]).unwrap();
        let (l, _) = labels(&outer.path, Some(&outer.path), &all(), &[]);
        assert!(l.is_empty(), "{l:?}");
    }

    #[test]
    fn busy_project_is_skipped_but_shells_and_nested_worktrees_do_not_count() {
        let d = tempfile::TempDir::new().unwrap();
        let r = paths::canonical(d.path());
        touch(&r.join("Cargo.toml"));
        touch(&r.join("target/x"));
        let proc_ = |pid, name: &str, cwd: std::path::PathBuf| LiveProc {
            pid,
            name: name.into(),
            cwd,
        };
        let (l, skipped) = labels(&r, None, &all(), &[proc_(7, "cargo", r.clone())]);
        assert!(l.is_empty());
        assert_eq!(skipped[0].reason, "in use: cargo (pid 7)");
        let harmless = [
            proc_(8, "zsh", r.clone()),
            proc_(9, "cargo", r.join(".worktrees/task/1")),
        ];
        assert_eq!(labels(&r, None, &all(), &harmless).0, ["target"]);
    }

    #[test]
    fn kind_filter_and_min_size_apply() {
        let d = tempfile::TempDir::new().unwrap();
        touch(&d.path().join("a/Cargo.toml"));
        touch(&d.path().join("a/target/x"));
        touch(&d.path().join("b/package.json"));
        touch(&d.path().join("b/node_modules/y"));
        let rust = CleanOptions {
            artifacts: Some(vec!["rust".into()]),
            ..Default::default()
        };
        assert_eq!(labels(d.path(), None, &rust, &[]).0, ["a/target"]);
        let big = CleanOptions {
            min_size: 1024,
            ..all()
        };
        assert!(labels(d.path(), None, &big, &[]).0.is_empty());
    }

    #[test]
    fn older_than_drops_recently_touched_dirs() {
        let d = tempfile::TempDir::new().unwrap();
        touch(&d.path().join("Cargo.toml"));
        touch(&d.path().join("target/x"));
        let old = CleanOptions {
            older_than: Some(Duration::from_secs(3600)),
            ..all()
        };
        assert!(
            labels(d.path(), None, &old, &[]).0.is_empty(),
            "just created => too new"
        );
    }

    #[test]
    fn kinds_lists_each_kind_once() {
        let k = kinds();
        assert!(k.contains(&"rust") && k.contains(&"node") && k.contains(&"python"));
        let mut unique = k.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), k.len());
    }

    // Review finding 6: an ignored directory that is its own git repository
    // (a deploy clone in `dist/`) may hold unpushed commits.
    #[test]
    fn directory_that_is_its_own_git_repo_is_never_a_candidate() {
        let repo = init_repo_with_branch("master");
        let r = &repo.path;
        touch(&r.join("package.json"));
        touch(&r.join("node_modules/a/index.js"));
        std::fs::write(r.join(".gitignore"), "dist/\nnode_modules/\n").unwrap();
        let dist = r.join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        run_in(&dist, &["init", "-b", "gh-pages"]).unwrap();
        touch(&dist.join("index.html"));
        let (l, _) = labels(r, Some(r), &all(), &[]);
        assert_eq!(l, ["node_modules"]);
    }

    // Review Focus 4, strengthened: a committed directory with an
    // unambiguous artifact name is still not an artifact.
    #[test]
    fn tracked_unambiguous_dir_is_never_a_candidate() {
        let repo = init_repo_with_branch("master");
        let r = &repo.path;
        touch(&r.join("package.json"));
        touch(&r.join("node_modules/vendored.js"));
        run_in(r, &["add", "-f", "."]).unwrap();
        run_in(r, &["commit", "-m", "vendored deps"]).unwrap();
        let (l, _) = labels(r, Some(r), &all(), &[]);
        assert!(l.is_empty(), "{l:?}");
    }
}
