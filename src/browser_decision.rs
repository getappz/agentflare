//! One-use observed browser decisions, inspired by browser-use/jev-ultrafast.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct Decision {
    action: String,
    positionals: Vec<String>,
    fingerprint: String,
    created: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub fn validate(action: &str, args: &[String]) -> Result<(), String> {
    let count = match action {
        "click" | "hover" | "check" | "uncheck" => 1,
        "fill" | "type" | "select" => 2,
        _ => return Err("plan requires click, hover, check, uncheck, fill, type or select".into()),
    };
    if args.len() != count
        || !args[0]
            .strip_prefix("@e")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
    {
        return Err(
            "plan requires an observed @e reference and the operation's exact arguments".into(),
        );
    }
    if args.iter().any(|a| a.starts_with('-') || a.len() > 64_000) {
        return Err("decision arguments cannot be options or exceed 64000 bytes".into());
    }
    Ok(())
}

fn observe(
    run: &mut impl FnMut(&str, &[String]) -> Result<String, String>,
) -> Result<(String, String), String> {
    let url = run("get", &["url".into()])?;
    let snapshot = run("snapshot", &[])?;
    if snapshot.len() >= 1_000_000 || snapshot.contains("[truncated") {
        return Err("snapshot is incomplete; cannot plan a decision".into());
    }
    Ok((hash(&format!("{url}\0{snapshot}")), snapshot))
}

pub fn plan_with(
    dir: &Path,
    session: &str,
    action: &str,
    positionals: &[String],
    mut run: impl FnMut(&str, &[String]) -> Result<String, String>,
) -> Result<String, String> {
    validate(action, positionals)?;
    let (fingerprint, snapshot) = observe(&mut run)?;
    if !snapshot.contains(&format!("[ref={}]", &positionals[0][1..])) {
        return Err("target is absent from the observed snapshot".into());
    }
    let dir = dir.join(hash(session));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let token = format!("{:032x}", rand::random::<u128>());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(dir.join(&token)).map_err(|e| e.to_string())?;
    let decision = Decision {
        action: action.into(),
        positionals: positionals.to_vec(),
        fingerprint,
        created: now(),
    };
    file.write_all(&serde_json::to_vec(&decision).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(token)
}

pub fn act_with(
    dir: &Path,
    session: &str,
    token: &str,
    mut run: impl FnMut(&str, &[String]) -> Result<String, String>,
) -> Result<String, String> {
    if token.len() != 32 || !token.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("invalid decision id".into());
    }
    let dir = dir.join(hash(session));
    let claimed = dir.join(format!("{token}.{:032x}.consumed", rand::random::<u128>()));
    // Atomic claim: retries cannot execute twice, including after an uncertain backend error.
    std::fs::rename(dir.join(token), &claimed)
        .map_err(|_| "decision missing or already consumed".to_string())?;
    let raw = std::fs::read(&claimed);
    std::fs::remove_file(&claimed).map_err(|e| e.to_string())?;
    let decision: Decision = serde_json::from_slice(&raw.map_err(|e| e.to_string())?)
        .map_err(|_| "invalid decision record".to_string())?;
    validate(&decision.action, &decision.positionals)?;
    if now().saturating_sub(decision.created) > 300 || decision.created > now() {
        return Err("decision expired; plan again".into());
    }
    if observe(&mut run)?.0 != decision.fingerprint {
        return Err("page changed; plan again".into());
    }
    // ponytail: freshness is best-effort; atomic DOM execution needs backend support.
    run(&decision.action, &decision.positionals)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(action: &str, _: &[String]) -> Result<String, String> {
        Ok(match action {
            "get" => "https://example.com".into(),
            "snapshot" => "- button \"Save\" [ref=e1]".into(),
            _ => "clicked".into(),
        })
    }

    #[test]
    fn research_patterns_browser_decision_executes_once() {
        let dir = tempfile::tempdir().unwrap();
        let token = plan_with(dir.path(), "s", "click", &["@e1".into()], page).unwrap();
        assert_eq!(act_with(dir.path(), "s", &token, page).unwrap(), "clicked");
        assert!(
            act_with(dir.path(), "s", &token, |_, _| panic!(
                "reused decision executed"
            ))
            .is_err()
        );
    }

    #[test]
    fn research_patterns_browser_stale_decision_is_consumed_without_action() {
        let dir = tempfile::tempdir().unwrap();
        let token = plan_with(dir.path(), "s", "click", &["@e1".into()], page).unwrap();
        let result = act_with(dir.path(), "s", &token, |action, _| match action {
            "get" => Ok("https://example.com/other".into()),
            "snapshot" => Ok("- button \"Save\" [ref=e1]".into()),
            _ => panic!("stale decision executed"),
        });
        assert!(result.unwrap_err().contains("changed"));
        assert!(act_with(dir.path(), "s", &token, page).is_err());
    }

    #[test]
    fn research_patterns_browser_rejects_unobserved_and_arbitrary_actions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            plan_with(dir.path(), "s", "eval", &["@e1".into()], |_, _| panic!(
                "invalid action reached backend"
            ))
            .is_err()
        );
        assert!(plan_with(dir.path(), "s", "click", &["@e2".into()], page).is_err());
        assert!(
            act_with(dir.path(), "s", "../escape", |_, _| panic!(
                "invalid token reached backend"
            ))
            .is_err()
        );
    }

    #[test]
    fn research_patterns_browser_failure_consumes_ticket_and_session_is_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let token = plan_with(dir.path(), "s", "click", &["@e1".into()], page).unwrap();
        assert!(
            act_with(dir.path(), "other", &token, |_, _| panic!(
                "wrong session reached backend"
            ))
            .is_err()
        );
        assert!(
            act_with(dir.path(), "s", &token, |a, p| if a == "click" {
                Err("uncertain action".into())
            } else {
                page(a, p)
            })
            .is_err()
        );
        assert!(act_with(dir.path(), "s", &token, |_, _| panic!("retry executed")).is_err());
    }

    #[test]
    fn research_patterns_browser_expiry_rejects_before_backend() {
        let dir = tempfile::tempdir().unwrap();
        let token = plan_with(dir.path(), "s", "click", &["@e1".into()], page).unwrap();
        let path = dir.path().join(hash("s")).join(&token);
        let mut decision: Decision =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        decision.created = now() - 301;
        std::fs::write(path, serde_json::to_vec(&decision).unwrap()).unwrap();
        assert!(
            act_with(dir.path(), "s", &token, |_, _| panic!(
                "expired decision executed"
            ))
            .unwrap_err()
            .contains("expired")
        );
    }
}
