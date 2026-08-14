#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

pub fn install() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        install_macos()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        install_linux()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("autostart not supported on this platform".to_string())
    }
}

pub fn uninstall() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        uninstall_macos()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        uninstall_linux()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("autostart not supported on this platform".to_string())
    }
}

#[allow(dead_code)]
pub fn stop() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        stop_macos()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        stop_linux()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("autostart not supported on this platform".to_string())
    }
}

#[allow(dead_code)]
pub fn start() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        start_macos()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        start_linux()?;
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("autostart not supported on this platform".to_string())
    }
}

// Both only called from the macOS LaunchAgent / Linux systemd install paths
// below, which are themselves target_os-gated -- gate the helpers to match
// so non-macOS/Linux builds (e.g. Windows) don't see them as dead code.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn agentflare_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "agentflare".to_string())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn daemon_home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

// A systemd/launchd-restarted daemon only inherits the service manager's
// bare default PATH, not the invoking shell's -- capture the real PATH at
// enable-time (which includes ~/.local/bin, ~/.cargo/bin, etc.) so a
// crash-restart doesn't strand the daemon without the agent CLIs it needs
// to dispatch jobs.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn daemon_path_env() -> String {
    std::env::var("PATH").unwrap_or_default()
}

// Command::args does not go through a shell, so a literal "gui/$(id -u)"
// string reaches launchctl unexpanded and every call below fails. Resolve
// the real uid via the libc dep already present for cfg(unix) targets.
#[cfg(target_os = "macos")]
fn gui_target() -> String {
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

// loginctl enable-linger needs a username, not a uid. $USER can be unset in
// non-interactive contexts (cron, some service managers), so fall back to
// the passwd entry for the real uid.
#[cfg(target_os = "linux")]
fn linux_username() -> Option<String> {
    if let Ok(user) = std::env::var("USER")
        && !user.is_empty()
    {
        return Some(user);
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_str()
            .ok()
            .map(String::from)
    }
}

// -- macOS LaunchAgent --

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    daemon_home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join("com.agentflare.daemon.plist")
}

#[cfg(target_os = "macos")]
fn install_macos() -> Result<(), String> {
    let path = plist_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {parent:?}: {e}"))?;
    }
    let binary = agentflare_binary();
    let label = "com.agentflare.daemon";
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>serve</string>
        <string>--_foreground-daemon</string>
    </array>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path_env}</string>
    </dict>
