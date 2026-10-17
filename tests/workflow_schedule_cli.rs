//! CLI coverage for workflow schedules (T45, T47, T48).
//! HOME is a temp dir so the test never touches a real workflows.db.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

const GIT_LOCATION_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
];

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    let out = cmd.args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "master"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    Fixture {
        _tmp: tmp,
        home,
        repo,
    }
}

fn second_repo(f: &Fixture) -> PathBuf {
    let repo = f._tmp.path().join("other");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "master"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    repo
}

fn write_workflow(repo: &Path, name: &str, body: &str) {
    let dir = repo.join(".agentflare").join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.json")), body).unwrap();
}

fn run(home: &Path, repo: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentflare"));
    cmd.current_dir(repo)
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENTFLARE_HOME_OVERRIDE", home)
        .env("AGENTFLARE_NO_INTERACTIVE", "1")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN");
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn t45_cli_fire_enqueues_and_exits_without_running() {
    let f = fixture();
    let marker = f.repo.join("ran");
    // serde_json escapes Windows separators; a format string would emit `\U`.
    let body = serde_json::json!({
        "name": "delayed",
        "steps": [
            {"name": "t", "command": ["/bin/sleep", "30"]},
            {"name": "m", "command": ["/usr/bin/touch", marker]},
        ]
    });
    write_workflow(&f.repo, "delayed", &serde_json::to_string(&body).unwrap());
    let created = run(
        &f.home,
        &f.repo,
        &[
            "workflow",
            "schedule",
            "create",
            "nightly",
            "--workflow",
            "delayed",
            "--every",
            "1m",
        ],
    );
    assert!(
        created.status.success(),
        "create failed: {}",
        stderr(&created)
    );

    let started = Instant::now();
    let fired = run(
        &f.home,
        &f.repo,
        &["workflow", "schedule", "fire", "nightly"],
    );
    let elapsed = started.elapsed();
    assert!(fired.status.success(), "fire failed: {}", stderr(&fired));
    let out = stdout(&fired);
    assert!(out.contains("queued; waiting for daemon"), "{out}");
    assert!(
        elapsed.as_secs() < 5,
        "client took {elapsed:?}; fire must not run the delayed command"
    );
    assert!(!marker.exists(), "client executed the workflow");

    let db = f.home.join(".agentflare").join("workflows.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let workflows: i64 = conn
        .query_row("SELECT COUNT(*) FROM workflow_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(workflows, 0);
    let status: String = conn
        .query_row(
            "SELECT status FROM schedule_runs WHERE trigger='manual'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "queued");
}

#[test]
fn t47_clean_accepts_s_m_and_month_and_rejects_junk() {
    let f = fixture();
    for age in ["30s", "5m", "3M", "2h30m"] {
        let out = run(
            &f.home,
            &f.repo,
            &["clean", "--older-than", age, "--dry-run", "--json"],
        );
        assert!(out.status.success(), "{age}: {}", stderr(&out));
    }
    // `-1d` is rejected by parse_duration; clap treats a leading dash as a flag,
    // so the CLI check covers the forms that actually reach the parser.
    for age in ["5x", "7", "soon"] {
        let out = run(
            &f.home,
            &f.repo,
            &["clean", "--older-than", age, "--dry-run", "--json"],
        );
        assert!(!out.status.success(), "{age} must be rejected");
        assert!(stderr(&out).contains("invalid duration"), "{age}");
    }
}

#[test]
fn t48_list_scope_id_lookup_json_fire_and_validation() {
    let f = fixture();
    write_workflow(
        &f.repo,
        "echo",
        r#"{"name":"echo","steps":[{"name":"s","command":["/bin/true"]}]}"#,
    );
    let other = second_repo(&f);
    assert!(
        run(
            &f.home,
            &f.repo,
            &[
                "workflow",
                "schedule",
                "create",
                "nightly",
                "--workflow",
                "echo",
                "--every",
                "2m",
                "--grace",
                "30s",
            ],
        )
        .status
        .success()
    );

    let show = run(
        &f.home,
        &f.repo,
        &["workflow", "schedule", "show", "nightly", "--json"],
    );
    assert!(show.status.success(), "{}", stderr(&show));
    let row: serde_json::Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(row["effective_grace_secs"], 30);
    let id = row["id"].as_str().unwrap().to_string();

    let scoped = run(&f.home, &other, &["workflow", "schedule", "list", "--json"]);
    assert!(scoped.status.success(), "{}", stderr(&scoped));
    let scoped: serde_json::Value = serde_json::from_str(&stdout(&scoped)).unwrap();
    assert!(scoped.as_array().unwrap().is_empty());

    let all = run(
        &f.home,
        &other,
        &["workflow", "schedule", "list", "--all", "--json"],
    );
    let all: serde_json::Value = serde_json::from_str(&stdout(&all)).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);

    let by_id = run(
        &f.home,
        &other,
        &["workflow", "schedule", "show", &id, "--json"],
    );
    assert!(by_id.status.success(), "{}", stderr(&by_id));

    let fired = run(
        &f.home,
        &f.repo,
        &["workflow", "schedule", "fire", &id, "--json"],
    );
    assert!(fired.status.success(), "{}", stderr(&fired));
    let receipt: serde_json::Value = serde_json::from_str(&stdout(&fired)).unwrap();
    assert_eq!(receipt["status"], "queued");
    assert!(receipt["occurrence_id"].is_string());

    for args in [
        vec![
            "workflow",
            "schedule",
            "create",
            "Bad Name",
            "--workflow",
            "echo",
            "--every",
            "1m",
        ],
        vec![
            "workflow",
            "schedule",
            "create",
            "other",
            "--workflow",
            "echo",
            "--every",
            "30s",
        ],
        vec![
            "workflow",
            "schedule",
            "create",
            "other",
            "--workflow",
            "missing",
            "--every",
            "1m",
        ],
        vec![
            "workflow",
            "schedule",
            "create",
            "other",
            "--workflow",
            "echo",
            "--every",
            "1m",
            "--params",
            r#"{"schedule":1}"#,
        ],
    ] {
        let out = run(&f.home, &f.repo, &args);
        assert!(
            !out.status.success(),
            "{args:?} must fail: {}",
            stdout(&out)
        );
    }
}
