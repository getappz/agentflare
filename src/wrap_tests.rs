use super::*;

#[test]
fn proxy_option_is_consumed_only_before_native_claude_arguments() {
    for (input, proxy, expected) in [
        (
            vec!["--with-proxy", "--resume", "a b"],
            true,
            vec!["--resume", "a b"],
        ),
        (
            vec!["--", "--with-proxy"],
            false,
            vec!["--", "--with-proxy"],
        ),
        (
            vec!["-p", "--with-proxy"],
            false,
            vec!["-p", "--with-proxy"],
        ),
        (vec!["--version"], false, vec!["--version"]),
    ] {
        let input: Vec<String> = input.into_iter().map(str::to_owned).collect();
        let (actual_proxy, args) = proxy_arguments(&input);
        assert_eq!(actual_proxy, proxy);
        assert_eq!(args, expected);
    }
}

#[test]
fn install_is_idempotent_and_unwrap_preserves_unrelated_profile_content() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile.ps1");
    let original = "# existing setup\r\n$env:EXAMPLE = 'keep me'\r\n";
    std::fs::write(&profile, original).unwrap();
    configure(
        Shell::PowerShell,
        &profile,
        Path::new("C:/Program Files/agentflare.exe"),
        false,
    )
    .unwrap();
    let once = std::fs::read(&profile).unwrap();
    configure(
        Shell::PowerShell,
        &profile,
        Path::new("C:/Program Files/agentflare.exe"),
        false,
    )
    .unwrap();
    assert_eq!(std::fs::read(&profile).unwrap(), once);
    configure(Shell::PowerShell, &profile, Path::new("unused"), true).unwrap();
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), original);
}

#[test]
fn malformed_or_unmanaged_shell_setup_is_never_overwritten() {
    for original in [
        "# >>> agentflare wrap claude >>>\npartial block\n",
        "# <<< agentflare wrap claude <<<\n",
        "function claude { custom @args }\n",
        "alias claude='custom'\n",
        "alias -- claude=true\n",
        "claude() { custom; }\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        std::fs::write(&profile, original).unwrap();
        assert!(configure(Shell::Bash, &profile, Path::new("agentflare"), false).is_err());
        assert_eq!(std::fs::read_to_string(&profile).unwrap(), original);
    }
}

#[test]
fn unreadable_profile_is_not_treated_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(configure(Shell::Bash, dir.path(), Path::new("agentflare"), false).is_err());
    assert!(dir.path().is_dir());
}

#[test]
fn executable_paths_are_literals_in_the_target_shell() {
    let path = Path::new("C:/folder'a; echo INJECTED; $HOME&x/agentflare.exe");
    let expected = path.to_string_lossy();
    #[cfg(windows)]
    {
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("Write-Output {}", shell_word(Shell::PowerShell, path)),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
    #[cfg(unix)]
    {
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                &format!("printf '%s' {}", shell_word(Shell::Bash, path)),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}

#[cfg(windows)]
#[test]
fn powershell_caller_observes_claude_failure() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("claude fixture.cmd");
    std::fs::write(&fake, "@exit /b 7\r\n").unwrap();
    let profile = dir.path().join("profile.ps1");
    configure(Shell::PowerShell, &profile, &fake, false).unwrap();
    let command = format!(
        ". {}; claude; $ok=$?; $code=$LASTEXITCODE; Write-Output \"$ok $code\"",
        shell_word(Shell::PowerShell, &profile)
    );
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "False 7");
}
