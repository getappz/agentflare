use super::*;

#[test]
fn resolve_workspace_id_creates_once_and_reuses() {
    let (tmp, _s) = harness();
    let conn = backend_conn(&tmp);
    let id1 = AgentflareMcp::resolve_workspace_id(&conn).unwrap();
    let id2 = AgentflareMcp::resolve_workspace_id(&conn).unwrap();
    assert_eq!(id1, id2);
}

/// If `.agentflare/project.json` is deleted (wiped worktree, `rm -rf`,
/// etc.) while the project it pointed to still exists, resolving again
/// must reconnect to that same project — not silently fork a duplicate,
/// which would strand the original project's items.
#[test]
fn resolve_project_relinks_to_existing_project_when_link_file_is_deleted() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let first = s.resolve_project(&conn).unwrap();

    std::fs::remove_file(s.project_link_path()).unwrap();

    let second = s.resolve_project(&conn).unwrap();
    assert_eq!(
        first.id, second.id,
        "must reconnect to the same project, not fork a duplicate"
    );
    let all = agentflare_backend::project::list_by_workspace(&conn, &first.workspace_id).unwrap();
    assert_eq!(
        all.len(),
        1,
        "no duplicate project should have been created: {all:?}"
    );
}

/// Two different repos can easily share a directory basename (or, for
/// non-git dirs, no distinguishing info at all beyond the name). They
/// must never be conflated into one project just because they'd derive
/// the same display identifier — each gets its own project, with the
/// second disambiguated by a suffix.
#[test]
fn resolve_project_does_not_conflate_different_repos_with_the_same_derived_name() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("backend.db");
    let s1 = AgentflareMcp {
        backend_db_override: Some(db_path.clone()),
        backend_project_link_override: Some(tmp.path().join("link1.json")),
        backend_repo_key_override: Some("path:/repo/one".to_string()),
        ..Default::default()
    };
    let s2 = AgentflareMcp {
        backend_db_override: Some(db_path.clone()),
        backend_project_link_override: Some(tmp.path().join("link2.json")),
        backend_repo_key_override: Some("path:/repo/two".to_string()),
        ..Default::default()
    };
    let conn = agentflare_backend::db::open_db(&db_path).unwrap();
    let p1 = s1.resolve_project(&conn).unwrap();
    let p2 = s2.resolve_project(&conn).unwrap();
    assert_ne!(
        p1.id, p2.id,
        "different repos must never share a project even with the same derived name"
    );
    assert_ne!(
        p1.identifier, p2.identifier,
        "the second project must get a disambiguating suffix"
    );

    // Each keeps resolving to its own project on repeat calls.
    assert_eq!(s1.resolve_project(&conn).unwrap().id, p1.id);
    assert_eq!(s2.resolve_project(&conn).unwrap().id, p2.id);
}

/// Regression test for item #94: `register_project_dir`/`register_bridge_repo`
/// must resolve the repo root through `self.worktree_repo_root()` -- which
/// redirects a linked worktree back to its main checkout (see that method's
/// own tests, and `flare_git_core::branch`'s `is_linked_worktree`/
/// `main_worktree_root` tests, for that redirect logic itself) -- not the
/// raw, override-blind `Self::repo_root()`. Getting this wrong is exactly how
/// item #91's dispatch nested one worktree inside another: a process whose
/// cwd was inside a linked worktree wrote that worktree's own path into the
/// daemon-wide `project_dirs`/`bridge_repos` registries, and the next
/// discovery tick built the following item's worktree relative to it.
///
/// Exercised via `worktree_repo_root_override` rather than an actual `cd`
/// into a linked worktree: `std::env::set_current_dir` is process-wide, and
/// mutating it here -- even briefly, even under a lock -- would race every
/// *other* test in this binary that reads cwd-derived state (e.g.
/// `resolve_repo_key`'s `git remote get-url origin` lookup) without holding
/// that same lock, exactly the flakiness this diff's first attempt hit.
#[test]
fn resolve_project_registers_the_worktree_redirected_root_not_the_raw_repo_root() {
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().canonicalize().unwrap();
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_root)
            .output()
            .unwrap()
    };
    run_git(&["init", "-b", "master"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);
    run_git(&["commit", "--allow-empty", "-m", "initial"]);
    run_git(&[
        "remote",
        "add",
        "origin",
        "https://github.com/testowner/testrepo.git",
    ]);

    let scratch = tempfile::tempdir().unwrap();
    let db_path = scratch.path().join("backend.db");
    let s = AgentflareMcp {
        backend_db_override: Some(db_path.clone()),
        backend_project_link_override: Some(scratch.path().join("project.json")),
        backend_repo_key_override: Some("path:test-worktree-redirect".to_string()),
        worktree_repo_root_override: Some(repo_root.clone()),
        ..Default::default()
    };
    let conn = agentflare_backend::db::open_db(&db_path).unwrap();

    let project = s.resolve_project(&conn).unwrap();

    let dirs = agentflare_backend::project_dir::list(&conn).unwrap();
    let dir_row = dirs
        .iter()
        .find(|d| d.project_id == project.id)
        .expect("register_project_dir must have written a row for this project");
    assert_eq!(
        std::path::Path::new(&dir_row.folder_path),
        repo_root,
        "project_dirs must use worktree_repo_root(), not the raw repo_root() of wherever this test binary's own cwd happens to be"
    );

    let repos = agentflare_backend::bridge_repo::list(&conn).unwrap();
    let repo_row = repos
        .iter()
        .find(|r| r.project_id == project.id)
        .expect("register_bridge_repo must have written a row for this project");
    assert_eq!(
        std::path::Path::new(&repo_row.folder_path),
        repo_root,
        "bridge_repos must use worktree_repo_root(), not the raw repo_root() of wherever this test binary's own cwd happens to be"
    );
}

