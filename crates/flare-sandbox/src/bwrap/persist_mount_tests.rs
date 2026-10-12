//! `MountPolicy::Persist` tests (item #355), split out of `mod tests`: the
//! LOC gate caps `bwrap/mod.rs` at 1500 lines and these cases live most
//! naturally beside the mount-policy code without bloating that file.

use super::*;

fn agent(binary_name: &'static str, mounts: &'static [AgentStateMount]) -> SandboxConfig {
    SandboxConfig {
        agent_profiles: Box::leak(Box::new([AgentProfile {
            binary_name,
            state_mounts: mounts,
        }])),
        writable_home_dirs: Vec::new(),
    }
}

#[test]
fn persist_mount_binds_read_write_when_present() {
    // Item #355: rotation-based OAuth state must persist -- a writable
    // bind, never an overlay/tmpfs whose discarded writes would leave
    // the host file holding server-consumed credentials.
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join(".testagent");
    std::fs::create_dir_all(&data_dir).unwrap();
    let data_dir = std::fs::canonicalize(&data_dir).unwrap();
    let home = std::ffi::OsString::from(dir.path());
    let config = agent(
        "testagent",
        &[AgentStateMount {
            relative_path: ".testagent",
            policy: MountPolicy::Persist,
            diagnostic_log: None,
        }],
    );
    let args = build_bwrap_args_with_home(
        None,
        "/usr/local/bin/testagent",
        &[],
        Some(&home),
        false,
        &config,
    );
    let data_str = path_to_string(&data_dir);
    let idx = args
        .iter()
        .position(|a| a == &data_str)
        .expect("agent data dir bound");
    assert_eq!(args[idx - 1], "--bind-try");
    assert!(
        !args
            .iter()
            .any(|a| a == "--overlay-src" || a == "--tmp-overlay"),
        "persist must not overlay: {args:?}"
    );
}

#[test]
fn persist_mount_skips_absent_dir_without_tmpfs() {
    // Nothing to persist when the dir was never created: no mount at
    // all (a tmpfs here would hide the host path from a later login).
    let dir = tempfile::tempdir().unwrap();
    let home = std::ffi::OsString::from(dir.path());
    let config = agent(
        "testagent",
        &[AgentStateMount {
            relative_path: ".testagent",
            policy: MountPolicy::Persist,
            diagnostic_log: None,
        }],
    );
    let args = build_bwrap_args_with_home(
        None,
        "/usr/local/bin/testagent",
        &[],
        Some(&home),
        false,
        &config,
    );
    let data_str = path_to_string(&dir.path().join(".testagent"));
    assert!(
        !args.iter().any(|a| a == &data_str),
        "absent dir must not be mounted: {args:?}"
    );
}

#[cfg(unix)]
#[test]
fn persist_mount_skips_symlink_destination() {
    // Binding through a symlink would persist writes to wherever it
    // points: skip with an event instead, live link or dangling alike.
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join(".live")).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("does-not-exist"),
        dir.path().join(".dangling"),
    )
    .unwrap();
    let home = std::ffi::OsString::from(dir.path());
    for (relative, mounts) in [
        (
            ".live",
            &[AgentStateMount {
                relative_path: ".live",
                policy: MountPolicy::Persist,
                diagnostic_log: None,
            }] as &'static [AgentStateMount],
        ),
        (
            ".dangling",
            &[AgentStateMount {
                relative_path: ".dangling",
                policy: MountPolicy::Persist,
                diagnostic_log: None,
            }] as &'static [AgentStateMount],
        ),
    ] {
        let config = agent("testagent", mounts);
        let args = build_bwrap_args_with_home(
            None,
            "/usr/local/bin/testagent",
            &[],
            Some(&home),
            false,
            &config,
        );
        let data_str = path_to_string(&dir.path().join(relative));
        assert!(
            !args.iter().any(|a| a == &data_str),
            "{relative} symlink must not be mounted: {args:?}"
        );
    }
}
