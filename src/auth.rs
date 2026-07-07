use crate::auth_crypt;
use crate::auth_db::{self, CooldownRow, ProfileHealth};
use crate::errors::AuthError;
use crate::paths::home;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

const VAULT_DIR: &str = "vault";

#[derive(Debug, Clone)]
pub struct AuthCatalog {
    pub agent_key: &'static str,
    pub files: &'static [&'static str],
}

static CATALOG: &[AuthCatalog] = &[
    AuthCatalog {
        agent_key: "claude-code",
        files: &[
            ".claude/.credentials.json",
            ".claude.json",
            ".config/claude-code/auth.json",
            "Library/Application Support/Claude/config.json",
        ],
    },
    AuthCatalog {
        agent_key: "codex",
        files: &[
            ".codex/auth.json",
        ],
    },
    AuthCatalog {
        agent_key: "antigravity",
        files: &[
            ".gemini/antigravity-cli/antigravity-oauth-token",
            ".gemini/google_accounts.json",
        ],
    },
    AuthCatalog {
        agent_key: "gemini",
        files: &[
            ".gemini/settings.json",
            ".gemini/oauth_creds.json",
        ],
    },
    AuthCatalog {
        agent_key: "opencode",
        files: &[
            ".opencode/auth.json",
        ],
    },
    AuthCatalog {
        agent_key: "copilot",
        files: &[
            ".copilot/auth.json",
        ],
    },
];

fn catalog_for(agent: &str) -> Option<&'static AuthCatalog> {
    CATALOG.iter().find(|c| c.agent_key == agent)
}

fn vault_dir() -> PathBuf {
    home().join(".local").join("share").join("agentflare").join(VAULT_DIR)
}

fn profile_dir(agent: &str, profile: &str) -> PathBuf {
    vault_dir().join(agent).join(profile)
}

fn validate_name(name: &str, kind: &'static str) -> Result<(), AuthError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        Err(AuthError::InvalidName { name: name.to_string(), kind: kind.to_string() })
    } else {
        Ok(())
    }
}

pub fn backup(agent: &str, profile: &str, json: bool) {
    let cat = match catalog_for(agent) {
        Some(c) => c,
        None => {
            fail("unknown agent", agent, json);
            return;
        }
    };
    let vault = profile_dir(agent, profile);
    fs::create_dir_all(&vault).expect("create vault dir");

    let mut backed = 0;
    let mut skipped = 0;
    let passphrase = auth_crypt::get_passphrase();
    for &rel in cat.files {
        let src = home().join(rel);
        let dest = vault.join(rel.rsplit('/').next().unwrap_or(rel));
        if src.exists() {
            let data = fs::read(&src).expect("read");
            if let Some(ref pw) = passphrase {
                let encrypted = auth_crypt::encrypt(&data, pw).expect("encrypt");
                fs::write(&dest, encrypted).expect("write");
            } else {
                fs::write(&dest, data).expect("write");
            }
            backed += 1;
        } else {
            skipped += 1;
        }
    }

    if json {
        let out = serde_json::json!({
            "agent": agent,
            "profile": profile,
            "backed": backed,
            "skipped": skipped,
            "vault": vault.to_string_lossy(),
        });
        println!("{}", out);
    } else if backed > 0 {
        println!("backed up {backed} file(s) for {agent}/{profile} (skipped {skipped} not found)");
    } else {
        println!("no auth files found for {agent} — nothing backed up (agent may not be set up)");
    }
}

pub fn resolve_name(agent: &str, name: &str) -> String {
    let conn = auth_db::open_or_rebuild();
    // Aliases always win (user explicitly mapped them)
    if let Some(real) = auth_db::resolve_alias(&conn, agent, name) {
        return real;
    }
    // Explicit profile name that exists in vault — use as-is, don't remap via project
    let vault_profiles = list_profiles(agent);
    if vault_profiles.contains(&name.to_string()) {
        return name.to_string();
    }
    // Project association: only applies when name doesn't match a vault profile
    let cwd = std::env::current_dir().unwrap_or_default();
    let path = cwd.to_string_lossy().to_string();
    if let Some(project_profile) = auth_db::get_project(&conn, &path, agent) {
        return project_profile;
    }
    name.to_string()
}

