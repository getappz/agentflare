use std::process::{Command, Output};

fn remember(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .current_dir(dir.path())
        .env("AGENTFLARE_HOME_OVERRIDE", dir.path())
        .env("AGENTFLARE_NO_INTERACTIVE", "1")
        .args(["memory", "remember"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn rejected_memory_writes_exit_nonzero() {
    for (args, message) in [
        (vec!["title", "content", "--type", "lesson"], "reserved"),
        (vec!["", "content"], "required"),
    ] {
        let out = remember(&args);
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(message),
            "{out:?}"
        );
    }
}

#[test]
fn advisory_warnings_leave_successful_memory_writes_at_exit_zero() {
    let out = remember(&["deadlock fix", "fixed deadlock"]);
    assert!(out.status.success(), "{out:?}");
    let response: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["status"], "created");
    assert!(
        response["warnings"]
            .as_array()
            .is_some_and(|w| !w.is_empty())
    );
}
