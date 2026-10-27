use flare_insights::config::InsightsConfig;
use flare_insights::ingest::{claude::ClaudeAdapter, Adapter};
use std::path::PathBuf;
use std::process::Command;

#[test]
fn default_sources_find_claude_transcripts() {
    const CHILD: &str = "AGENTFLARE_INSIGHTS_SOURCE_PATH_TEST";
    if let Some(mode) = std::env::var_os(CHILD) {
        let config = InsightsConfig::default();
        if mode == "native" || mode == "bogus_home" {
            let home = dirs::home_dir().expect("OS home directory");
            assert_eq!(
                config.sources["claude_code"],
                home.join(".claude/projects"),
                "mode={mode:?}"
            );
            assert_eq!(config.sources["codex"], home.join(".codex/sessions"));
            return;
        }

        let projects = PathBuf::from(mode);
        assert_eq!(config.sources["claude_code"], projects);
        let bundle = ClaudeAdapter
            .scan(&config, &Default::default(), &mut |_, _| {})
            .unwrap();
        assert_eq!(bundle.sessions.len(), 2);
        for id in ["eac697c3-5f7e-421a-a4cd-239d04881d3b", "worktree-session"] {
            assert!(bundle.sessions.iter().any(|session| session.id == id));
            assert!(bundle.turns.iter().any(|turn| {
                turn.session_id == id && turn.user_text.as_deref() == Some("fix login")
            }));
        }
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let projects = tmp.path().join(".claude/projects");
    for (slug, id) in [
        (
            "C--Users-shiva-workspace-leanstack",
            "eac697c3-5f7e-421a-a4cd-239d04881d3b",
        ),
        (
            "C--Users-shiva-workspace-leanstack--worktrees-task-781",
            "worktree-session",
        ),
    ] {
        let project = projects.join(slug);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{id}.jsonl")),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"fix login\"},\"timestamp\":\"2026-10-26T10:00:00Z\"}\n",
        )
        .unwrap();
    }

    // Child-only env changes avoid racing the parallel test runner.
    let mut modes = vec!["native", "home", "override"];
    // Windows: a Git-Bash-style HOME with no agent stores must not hide the
    // native profile (the resume live-scan "scanned 0" failure mode).
    if cfg!(windows) {
        modes.push("bogus_home");
    }
    for mode in modes {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "default_sources_find_claude_transcripts",
                "--nocapture",
            ])
            .env_remove("HOME")
            .env_remove("CLAUDE_PROJECTS_DIR")
            .env_remove("CODEX_SESSIONS_DIR");
        match mode {
            "native" | "bogus_home" => {
                child.env(CHILD, mode);
                if mode == "bogus_home" {
                    child.env("HOME", tmp.path().join("no-agent-stores-here"));
                }
            }
            "home" => {
                child.env(CHILD, &projects);
                child.env("HOME", tmp.path());
            }
            "override" => {
                child.env(CHILD, &projects);
                child.env("CLAUDE_PROJECTS_DIR", &projects);
            }
            _ => unreachable!(),
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