pub fn activate(agent: &str, profile: &str, json: bool) {
    let profile = resolve_name(agent, profile);
    if let Err(e) = validate_name(agent, "agent").and_then(|_| validate_name(&profile, "profile")) {
        fail(&e, "", json);
        return;
    }
    let cat = match catalog_for(agent) {
        Some(c) => c,
        None => {
            fail("unknown agent", agent, json);
            return;
        }
    };
    let vault = profile_dir(agent, &profile);
    if !vault.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "profile not found", "profile": profile}));
        } else {
            eprintln!("error: profile '{profile}' not found for {agent}");
        }
        return;
    }

    let mut restored = 0;
    let passphrase = auth_crypt::get_passphrase();
    for &rel in cat.files {
        let src = vault.join(
            rel.split('/').next_back().unwrap_or(rel)
        );
        if src.exists() {
            let dest = home().join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            let data = fs::read(&src).expect("read");
            if auth_crypt::is_encrypted(&data) {
                if let Some(ref pw) = passphrase {
                    if let Some(decrypted) = auth_crypt::decrypt(&data, pw) {
                        fs::write(&dest, decrypted).expect("write");
                    } else {
                        eprintln!("warning: cannot decrypt {} — wrong passphrase", src.display());
                        continue;
                    }
                } else {
                    eprintln!(
                        "warning: {} is encrypted but no passphrase set — set AGENTFLARE_VAULT_PASSPHRASE",
                        src.display()
                    );
                    continue;
                }
            } else {
                fs::write(&dest, data).expect("write");
            }
            restored += 1;
        }
    }

    if json {
        println!("{}", serde_json::json!({
            "agent": agent,
            "profile": profile,
            "restored": restored,
        }));
    } else {
        println!("activated {agent}/{profile} — restored {restored} file(s)");
    }
}

pub fn status(agent: Option<&str>, json: bool) {
    let agents: Vec<&AuthCatalog> = match agent {
        Some(a) => catalog_for(a).into_iter().collect(),
        None => CATALOG.iter().collect(),
    };

    let mut results = Vec::new();
    for cat in &agents {
        let profiles = list_profiles(cat.agent_key);
        if profiles.is_empty() {
            continue;
        }
        let active = detect_active(cat);
        if json {
            results.push(serde_json::json!({
                "agent": cat.agent_key,
                "profiles": profiles,
                "active": active,
            }));
        } else {
            println!("{}:", cat.agent_key);
            for p in &profiles {
                let mark = if Some(p.as_str()) == active.as_deref() { " *" } else { "" };
                println!("  {p}{mark}");
            }
            if active.is_none() {
                println!("  (no matching profile)");
            }
            println!();
        }
    }
    if json {
        println!("{}", serde_json::to_string(&results).unwrap());
    }
}

pub fn list_agents(json: bool) {
    let agents: Vec<String> = CATALOG.iter().map(|c| c.agent_key.to_string()).collect();
    if json {
        println!("{}", serde_json::to_string(&agents).unwrap());
    } else {
        for a in &agents {
            println!("{a}");
        }
    }
}

pub fn ls(agent: &str, json: bool) {
    if !catalog_for(agent).is_some() {
        fail("unknown agent", agent, json);
        return;
    }
    let profiles = list_profiles(agent);
    if json {
        println!("{}", serde_json::to_string(&profiles).unwrap());
    } else if profiles.is_empty() {
        println!("no profiles for {agent}");
    } else {
        for p in &profiles {
            println!("{p}");
        }
    }
}

pub fn delete(agent: &str, profile: &str, json: bool) {
    let dir = profile_dir(agent, profile);
    if !dir.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "not found"}));
        } else {
            eprintln!("profile '{profile}' not found for {agent}");
        }
        return;
    }
    fs::remove_dir_all(&dir).expect("remove dir");
    if json {
        println!("{}", serde_json::json!({"deleted": true, "agent": agent, "profile": profile}));
    } else {
        println!("deleted {agent}/{profile}");
    }
}

