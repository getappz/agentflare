//! `flared service print` — autostart recipes. v1 prints instructions; it
//! does not install anything.

use std::path::Path;

/// Recipe text for the given platform ("windows" | "linux" | "macos").
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn autostart_recipe(platform: &str, exe: &Path) -> String {
    let exe = exe.display().to_string();
    let exe_xml = xml_escape(&exe);
    match platform {
        "windows" => format!(
            r#"# Register flared as a logon task (run in an elevated or user shell):
schtasks /Create /TN "flared" /TR "\"{exe}\" serve" /SC ONLOGON /RL LIMITED /F

# Start it now without waiting for the next logon:
schtasks /Run /TN "flared"

# Remove:
schtasks /Delete /TN "flared" /F
"#
        ),
        "linux" => format!(
            r#"# Save as ~/.config/systemd/user/flared.service:
[Unit]
Description=flared - AI-agent workload hygiene supervisor

[Service]
ExecStart="{exe}" serve
Restart=on-failure

[Install]
WantedBy=default.target

# Then:
systemctl --user enable --now flared
"#
        ),
        "macos" => format!(
            r#"# Save as ~/Library/LaunchAgents/com.getappz.flared.plist:
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.getappz.flared</string>
  <key>ProgramArguments</key><array>
    <string>{exe_xml}</string><string>serve</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>

# Then:
launchctl load ~/Library/LaunchAgents/com.getappz.flared.plist
"#
        ),
        other => format!("no autostart recipe for platform '{other}'; run '{exe} serve' manually"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_recipe_uses_task_scheduler() {
        let text = autostart_recipe("windows", Path::new("C:/bin/flared.exe"));
        assert!(text.contains("schtasks"));
        assert!(text.contains("C:/bin/flared.exe"));
    }

    #[test]
    fn linux_recipe_is_a_systemd_unit() {
        let text = autostart_recipe("linux", Path::new("/usr/local/bin/flared"));
        assert!(text.contains("[Service]"));
        assert!(text.contains("systemctl --user enable"));
    }

    #[test]
    fn macos_recipe_is_a_launchd_plist() {
        let text = autostart_recipe("macos", Path::new("/usr/local/bin/flared"));
        assert!(text.contains("launchctl"));
        assert!(text.contains("plist"));
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let text = autostart_recipe("linux", Path::new("/opt/my tools/flared"));
        assert!(text.contains("ExecStart=\"/opt/my tools/flared\" serve"));
        let text = autostart_recipe("windows", Path::new("C:/My Tools/flared.exe"));
        assert!(text.contains(r#"\"C:/My Tools/flared.exe\" serve"#));
    }

    #[test]
    fn plist_xml_escapes_special_characters() {
        let text = autostart_recipe("macos", Path::new("/opt/a&b/flared"));
        assert!(text.contains("/opt/a&amp;b/flared"));
        assert!(!text.contains("<string>/opt/a&b/flared</string>"));
    }

    #[test]
    fn unknown_platform_gets_manual_fallback() {
        let text = autostart_recipe("freebsd", Path::new("/bin/flared"));
        assert!(text.contains("no autostart recipe"));
    }
}
