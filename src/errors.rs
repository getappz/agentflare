use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum AgentDetectError {
    #[error("{0} produced no output")]
    NoOutput(String),
    #[error("{0} timed out after {1:?}")]
    Timeout(String, std::time::Duration),
    #[error("version output was not valid UTF-8 for {0}")]
    InvalidUtf8(String),
    #[error("{0}")]
    Simulated(String),
}

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum AuthError {
    #[error("invalid {kind}: '{name}' (must not contain path separators)")]
    InvalidName { name: String, kind: String },
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    #[error("unknown algorithm: '{0}' (valid: smart, round-robin, random)")]
    UnknownAlgorithm(String),
    #[error("profile '{profile}' not found for {agent}")]
    ProfileNotFound { agent: String, profile: String },
    #[error("profile '{new}' already exists for {agent}")]
    ProfileExists { agent: String, new: String },
    #[error("all profiles are in cooldown")]
    AllCooldown,
    #[error("isolated profile not found: {agent}/{profile}")]
    IsolateNotFound { agent: String, profile: String },
    #[error("expected <agent>/<profile>")]
    InvalidTarget,
    #[error("cannot determine current directory: {0}")]
    NoCwd(#[source] std::io::Error),
}

#[derive(Error, Debug)]
pub enum DaemonError {
    #[error("taskkill: {0}")]
    TaskKill(#[from] std::io::Error),
}

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum UpdateError {
    #[error("HTTP request failed: {0}")]
    Http(String),
    #[error("latest version not found in release list")]
    NoLatest,
}

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum AliasError {
    #[error("unsupported shell: {0}")]
    UnsupportedShell(String),
}

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum AgentInstallError {
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    #[error("unsupported package manager: {0}")]
    UnsupportedPm(String),
    #[error("no package name for {0}")]
    NoPackageName(String),
    #[error("{0} — exit code {1}")]
    CommandFailed(String, i32),
    #[error("{0} — {1}")]
    CommandIo(String, #[source] std::io::Error),
}

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum CoachingError {
    #[error("invalid coaching rule: {0}")]
    InvalidRule(String),
    #[error("coaching rule not found: {0}")]
    NotFound(String),
}