pub fn clear(agent: &str, json: bool) {
    let cat = match catalog_for(agent) {
        Some(c) => c,
        None => {
            fail("unknown agent", agent, json);
            return;
        }
    };
    let mut removed = 0;
    for &rel in cat.files {
        let path = home().join(rel);
        if path.exists() {
            fs::remove_file(&path).ok();
            removed += 1;
        }
    }
    if json {
        println!("{}", serde_json::json!({"cleared": removed, "agent": agent}));
    } else {
        println!("cleared {removed} auth file(s) for {agent}");
    }
}

pub fn rename(agent: &str, old: &str, new: &str, json: bool) {
    let old_dir = profile_dir(agent, old);
    if !old_dir.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "not found"}));
        } else {
            eprintln!("profile '{old}' not found for {agent}");
        }
        return;
    }
    let new_dir = profile_dir(agent, new);
    if new_dir.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "destination exists"}));
        } else {
            eprintln!("profile '{new}' already exists for {agent}");
        }
        return;
    }
    fs::create_dir_all(new_dir.parent().unwrap()).expect("create parent");
    fs::rename(&old_dir, &new_dir).expect("rename");
    if json {
        println!("{}", serde_json::json!({"renamed": true, "agent": agent, "old": old, "new": new}));
    } else {
        println!("renamed {agent}/{old} → {new}");
    }
}

fn list_profiles(agent: &str) -> Vec<String> {
    let dir = vault_dir().join(agent);
    if !dir.exists() {
        return Vec::new();
    }
    let mut profiles = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                profiles.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    profiles.sort();
    profiles
}

fn detect_active(cat: &AuthCatalog) -> Option<String> {
    let dir = vault_dir().join(cat.agent_key);
    if !dir.exists() {
        return None;
    }
    let live_hash = hash_live_files(cat);

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let profile = entry.file_name().to_string_lossy().to_string();
                if hash_vault_profile(cat, &profile) == live_hash {
                    return Some(profile);
                }
            }
        }
    }
    None
}

