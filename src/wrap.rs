use crate::shell::Shell;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

#[path = "wrap_interrupt.rs"]
mod interrupt;

#[path = "wrap_https.rs"]
mod https;

const START: &str = "# >>> agentflare wrap claude >>>";
const END: &str = "# <<< agentflare wrap claude <<<";
const NATIVE_ENTRY: &str = "__agentflare-claude-native";

fn proxy_arguments(args: &[OsString]) -> (bool, &[OsString]) {
    if args.first().is_some_and(|arg| arg == "--with-proxy") {
        (true, &args[1..])
    } else {
        (false, args)
    }
}

fn managed_range(content: &str) -> Result<Option<std::ops::Range<usize>>, String> {
    match (content.find(START), content.find(END)) {
        (None, None) => Ok(None),
        (Some(start), Some(end))
            if start < end
                && content.matches(START).count() == 1
                && content.matches(END).count() == 1
                && (start == 0 || content[..start].ends_with('\n'))
                && content[..end].ends_with('\n')
                && content[start + START.len()..].starts_with(['\r', '\n'])
                && (content[end + END.len()..].is_empty()
                    || content[end + END.len()..].starts_with(['\r', '\n'])) =>
        {
            let mut finish = end + END.len();
            if content[finish..].starts_with("\r\n") {
                finish += 2;
            } else if content[finish..].starts_with('\n') {
                finish += 1;
            }
            Ok(Some(start..finish))
        }
        _ => Err(
            "Malformed agentflare Claude wrapper block; repair it before wrapping/unwrapping"
                .into(),
        ),
    }
}

fn shell_word(shell: Shell, path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    match shell {
        Shell::PowerShell => format!("'{}'", path.replace('\'', "''")),
        Shell::Fish => format!("'{}'", path.replace('\\', "\\\\").replace('\'', "\\'")),
        Shell::Bash | Shell::Zsh => format!("'{}'", path.replace('\'', "'\"'\"'")),
    }
}

fn block(shell: Shell, executable: &Path) -> String {
    let executable = shell_word(shell, executable);
    let function = match shell {
        Shell::PowerShell => format!("Set-Alias -Name claude -Value {executable} -Scope Global"),
        Shell::Bash | Shell::Zsh => {
            format!("claude() {{ command {executable} wrap claude --launch -- \"$@\"; }}")
        }
        Shell::Fish => {
            format!("function claude\n    command {executable} wrap claude --launch -- $argv\nend")
        }
    };
    format!("{START}\n{function}\n{END}\n")
}

pub fn configure(
    shell: Shell,
    profile: &Path,
    executable: &Path,
    remove: bool,
) -> Result<(), String> {
    let content = match std::fs::read_to_string(profile) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if remove {
                return Ok(());
            }
            String::new()
        }
        Err(error) => return Err(format!("Cannot read {}: {error}", profile.display())),
    };
    let range = managed_range(&content)?;
    let outside = match &range {
        Some(range) => format!("{}{}", &content[..range.start], &content[range.end..]),
        None => content.clone(),
    };
    let rendered = if remove {
        outside
    } else {
        let conflicting = regex::Regex::new(
            r#"(?im)^\s*(?:function\s+(?:global:)?claude(?:\s|[({])|claude\s*\(\s*\)|alias\s+(?:--\s+)?claude(?:=|\s)|(?:Set-Alias|New-Alias)\s+(?:-Name\s+)?['\"]?claude(?:['\"]?\s|$))"#,
        ).expect("static shell-definition regex");
        if conflicting.is_match(&outside) {
            return Err(format!(
                "{} already defines claude outside agentflare's wrapper; resolve that definition first",
                profile.display()
            ));
        }
        let newline = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let replacement = block(shell, executable).replace('\n', newline);
        match range {
            Some(range) => format!(
                "{}{}{}",
                &content[..range.start],
                replacement,
                &content[range.end..]
            ),
            None => {
                let separator = if content.is_empty() || content.ends_with('\n') {
                    ""
                } else {
                    newline
                };
                format!("{content}{separator}{replacement}")
            }
        }
    };
    if rendered == content {
        return Ok(());
    }
    if let Some(parent) = profile.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    crate::atomic_fs::write_bytes_with_fallback(profile, rendered.as_bytes(), None)
}

