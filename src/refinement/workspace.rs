use super::receipts::{ensure_plain, identity, now_ms};
use flare_git_core::{shell::command_in, snapshot::snapshot_worktree_before_with_runner};
use flare_workflow::refinement::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct GitControl {
    pub deadline_ms: u64,
    pub job: Option<String>,
}
impl Default for GitControl {
    fn default() -> Self {
        Self {
            deadline_ms: now_ms() + 60_000,
            job: None,
        }
    }
}
impl GitControl {
    fn check_active(&self) -> Result<Duration, RefinementError> {
        if self
            .job
            .as_deref()
            .is_some_and(agentflare_jobs::cancel::job_cancelled)
        {
            return Err(RefinementError::Cancelled);
        }
        let remaining = self.deadline_ms.saturating_sub(now_ms());
        if remaining == 0 {
            return Err(RefinementError::Exhausted("Git deadline".into()));
        }
        Ok(Duration::from_millis(remaining))
    }
    fn execute(&self, command: std::process::Command) -> Result<String, RefinementError> {
        let timeout = self.check_active()?;
        let output = crate::agent_launch::run_captured_for_job(
            command,
            timeout,
            timeout,
            None,
            self.job.as_deref(),
        )?;
        if output.cancelled {
            return Err(RefinementError::Cancelled);
        }
        if output.timed_out {
            return Err(RefinementError::Exhausted("Git deadline".into()));
        }
        if !output.success {
            return Err(RefinementError::Persistence(output.stderr));
        }
        Ok(output.stdout.trim().to_string())
    }
    fn git(&self, cwd: &Path, args: &[&str]) -> Result<String, RefinementError> {
        self.execute(command_in(cwd, args))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenInput {
    pub repository: PathBuf,
    pub snapshot: String,
}
impl FrozenInput {
    #[cfg(test)]
    pub fn capture(repository: &Path) -> Result<Self, RefinementError> {
        Self::capture_with_control(repository, &GitControl::default())
    }
    pub fn capture_with_control(
        repository: &Path,
        control: &GitControl,
    ) -> Result<Self, RefinementError> {
        let repository = dunce::canonicalize(repository)?;
        let snapshot = snapshot_worktree_before_with_runner(
            &repository,
            &repository,
            "Harness2 immutable task input",
            |command| control.execute(command).map_err(|e| e.to_string()),
        )
        .map_err(RefinementError::Persistence)?
        .0;
        Ok(Self {
            repository,
            snapshot,
        })
    }
}
pub struct TrialWorkspace {
    path: PathBuf,
    repository: PathBuf,
    root: PathBuf,
    control: GitControl,
}
impl TrialWorkspace {
    #[cfg(test)]
    pub fn create(
        input: &FrozenInput,
        root: &Path,
        id: &str,
        tree: &HarnessTree,
    ) -> Result<Self, RefinementError> {
        Self::create_with_control(input, root, id, tree, GitControl::default())
    }
    pub fn create_with_control(
        input: &FrozenInput,
        root: &Path,
        id: &str,
        tree: &HarnessTree,
        control: GitControl,
    ) -> Result<Self, RefinementError> {
        validate_tree(tree)?;
        let workspace = Self::snapshot_with_control(input, root, id, control)?;
        if let Err(e) = stage_tree(&workspace.path, tree) {
            let _ = workspace.cleanup();
            return Err(e);
        }
        Ok(workspace)
    }
    pub fn snapshot_with_control(
        input: &FrozenInput,
        root: &Path,
        id: &str,
        control: GitControl,
    ) -> Result<Self, RefinementError> {
        ensure_plain(root)?;
        std::fs::create_dir_all(root)?;
        let root = dunce::canonicalize(root)?;
        let path = root.join(format!("trial-{}", identity(id.as_bytes())));
        ensure_plain(&path)?;
        if path.exists() {
            return Err(RefinementError::Interrupted(format!(
                "workspace already exists: {id}"
            )));
        }
        control.git(
            &input.repository,
            &[
                "worktree",
                "add",
                "--detach",
                &path.to_string_lossy(),
                &input.snapshot,
            ],
        )?;
        let workspace = Self {
            path,
            repository: input.repository.clone(),
            root,
            control,
        };
        Ok(workspace)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn cleanup(self) -> Result<(), RefinementError> {
        ensure_plain(&self.path)?;
        if !dunce::canonicalize(&self.path)?.starts_with(&self.root) {
            return Err(RefinementError::Invalid(
                "workspace containment changed".into(),
            ));
        }
        self.control.git(
            &self.repository,
            &[
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ],
        )?;
        Ok(())
    }
}

pub fn stage_tree(root: &Path, tree: &HarnessTree) -> Result<(), RefinementError> {
    validate_tree(tree)?;
    ensure_plain(root)?;
    // Remove editable files from the frozen tree so a removed skill cannot remain active.
    for path in harness_paths(root)? {
        ensure_plain(&path)?;
        std::fs::remove_file(path)?;
    }
    for (relative, body) in tree {
        let path = root.join(relative);
        ensure_plain(&path)?;
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| RefinementError::Invalid(relative.clone()))?,
        )?;
        crate::atomic_fs::try_atomic_write(&path, body.as_bytes(), None)?;
    }
    Ok(())
}
fn harness_paths(root: &Path) -> Result<Vec<PathBuf>, RefinementError> {
    let mut out = vec![];
    let agents = root.join("AGENTS.md");
    if agents.try_exists()? {
        ensure_plain(&agents)?;
        out.push(agents);
    }
    let skills = root.join(".agents/skills");
    ensure_plain(&skills)?;
    if skills.try_exists()? {
        for entry in std::fs::read_dir(skills)? {
            let path = entry?.path();
            ensure_plain(&path)?;
            if path.is_dir() {
                let skill = path.join("SKILL.md");
                if skill.try_exists()? {
                    ensure_plain(&skill)?;
                    out.push(skill);
                }
            }
        }
    }
    Ok(out)
}
pub fn read_tree(root: &Path) -> Result<HarnessTree, RefinementError> {
    let mut tree = HarnessTree::new();
    for path in harness_paths(root)? {
        let relative = path
            .strip_prefix(root)
            .map_err(|e| RefinementError::Invalid(e.to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        let meta = std::fs::metadata(&path)?;
        if meta.len() > 256 * 1024 {
            return Err(RefinementError::Invalid("oversized harness file".into()));
        }
        tree.insert(relative, std::fs::read_to_string(path)?);
    }
    if !tree.contains_key("AGENTS.md") {
        tree.insert(
            "AGENTS.md".into(),
            "Complete the task carefully and verify the result.".into(),
        );
    }
    validate_tree(&tree)?;
    Ok(tree)
}
pub fn artifact_digest(root: &Path) -> Result<String, RefinementError> {
    artifact_digest_with_control(root, &GitControl::default())
}
pub fn artifact_digest_with_control(
    root: &Path,
    control: &GitControl,
) -> Result<String, RefinementError> {
    ensure_plain(root)?;
    let names = control.git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut names: Vec<_> = names.split('\0').filter(|n| !n.is_empty()).collect();
    names.sort_unstable();
    names.dedup();
    let mut hash = Sha256::new();
    for name in names {
        control.check_active()?;
        let path = root.join(name);
        ensure_plain(&path)?;
        hash.update((name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        if !path.try_exists()? {
            hash.update(b"absent");
            continue;
        }
        let mut file = std::fs::File::open(path)?;
        hash.update(file.metadata()?.len().to_le_bytes());
        hash_contents(&mut file, &mut hash, control)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn hash_contents(
    file: &mut impl Read,
    hash: &mut Sha256,
    control: &GitControl,
) -> Result<(), RefinementError> {
    let mut buf = [0u8; 65536];
    loop {
        control.check_active()?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        hash.update(&buf[..n]);
    }
}
#[cfg(test)]
pub fn public_artifacts(root: &Path, snapshot: &str) -> Result<String, RefinementError> {
    public_artifacts_with_control(root, snapshot, &GitControl::default())
}
pub fn public_artifacts_with_control(
    root: &Path,
    snapshot: &str,
    control: &GitControl,
) -> Result<String, RefinementError> {
    let diff = control.git(
        root,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            snapshot,
            "--",
        ],
    )?;
    let new = control.git(root, &["ls-files", "-z", "--others", "--exclude-standard"])?;
    let mut evidence = diff;
    for name in new.split('\0').filter(|n| !n.is_empty()) {
        control.check_active()?;
        let path = root.join(name);
        ensure_plain(&path)?;
        if std::fs::metadata(&path)?.len() <= 65536 {
            if let Ok(text) = std::fs::read_to_string(path) {
                evidence.push_str(&format!("\nFILE {name}\n{text}\n"));
            }
        } else {
            evidence.push_str(&format!("\nFILE {name}: exceeds text evidence limit\n"));
        }
        if evidence.len() > 262144 {
            break;
        }
    }
    if evidence.len() > 262144 {
        let mut end = 262144;
        while !evidence.is_char_boundary(end) {
            end -= 1;
        }
        evidence.truncate(end);
        evidence.push_str("\n[evidence truncated; complete artifacts retained on disk]");
    }
    Ok(evidence)
}