fn hash_live_files(cat: &AuthCatalog) -> String {
    let mut hasher = Sha256::new();
    for &rel in cat.files {
        let path = home().join(rel);
        if path.exists() {
            if let Ok(data) = fs::read(&path) {
                hasher.update(rel.as_bytes());
                hasher.update(data);
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

fn hash_vault_profile(cat: &AuthCatalog, profile: &str) -> String {
    let dir = profile_dir(cat.agent_key, profile);
    let mut hasher = Sha256::new();
    for &rel in cat.files {
        let fname = rel.split('/').next_back().unwrap_or(rel);
        let path = dir.join(fname);
        if path.exists() {
            if let Ok(data) = fs::read(&path) {
                hasher.update(rel.as_bytes());
                hasher.update(data);
            }
        }
    }
    format!("{:x}", hasher.finalize())
}

fn fail(msg: &(impl std::fmt::Display + ?Sized), detail: &str, json: bool) {
    if json {
        println!("{}", serde_json::json!({"error": msg.to_string(), "detail": detail}));
    } else {
        eprintln!("error: {msg}: {detail}");
    }
}

fn validate_algorithm(name: &str) -> Result<&str, AuthError> {
    match name {
        "smart" | "round-robin" | "random" => Ok(name),
        _ => Err(AuthError::UnknownAlgorithm(name.to_string())),
    }
}

pub fn rotate(agent: &str, algorithm: &str, json: bool) {
    let algorithm = match validate_algorithm(algorithm) {
        Ok(a) => a,
        Err(e) => { fail(&e, "", json); return; }
    };
    let conn = auth_db::open_or_rebuild();
    let cooldowns = auth_db::list_cooldowns(&conn, Some(agent));
    let health = auth_db::list_health(&conn, agent);
    let vault_profiles = list_profiles(agent);
    let active = filter_active(&vault_profiles, &cooldowns);

    if active.is_empty() {
        if json {
            println!("{}", serde_json::json!({"error": "no non-cooldown profiles available"}));
        } else {
            eprintln!("error: all profiles are in cooldown");
        }
        return;
    }

    let chosen = select_profile(&conn, agent, algorithm, &health, &active);
    activate(agent, &chosen, json);
    auth_db::set_rotation_last(&conn, agent, &chosen, algorithm);
}

fn filter_active(profiles: &[String], cooldowns: &[CooldownRow]) -> Vec<String> {
    profiles
        .iter()
        .filter(|p| !cooldowns.iter().any(|c| c.profile == **p))
        .cloned()
        .collect()
}

fn select_profile(
    conn: &Connection,
    agent: &str,
    algorithm: &str,
    health: &[ProfileHealth],
    profiles: &[String],
) -> String {
    match algorithm {
        "round-robin" => round_robin(conn, agent, profiles),
        "random" => random_pick(profiles),
        _ => smart_pick(health, profiles),
    }
}

fn smart_pick(health: &[ProfileHealth], profiles: &[String]) -> String {
    let mut scored: Vec<(String, f64)> = profiles
        .iter()
        .map(|p| {
            let h = health.iter().find(|h| h.profile == *p);
            let base = match h.map(|h| h.status.as_str()) {
                Some("healthy") => 100.0,
                Some("warning") => 50.0,
                Some("critical") => 0.0,
                _ => 100.0,
            };
            let penalty = h.map(|h| h.penalty).unwrap_or(0.0);
            let recency = if h.and_then(|h| h.last_used_at.as_ref()).is_some() { 0.0 } else { 10.0 };
            let jitter = (rand::random::<f64>() * 10.0) - 5.0;
            (p.clone(), base - penalty + recency + jitter)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    scored[0].0.clone()
}

fn round_robin(conn: &Connection, agent: &str, profiles: &[String]) -> String {
    if let Some((last, _)) = auth_db::get_rotation_last(conn, agent) {
        if let Some(pos) = profiles.iter().position(|p| *p == last) {
            let next = (pos + 1) % profiles.len();
            return profiles[next].clone();
        }
    }
    profiles[0].clone()
}

fn random_pick(profiles: &[String]) -> String {
    let idx = rand::random::<usize>() % profiles.len();
    profiles[idx].clone()
}

pub fn next(agent: &str, algorithm: &str, json: bool) {
    let algorithm = match validate_algorithm(algorithm) {
        Ok(a) => a,
        Err(e) => { fail(&e, "", json); return; }
    };
    let conn = auth_db::open_or_rebuild();
    let cooldowns = auth_db::list_cooldowns(&conn, Some(agent));
    let health = auth_db::list_health(&conn, agent);
    let profiles = list_profiles(agent);
    let active = filter_active(&profiles, &cooldowns);

    let chosen = if active.is_empty() {
        "(none — all cooldown'd)".to_string()
    } else {
        select_profile(&conn, agent, algorithm, &health, &active)
    };
    if json {
        println!("{}", serde_json::json!({"agent": agent, "next": chosen, "algorithm": algorithm}));
    } else {
        println!("next rotation for {agent} [{algorithm}]: {chosen}");
    }
}

pub fn pick(agent: &str) {
    let profiles = list_profiles(agent);
    if profiles.is_empty() {
        println!("no profiles for {agent}");
        return;
    }
    for (i, p) in profiles.iter().enumerate() {
        println!("  [{}] {p}", i + 1);
    }
    print!("choose profile: ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    if let Ok(idx) = input.trim().parse::<usize>() {
        if idx > 0 && idx <= profiles.len() {
            activate(agent, &profiles[idx - 1], false);
            return;
        }
    }
    eprintln!("invalid selection");
}

pub fn cooldown_set(target: &str, minutes: Option<u32>, json: bool) {
    let (agent, profile) = match parse_target(target) {
        Some(p) => p,
        None => {
            if json {
                println!("{}", serde_json::json!({"error": "expected <agent>/<profile>"}));
            } else {
                eprintln!("error: expected <agent>/<profile>");
            }
            return;
        }
    };
    let mins = minutes.unwrap_or(60);
    let conn = auth_db::open_or_rebuild();
    auth_db::set_cooldown(&conn, &agent, &profile, mins, "manual");
    if json {
        println!(
            "{}",
            serde_json::json!({"agent": agent, "profile": profile, "cooldown_minutes": mins})
        );
    } else {
        println!("cooldown set: {agent}/{profile} for {mins} minutes");
    }
}

pub fn cooldown_list(agent: Option<&str>, json: bool) {
    let conn = auth_db::open_or_rebuild();
    let list = auth_db::list_cooldowns(&conn, agent);
    if json {
        println!(
            "{}",
            serde_json::to_string(
                &list
                    .iter()
                    .map(|c| serde_json::json!({
                        "agent": c.agent,
                        "profile": c.profile,
                        "until": c.until,
                        "reason": c.reason,
                    }))
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
    } else if list.is_empty() {
        println!("no active cooldowns");
    } else {
        for c in &list {
            println!(
                "  {}/{}  until {}  {}",
                c.agent,
                c.profile,
                c.until,
                c.reason.as_deref().unwrap_or("")
            );
        }
    }
}

pub fn cooldown_clear(target: &str, json: bool) {
    let (agent, profile) = match parse_target(target) {
        Some(p) => p,
        None => {
            if json {
                println!("{}", serde_json::json!({"error": "expected <agent>/<profile>"}));
            } else {
                eprintln!("error: expected <agent>/<profile>");
            }
            return;
        }
    };
    let conn = auth_db::open_or_rebuild();
    auth_db::clear_cooldown(&conn, &agent, &profile);
    if json {
        println!(
            "{}",
            serde_json::json!({"cleared": true, "agent": agent, "profile": profile})
        );
    } else {
        println!("cooldown cleared: {agent}/{profile}");
    }
}

fn parse_target(target: &str) -> Option<(String, String)> {
    if target.is_empty() { return None; }
    let parts: Vec<&str> = target.splitn(2, '/').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

pub fn set_alias_cmd(agent: &str, profile: &str, alias: &str, json: bool) {
    let conn = auth_db::open_or_rebuild();
    auth_db::set_alias(&conn, agent, alias, profile);
    if json {
        println!(
            "{}",
            serde_json::json!({"agent": agent, "alias": alias, "profile": profile})
        );
    } else {
        println!("alias set: {agent}/{alias} -> {profile}");
    }
}

pub fn project_set(agent: &str, profile: &str, json: bool) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("cannot determine current directory: {e}");
            if json {
                println!("{}", serde_json::json!({"error": msg}));
            } else {
                eprintln!("error: {msg}");
            }
            return;
        }
    };
    let path = cwd.to_string_lossy().to_string();
    let conn = auth_db::open_or_rebuild();
    auth_db::set_project(&conn, &path, agent, profile);
    if json {
        println!(
            "{}",
            serde_json::json!({"path": path, "agent": agent, "profile": profile})
        );
    } else {
        println!("project set: {path} -> {agent}/{profile}");
    }
}

pub fn project_unset(agent: &str, json: bool) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            let msg = format!("cannot determine current directory: {e}");
            if json {
                println!("{}", serde_json::json!({"error": msg}));
            } else {
                eprintln!("error: {msg}");
            }
            return;
        }
    };
    let path = cwd.to_string_lossy().to_string();
    let conn = auth_db::open_or_rebuild();
    auth_db::unset_project(&conn, &path, agent);
    if json {
        println!(
            "{}",
            serde_json::json!({"path": path, "agent": agent, "unset": true})
        );
    } else {
        println!("project unset: {path}/{agent}");
    }
}

const ISOLATE_DIR: &str = "isolate";

fn isolates_dir() -> PathBuf {
    home().join(".local").join("share").join("agentflare").join(ISOLATE_DIR)
}

pub fn isolate_add(agent: &str, profile: &str, json: bool) {
    let dir = isolates_dir().join(agent).join(profile);
    if dir.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "isolated profile already exists"}));
        } else {
            eprintln!("isolated profile '{agent}/{profile}' already exists");
        }
        return;
    }
    fs::create_dir_all(&dir).expect("create isolate dir");

    // Symlink shared host files
    for host_file in &[".ssh", ".gitconfig", ".git-credentials"] {
        let src = home().join(host_file);
        if src.exists() {
            symlink_or_copy(&src, &dir.join(host_file));
        }
    }

    // Copy auth files from vault profile
    activate_into(agent, profile, &dir);

    if json {
        println!("{}", serde_json::json!({"agent": agent, "profile": profile, "isolate_dir": dir.to_string_lossy()}));
    } else {
        println!("isolated profile created: {agent}/{profile} at {}", dir.display());
    }
}