pub fn setup(shell_name: Option<&str>, profile: Option<&Path>, remove: bool) -> Result<(), String> {
    let shell = match shell_name {
        Some(name) => Shell::from_name(name).ok_or_else(|| format!("Unsupported shell: {name}"))?,
        None => Shell::detect().ok_or("Cannot detect shell; pass --shell")?,
    };
    let profile = profile
        .map(Path::to_path_buf)
        .or_else(|| shell.profile_path())
        .ok_or("Cannot find shell profile; pass --profile")?;
    let mut executable = std::env::current_exe().map_err(|error| error.to_string())?;
    if shell == Shell::PowerShell && !remove {
        executable = install_companion(&executable)?;
    }
    configure(shell, &profile, &executable, remove)?;
    if remove {
        let clear = match shell {
            Shell::PowerShell => "Remove-Item Alias:\\claude -ErrorAction SilentlyContinue",
            Shell::Bash | Shell::Zsh => "unset -f claude",
            Shell::Fish => "functions -e claude",
        };
        println!(
            "Claude wrapper removed from {}. Open a new terminal, or run:\n  {clear}",
            profile.display()
        );
    } else {
        let reload = match shell {
            Shell::PowerShell => ".",
            _ => "source",
        };
        println!(
            "Claude wrapper installed in {}. Reload it:\n  {reload} {}\nThen: claude --with-proxy --resume",
            profile.display(),
            shell_word(shell, &profile)
        );
    }
    Ok(())
}

fn companion_path() -> Result<std::path::PathBuf, String> {
    Ok(dirs::data_local_dir()
        .ok_or("Cannot find local application data directory")?
        .join("agentflare/wrappers")
        .join(format!("agentflare-claude{}", std::env::consts::EXE_SUFFIX)))
}

fn install_companion(executable: &Path) -> Result<std::path::PathBuf, String> {
    let target = companion_path()?;
    if target.exists() {
        refresh_companion_at(executable, &target)?;
        return Ok(target);
    }
    let parent = target.parent().expect("companion has a parent directory");
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let staging = tempfile::tempdir_in(parent).map_err(|error| error.to_string())?;
    let staged = staging
        .path()
        .join(target.file_name().expect("companion filename"));
    if std::fs::hard_link(executable, &staged).is_err() {
        std::fs::copy(executable, &staged).map_err(|error| error.to_string())?;
    }
    std::fs::rename(staged, &target).map_err(|error| format!("Cannot update Claude wrapper executable; close other wrapped Claude sessions and retry: {error}"))?;
    Ok(target)
}

pub(crate) fn refresh_existing_companion(executable: &Path) -> Result<(), String> {
    refresh_companion_at(executable, &companion_path()?)
}

fn refresh_companion_at(executable: &Path, target: &Path) -> Result<(), String> {
    use crate::update::swap::{SwapOutcome, replace_binary, verify_installed};
    if !target.exists() || verify_installed(executable, target).is_ok() {
        return Ok(());
    }
    match replace_binary(executable, target)? {
        SwapOutcome::Installed => verify_installed(executable, target),
        #[cfg(windows)]
        SwapOutcome::Deferred { log } => Err(format!(
            "Claude wrapper update deferred; retry `agentflare wrap claude` after sessions exit (log: {})",
            log.display()
        )),
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
    }
    #[cfg(not(unix))]
    status.code().unwrap_or(1)
}