/// Non-git projects need the same "root is stable no matter which
/// subdirectory you're in" guarantee git repos get for free from `git
/// rev-parse --show-toplevel` — otherwise the same project would split
/// across multiple `.agentflare/project.json` files depending on which
/// subdirectory a tool happened to be called from.
#[test]
fn find_root_from_walks_up_to_the_nearest_marker() {
    // Bounding "home" at the tempdir's own parent contains the walk
    // entirely within this test's constructed tree — passing some
    // unrelated path here would NOT do that: the walk follows the real
    // filesystem's `.parent()` chain regardless, so it would keep
    // climbing past `root` into real ancestor directories (which may
    // have their own real markers, e.g. this machine's actual
    // `~/.agentflare`) until it happened to reach that unrelated path,
    // which — not being a real ancestor — it never would, walking all
    // the way to the filesystem root instead.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.parent().unwrap();
    std::fs::write(root.join("package.json"), "{}").unwrap();
    let deep = root.join("src").join("nested").join("deep");
    std::fs::create_dir_all(&deep).unwrap();

    assert_eq!(AgentflareMcp::find_root_from(&deep, home), root);
    assert_eq!(AgentflareMcp::find_root_from(root, home), root);
}

#[test]
fn find_root_from_prefers_an_existing_agentflare_link_over_other_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.parent().unwrap();
    // A nested directory with its own marker (e.g. a sub-package) must
    // not shadow an ancestor's existing project link — the
    // .agentflare pass runs before the ROOT_MARKERS pass for
    // exactly this reason.
    std::fs::create_dir_all(root.join(".agentflare")).unwrap();
    let sub = root.join("packages").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("package.json"), "{}").unwrap();

    assert_eq!(AgentflareMcp::find_root_from(&sub, home), root);
    assert_eq!(AgentflareMcp::find_root_from(root, home), root);
}

/// The boundary itself: a directory that IS `home` must never be
/// treated as a project root, even if it happens to contain a marker —
/// this is what keeps the global `~/.agentflare` data dir from ever
/// being mistaken for a per-repo link.
#[test]
fn find_root_from_never_resolves_to_home_itself() {
    let home = tempfile::tempdir().unwrap();
    // Stands in for the real global data dir at ~/.agentflare.
    std::fs::create_dir_all(home.path().join(".agentflare")).unwrap();
    let start = home.path().join("some_project");
    std::fs::create_dir_all(&start).unwrap();

    // `start` itself has no marker, and home — one level up — does. If
    // the walk checked markers at `home`, this would return `home`. It
    // must instead stop short of ever inspecting `home` and fall back
    // to `start`.
    assert_eq!(AgentflareMcp::find_root_from(&start, home.path()), start);
}

// No test for the "nothing found anywhere above" fallback: `find_root_from`
// walks all the way to the filesystem root, so a tempdir-based test would
// depend on what markers happen to exist above the OS temp directory on
// whatever machine runs this — not a property this test can control. The
// fallback itself is a single trivial `None => return start`.