pub fn isolate_ls(agent: Option<&str>, json: bool) {
    let dir = isolates_dir();
    if !dir.exists() {
        if json {
            println!("[]");
        } else {
            println!("no isolated profiles");
        }
        return;
    }
    let mut results: Vec<serde_json::Value> = Vec::new();
    let agents: Vec<String> = match agent {
        Some(a) => vec![a.to_string()],
        None => fs::read_dir(&dir).ok().map(|entries| {
            entries.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string()).collect()
        }).unwrap_or_default(),
    };
    for a in &agents {
        let agent_dir = dir.join(a);
        if !agent_dir.exists() { continue; }
        if let Ok(entries) = fs::read_dir(&agent_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let p = entry.file_name().to_string_lossy().to_string();
                    if json {
                        results.push(serde_json::json!({"agent": a, "profile": p}));
                    } else {
                        println!("{a}/{p}");
                    }
                }
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string(&results).unwrap());
    }
}

pub fn isolate_delete(agent: &str, profile: &str, json: bool) {
    let dir = isolates_dir().join(agent).join(profile);
    if !dir.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "not found"}));
        } else {
            eprintln!("isolated profile '{agent}/{profile}' not found");
        }
        return;
    }
    fs::remove_dir_all(&dir).expect("remove isolate dir");
    if json {
        println!("{}", serde_json::json!({"deleted": true, "agent": agent, "profile": profile}));
    } else {
        println!("deleted isolated profile: {agent}/{profile}");
    }
}