pub fn launch(args: &[OsString]) -> Result<i32, String> {
    let (proxy, args) = proxy_arguments(args);
    if !proxy {
        return launch_native(args);
    }
    if let Some(base) = std::env::var_os("ANTHROPIC_BASE_URL")
        && !matches!(
            base.to_str().map(|base| base.trim_end_matches('/')),
            Some("" | "https://api.anthropic.com")
        )
    {
        return Err("--with-proxy currently supports the default Anthropic HTTPS endpoint".into());
    }
    let lean = agent_registry::detect::find_binary(&["lean-ctx"])
        .ok_or("--with-proxy requires lean-ctx on PATH")?;
    let mut command = native_command(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let previous_ca = std::env::var_os("NODE_EXTRA_CA_CERTS");
    let bridge = runtime.block_on(async {
        https::shared_proxy(&lean, https::PROXY_PORT).await?;
        https::Bridge::start(https::PROXY_PORT, previous_ca.as_deref().map(Path::new)).await
    })?;
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.to_ascii_uppercase().ends_with("_PROXY"))
        {
            command.env_remove(key);
        }
    }
    command
        .env_remove("ANTHROPIC_BASE_URL")
        .env("HTTPS_PROXY", bridge.url())
        .env("NODE_EXTRA_CA_CERTS", bridge.ca_path())
        .env("NO_PROXY", "127.0.0.1,localhost");
    eprintln!(
        "agentflare: HTTPS bridge -> shared lean-ctx on 127.0.0.1:{}\nMonitor: lean-ctx proxy status",
        https::PROXY_PORT
    );
    wait_for_child(&mut command)
}

fn native_command(args: &[OsString]) -> Result<Command, String> {
    let claude = agent_registry::detect::find_binary(&["claude"])
        .ok_or("Claude executable not found on PATH")?;
    let mut command = Command::new(claude);
    command
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(command)
}

fn launch_native(args: &[OsString]) -> Result<i32, String> {
    let mut command = native_command(args)?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("Cannot launch Claude: {}", command.exec()))
    }
    #[cfg(not(unix))]
    wait_for_child(&mut command)
}

fn wait_for_child(command: &mut Command) -> Result<i32, String> {
    // Claude handles terminal Ctrl-C; the waiting parent must outlive a cancelled turn.
    let _interrupt = interrupt::Guard::install().map_err(|error| error.to_string())?;
    command
        .status()
        .map(exit_code)
        .map_err(|error| format!("Cannot launch Claude: {error}"))
}

pub fn handle_entrypoint() -> Option<Result<i32, String>> {
    let program = std::env::args_os().next()?;
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == NATIVE_ENTRY) {
        return Some(launch_native(&args[1..]));
    }
    if Path::new(&program)
        .file_stem()
        .is_some_and(|name| name == "agentflare-claude")
    {
        return Some(launch(&args));
    }
    if args.len() >= 4
        && args[0] == "wrap"
        && matches!(args[1].to_str(), Some("claude" | "claude-code"))
        && args[2] == "--launch"
        && args[3] == "--"
    {
        return Some(launch(&args[4..]));
    }
    None
}

pub fn cleanup_profiles(dry_run: bool) {
    let mut profiles: std::collections::BTreeSet<_> =
        [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell]
            .into_iter()
            .filter_map(|shell| shell.profile_path())
            .collect();
    if let Some(home) = dirs::home_dir() {
        profiles.insert(home.join("Documents/WindowsPowerShell/Microsoft.PowerShell_profile.ps1"));
        profiles.insert(home.join("Documents/PowerShell/Microsoft.PowerShell_profile.ps1"));
    }
    for profile in profiles {
        if std::fs::read_to_string(&profile).is_ok_and(|content| content.contains(START)) {
            if dry_run {
                println!("  remove Claude wrapper from {}", profile.display());
            } else if let Err(error) = configure(Shell::Bash, &profile, Path::new(""), true) {
                eprintln!("Cannot remove Claude wrapper: {error}");
            }
        }
    }
    if let Ok(companion) = companion_path()
        && companion.is_file()
    {
        if dry_run {
            println!("  remove {}", companion.display());
        } else if let Err(error) = std::fs::remove_file(&companion) {
            eprintln!(
                "Cannot remove wrapper executable {}: {error}",
                companion.display()
            );
        }
    }
}

#[cfg(test)]
#[path = "wrap_tests.rs"]
mod tests;
