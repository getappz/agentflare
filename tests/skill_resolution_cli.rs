use std::{fs, path::Path, process::Command};

fn skill(dir: &Path, name: &str, body: &str) {
    fs::create_dir_all(dir.join("reference")).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Marigold fixture\n---\n{body}\n"),
    )
    .unwrap();
    fs::write(dir.join("reference/example.md"), body).unwrap();
}

#[test]
fn search_install_get_and_remove_stay_in_flare() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let home = root.join("home");
    let registry = root.join("registry");
    let repo = root.join("repo");
    let github = "https://github.com/fixture/bundle";
    let tree = format!("{github}/tree/main/skills/demo");
    skill(&registry, "catalog", "root skill");
    skill(&registry.join("demo"), "demo", "registry copy");
    skill(&repo.join("skills/demo"), "demo", "github copy");
    for args in [
        vec!["init", "-b", "main"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let conf = home.join(".agentflare/registries.json");
    fs::create_dir_all(conf.parent().unwrap()).unwrap();
    fs::write(&conf, serde_json::to_vec(&[&registry]).unwrap()).unwrap();
    let sentinel = home.join(".codex/skills/demo");
    skill(&sentinel, "demo", "agent-owned");
    let run = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_agentflare"))
            .current_dir(root)
            .env("AGENTFLARE_HOME_OVERRIDE", &home)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("AGENTFLARE_NO_INTERACTIVE", "1")
            .env("GIT_ALLOW_PROTOCOL", "file")
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!(
                    "url.{}.insteadOf",
                    repo.to_str().unwrap().replace('\\', "/")
                ),
            )
            .env("GIT_CONFIG_VALUE_0", format!("{github}.git"))
            .arg("skills")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(run(&["search", "marigold"]).contains("demo"));
    run(&["install", &tree]);
    assert!(run(&["get", "demo"]).contains("registry copy"));
    run(&["remove", "demo"]);
    fs::write(&conf, "[]").unwrap();
    assert!(run(&["get", &tree, "--reference", "reference/example.md"]).contains("github copy"));
    assert!(home.join(".agentflare/skills/demo/SKILL.md").is_file());
    assert!(run(&["list"]).contains("demo"));
    run(&["remove", "demo"]);
    assert!(
        fs::read_to_string(sentinel.join("SKILL.md"))
            .unwrap()
            .contains("agent-owned")
    );
    for dir in [
        ".agents/skills",
        ".claude/skills",
        ".cursor/skills",
        ".agentflare/skills",
    ] {
        assert!(!home.join(dir).join("demo").exists(), "{dir}");
    }
}