pub fn auth_exec(agent: &str, profile: &str, args: &[String], json: bool) {
    let dir = isolates_dir().join(agent).join(profile);
    if !dir.exists() {
        if json {
            println!("{}", serde_json::json!({"error": "isolated profile not found"}));
        } else {
            eprintln!("error: isolated profile '{agent}/{profile}' not found — run 'auth isolate add' first");
        }
        return;
    }

    if args.is_empty() {
        eprintln!("error: no command specified after --");
        return;
    }

    let binary = &args[0];
    let rest = &args[1..];

    let mut cmd = std::process::Command::new(binary);
    cmd.args(rest);
    cmd.env("HOME", &dir);
    #[cfg(windows)]
    {
        cmd.env("USERPROFILE", &dir);
        cmd.env("HOMEDRIVE", "");
        cmd.env("HOMEPATH", &dir);
    }
    let status = cmd
        .spawn()
        .and_then(|mut c| c.wait())
        .unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

pub fn auth_login(agent: &str, profile: &str, args: &[String], json: bool) {
    let dir = isolates_dir().join(agent).join(profile);
    if !dir.exists() {
        // Auto-create isolate if it doesn't exist
        isolate_add(agent, profile, json);
    }

    if args.is_empty() {
        eprintln!("error: no login command specified after --");
        return;
    }

    let binary = &args[0];
    let rest = &args[1..];

    let mut cmd = std::process::Command::new(binary);
    cmd.args(rest);
    cmd.env("HOME", &dir);
    #[cfg(windows)]
    {
        cmd.env("USERPROFILE", &dir);
        cmd.env("HOMEDRIVE", "");
        cmd.env("HOMEPATH", &dir);
    }
    let status = cmd
        .spawn()
        .and_then(|mut c| c.wait())
        .unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });

    if status.success() {
        let passphrase = auth_crypt::get_passphrase();
        let dir = isolates_dir().join(agent).join(profile);
        for &rel in CATALOG.iter().find(|c| c.agent_key == agent).map(|c| c.files).unwrap_or(&[]) {
            let dest = profile_dir(agent, profile).join(rel.rsplit('/').next().unwrap_or(rel));
            let src = dir.join(rel);
            if src.exists() {
                let data = fs::read(&src).expect("read");
                fs::create_dir_all(dest.parent().unwrap()).ok();
                if let Some(ref pw) = passphrase {
                    let encrypted = auth_crypt::encrypt(&data, pw).expect("encrypt");
                    fs::write(&dest, encrypted).expect("write");
                } else {
                    fs::write(&dest, data).expect("write");
                }
            }
        }
        if !json {
            println!("login complete — auth backed up to vault profile '{agent}/{profile}'");
        }
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}