</dict>
</plist>"#,
        label = label,
        binary = binary,
        out = daemon_home_dir()
            .join("Library/Logs/agentflare-daemon.log")
            .display(),
        err = daemon_home_dir()
            .join("Library/Logs/agentflare-daemon.err")
            .display(),
        path_env = daemon_path_env(),
    );
    std::fs::write(&path, &plist).map_err(|e| format!("write plist: {e}"))?;

    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &gui_target(), &path.to_string_lossy()])
        .status()
        .map_err(|e| format!("launchctl bootstrap: {e}"))?;
    if !status.success() {
        return Err("launchctl bootstrap failed".to_string());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_macos() -> Result<(), String> {
    let path = plist_path();
    let status = std::process::Command::new("launchctl")
        .args([
            "bootout",
            &format!("{}/com.agentflare.daemon", gui_target()),
        ])
        .status()
        .map_err(|e| format!("launchctl bootout: {e}"))?;
    let _ = std::fs::remove_file(&path);
    if status.success() {
        Ok(())
    } else {
        Err("launchctl bootout failed".to_string())
    }
}

#[cfg(target_os = "macos")]
fn stop_macos() -> Result<(), String> {
    let status = std::process::Command::new("launchctl")
        .args([
            "kill",
            "SIGTERM",
            &format!("{}/com.agentflare.daemon", gui_target()),
        ])
        .status()
        .map_err(|e| format!("launchctl kill: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("launchctl kill failed".to_string())
    }
}

#[cfg(target_os = "macos")]
fn start_macos() -> Result<(), String> {
    let path = plist_path();
    let status = std::process::Command::new("launchctl")
        .args([
            "kickstart",
            &format!("{}/com.agentflare.daemon", gui_target()),
        ])
        .status()
        .map_err(|e| format!("launchctl kickstart: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        // fallback: bootstrap
        let status = std::process::Command::new("launchctl")
            .args(["bootstrap", &gui_target(), &path.to_string_lossy()])
            .status()
            .map_err(|e| format!("launchctl bootstrap: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("launchctl start failed".to_string())
        }
    }
}

// -- Linux systemd --

#[cfg(target_os = "linux")]
fn service_path() -> PathBuf {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| daemon_home_dir().join(".config"));
    config_home
        .join("systemd")
        .join("user")
        .join("agentflare-daemon.service")
}

#[cfg(target_os = "linux")]
fn install_linux() -> Result<(), String> {
    let path = service_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {parent:?}: {e}"))?;
    }
    let binary = agentflare_binary();
    let path_env = daemon_path_env();
    let unit = format!(
        "[Unit]\n\
         Description=agentflare daemon\n\
         After=network.target\n\n\
         [Service]\n\
         ExecStart={binary} serve --_foreground-daemon\n\
         Environment=\"PATH={path_env}\"\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         Type=simple\n\n\
         [Install]\n\
         WantedBy=default.target\n"
    );
    std::fs::write(&path, &unit).map_err(|e| format!("write service unit: {e}"))?;

    let status = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .map_err(|e| format!("systemctl daemon-reload: {e}"))?;
    if !status.success() {
        return Err("systemctl daemon-reload failed".to_string());
    }

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "agentflare-daemon.service"])
        .status()
        .map_err(|e| format!("systemctl enable: {e}"))?;
    if !status.success() {
        return Err("systemctl enable --now failed".to_string());
    }

    // A `systemctl --user` unit only starts on login (and can be torn down
    // after logout) unless linger is enabled for the user, so the
    // WantedBy=default.target above doesn't actually survive a headless
    // reboot without this. This can fail under restricted/non-interactive
    // setups (no polkit agent, containers) -- warn but don't fail install,
    // since the unit itself is still installed and enabled correctly.
    match linux_username() {
        Some(user) => {
            match std::process::Command::new("loginctl")
                .args(["enable-linger", &user])
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => eprintln!(
                    "warning: loginctl enable-linger failed ({status}); the daemon may not start on a headless reboot until this account logs in"
                ),
                Err(e) => eprintln!(
                    "warning: failed to run loginctl enable-linger: {e}; the daemon may not start on a headless reboot until this account logs in"
                ),
            }
        }
        None => eprintln!(
            "warning: could not determine username for loginctl enable-linger; the daemon may not start on a headless reboot until this account logs in"
        ),
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_linux() -> Result<(), String> {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", "agentflare-daemon.service"])
        .status();
    let path = service_path();
    let _ = std::fs::remove_file(&path);
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    // Deliberately not calling `loginctl disable-linger` here: linger is a
    // per-user, not per-unit, setting, and we have no record of whether it
    // was already on before install_linux() enabled it (or turned on for an
    // unrelated reason). Disabling it on uninstall risks breaking other
    // user services that depend on linger. Leaving it enabled is harmless.
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop_linux() -> Result<(), String> {
    let status = std::process::Command::new("systemctl")
        .args(["--user", "stop", "agentflare-daemon.service"])
        .status()
        .map_err(|e| format!("systemctl stop: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("systemctl stop failed".to_string())
    }
}

#[cfg(target_os = "linux")]
fn start_linux() -> Result<(), String> {
    let status = std::process::Command::new("systemctl")
        .args(["--user", "start", "agentflare-daemon.service"])
        .status()
        .map_err(|e| format!("systemctl start: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("systemctl start failed".to_string())
    }
}