fn activate_into(agent: &str, profile: &str, target_dir: &std::path::Path) {
    let cat = match catalog_for(agent) { Some(c) => c, None => { return; } };
    let vault = profile_dir(agent, profile);
    let passphrase = auth_crypt::get_passphrase();
    for &rel in cat.files {
        let src = vault.join(rel.rsplit('/').next().unwrap_or(rel));
        if src.exists() {
            let dest = target_dir.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).ok();
            }
            let data = fs::read(&src).expect("read");
            if auth_crypt::is_encrypted(&data) {
                if let Some(ref pw) = passphrase {
                    if let Some(decrypted) = auth_crypt::decrypt(&data, pw) {
                        fs::write(&dest, decrypted).expect("write");
                    } else {
                        eprintln!("warning: cannot decrypt {} — wrong passphrase", src.display());
                        continue;
                    }
                } else {
                    eprintln!("warning: {} is encrypted but no passphrase set", src.display());
                    continue;
                }
            } else {
                fs::write(&dest, data).expect("write");
            }
        }
    }
}

#[cfg(not(windows))]
fn symlink_or_copy(src: &std::path::Path, dest: &std::path::Path) {
    std::os::unix::fs::symlink(src, dest).ok();
}

#[cfg(windows)]
fn symlink_or_copy(src: &std::path::Path, dest: &std::path::Path) {
    fs::copy(src, dest).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn backup_and_activate_roundtrips() {
        with_temp_home(|| {
            let creds = home().join(".claude").join(".credentials.json");
            fs::create_dir_all(creds.parent().unwrap()).unwrap();
            fs::write(&creds, r#"{"token": "abc123"}"#).unwrap();

            backup("claude-code", "alice", false);
            clear("claude-code", false);

            assert!(!creds.exists());

            activate("claude-code", "alice", false);

            let content = fs::read_to_string(&creds).unwrap();
            assert_eq!(content, r#"{"token": "abc123"}"#);
        });
    }

    #[test]
    fn status_detects_active_profile() {
        with_temp_home(|| {
            let creds = home().join(".claude").join(".credentials.json");
            fs::create_dir_all(creds.parent().unwrap()).unwrap();
            fs::write(&creds, r#"{"token": "abc"}"#).unwrap();

            backup("claude-code", "alice", false);

            let active = detect_active(&CATALOG[0]);
            assert_eq!(active.as_deref(), Some("alice"));
        });
    }

    #[test]
    fn rename_moves_profile() {
        with_temp_home(|| {
            let creds = home().join(".claude").join(".credentials.json");
            fs::create_dir_all(creds.parent().unwrap()).unwrap();
            fs::write(&creds, "x").unwrap();
            backup("claude-code", "old", false);

            rename("claude-code", "old", "new", false);

            let profiles = list_profiles("claude-code");
            assert_eq!(profiles, vec!["new"]);
            assert!(!profile_dir("claude-code", "old").exists());
            assert!(profile_dir("claude-code", "new").exists());
        });
    }

    #[test]
    fn delete_removes_profile() {
        with_temp_home(|| {
            let p = profile_dir("claude-code", "test");
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join("dummy"), "x").unwrap();

            delete("claude-code", "test", false);

            assert!(!p.exists());
        });
    }

    #[test]
    fn clear_removes_live_auth_files() {
        with_temp_home(|| {
            let creds = home().join(".claude").join(".credentials.json");
            fs::create_dir_all(creds.parent().unwrap()).unwrap();
            fs::write(&creds, "x").unwrap();

            clear("claude-code", false);

            assert!(!creds.exists());
        });
    }

    fn setup_vault_profile(agent: &str, profile: &str, content: &str) {
        let dir = profile_dir(agent, profile);
        fs::create_dir_all(&dir).unwrap();
        // Use the catalog's first file name for the vault entry
        let cat = CATALOG.iter().find(|c| c.agent_key == agent).unwrap();
        let fname = cat.files[0].rsplit('/').next().unwrap_or(cat.files[0]);
        fs::write(dir.join(fname), content).unwrap();
    }

    #[test]
    fn cooldown_set_and_rotate_skips() {
        with_temp_home(|| {
            setup_vault_profile("claude-code", "alice", r#"{"token":"a"}"#);
            setup_vault_profile("claude-code", "bob", r#"{"token":"b"}"#);

            let conn = auth_db::open_or_rebuild();
            auth_db::set_cooldown(&conn, "claude-code", "alice", 60, "test");

            let profiles = list_profiles("claude-code");
            let cooldowns = auth_db::list_cooldowns(&conn, Some("claude-code"));
            let active = filter_active(&profiles, &cooldowns);
            assert_eq!(active.len(), 1);
            assert_eq!(active[0], "bob");
        });
    }

    #[test]
    fn alias_activates_profile() {
        with_temp_home(|| {
            setup_vault_profile("claude-code", "work@company.com", r#"{"token":"x"}"#);
            let conn = auth_db::open_or_rebuild();
            auth_db::set_alias(&conn, "claude-code", "w", "work@company.com");

            let resolved = auth_db::resolve_alias(&conn, "claude-code", "w");
            assert_eq!(resolved, Some("work@company.com".to_string()));

            // activate via alias
            activate("claude-code", "w", false);
            let creds = home().join(".claude").join(".credentials.json");
            assert!(creds.exists());
            assert_eq!(fs::read_to_string(&creds).unwrap(), r#"{"token":"x"}"#);
        });
    }

    #[test]
    fn full_rotate_flow() {
        with_temp_home(|| {
            let conn = auth_db::open_or_rebuild();
            setup_vault_profile("claude-code", "alice", r#"{"token":"a"}"#);
            setup_vault_profile("claude-code", "bob", r#"{"token":"b"}"#);
            setup_vault_profile("claude-code", "carol", r#"{"token":"c"}"#);

            auth_db::set_cooldown(&conn, "claude-code", "alice", 60, "manual");
            auth_db::record_error(&conn, "claude-code", "bob", "502 Bad Gateway");

            // rotate() should pick carol — alice cooldown'd, bob penalized
            rotate("claude-code", "smart", false);

            // verify carol was activated (auth file restored)
            let creds = home().join(".claude").join(".credentials.json");
            assert!(creds.exists());
            assert_eq!(fs::read_to_string(&creds).unwrap(), r#"{"token":"c"}"#);

            // verify rotation last was set
            let last = auth_db::get_rotation_last(&conn, "claude-code");
            assert_eq!(last, Some(("carol".to_string(), "smart".to_string())));
        });
    }

    #[test]
    fn round_robin_cycles_profiles() {
        with_temp_home(|| {
            let conn = auth_db::open_or_rebuild();
            let profiles = vec!["alice".to_string(), "bob".to_string(), "carol".to_string()];

            let first = round_robin(&conn, "claude-code", &profiles);
            assert_eq!(first, "alice"); // no last → first profile

            auth_db::set_rotation_last(&conn, "claude-code", "alice", "round-robin");
            let second = round_robin(&conn, "claude-code", &profiles);
            assert_eq!(second, "bob"); // alice → bob

            auth_db::set_rotation_last(&conn, "claude-code", "carol", "round-robin");
            let third = round_robin(&conn, "claude-code", &profiles);
            assert_eq!(third, "alice"); // carol wraps to alice
        });
    }

    #[test]
    fn parse_target_validates() {
        assert_eq!(parse_target("claude-code/alice"), Some(("claude-code".to_string(), "alice".to_string())));
        assert_eq!(parse_target(""), None);
        assert_eq!(parse_target("/"), None);
        assert_eq!(parse_target("a/"), None);
        assert_eq!(parse_target("/b"), None);
        assert_eq!(parse_target("no-slash"), None);
        assert_eq!(parse_target("a/b/c"), Some(("a".to_string(), "b/c".to_string())));
    }

    #[test]
    fn next_shows_preview() {
        with_temp_home(|| {
            let conn = auth_db::open_or_rebuild();
            setup_vault_profile("claude-code", "alice", r#"{"token":"a"}"#);
            setup_vault_profile("claude-code", "bob", r#"{"token":"b"}"#);

            auth_db::set_cooldown(&conn, "claude-code", "bob", 60, "test");

            // next should pick alice (bob cooldown'd)
            let health = auth_db::list_health(&conn, "claude-code");
            let profiles = list_profiles("claude-code");
            let cooldowns = auth_db::list_cooldowns(&conn, Some("claude-code"));
            let active = filter_active(&profiles, &cooldowns);
            let picked = select_profile(&conn, "claude-code", "smart", &health, &active);
            assert_eq!(picked, "alice");
        });
    }
}
