//! Shared dependency store for claimed worktrees (item #329).
//!
//! A worktree whose lockfile is byte-identical to one already installed gets
//! its `node_modules` materialised from `~/.agentflare/deps/<hash>-<os>-<arch>/`
//! instead of a fresh install: one hard link per file (zero extra bytes on
//! ext4), falling back to `std::fs::copy` per file (reflinks on btrfs/XFS,
//! and the cross-device case). A differing lockfile is never shared.
//!
//! Hard links share inodes, so in-place writes through a provisioned tree
//! reach every sharer; sandboxed jobs therefore overlay it (see
//! `flare-sandbox`), and `npm ci` (which recreates files) is safe. Unsandboxed
//! sessions are NOT protected: an in-place edit of a provisioned worktree's
//! `node_modules` reaches the store entry and every other worktree on it. The
//! store entry itself is seeded by a real copy, so edits to the main
//! checkout's own `node_modules` never leak into it.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// Lockfiles that fully determine an installed `node_modules`.
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];
const DIR: &str = "node_modules";
/// Dropped inside a provisioned `node_modules`; `clean` never offers such a
/// directory and the sandbox overlays it. An install that recreates the
/// directory removes it, which is correct: that copy is private again.
pub const MARKER: &str = ".agentflare-deps";
/// Project dirs searched below a worktree root (root, `a`, `a/b`).
const MAX_DEPTH: usize = 2;
/// The store is shared by every project on the machine, so an entry no
/// *visible* checkout references is only reclaimable once it has also sat
/// unused (no provisioning touched it) this long.
pub const STALE_AFTER: Duration = Duration::from_secs(14 * 24 * 3600);
/// A `<key>.tmp-<pid>` dir this old belongs to a crashed seeder.
const TMP_STALE_AFTER: Duration = Duration::from_secs(3600);

#[must_use]
pub fn store_root() -> PathBuf {
    agentflare_config::agentflare_dir().join("deps")
}

/// `<sha256 of the lockfile>-<os>-<arch>` for the project in `dir`.
#[must_use]
pub fn lockfile_key(dir: &Path) -> Option<String> {
    let lock = LOCKFILES
        .iter()
        .map(|l| dir.join(l))
        .find(|p| p.is_file())?;
    let bytes = std::fs::read(lock).ok()?;
    let hash: String = Sha256::digest(bytes)
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect();
    Some(format!(
        "{hash}-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

/// Directories under `root` (relative, `""` for root itself) holding a
/// `package.json` and a lockfile.
#[must_use]
pub fn project_dirs(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            e.depth() == 0 || !(n.starts_with('.') || n == DIR || n == "target")
        })
        .flatten()
        .filter(|e| {
            e.file_type().is_dir()
                && e.path().join("package.json").is_file()
                && lockfile_key(e.path()).is_some()
        })
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .collect()
}

/// Hard-links (else copies) the tree `src` to the new directory `dst`;
/// symlinks are recreated, never followed.
pub fn link_tree(src: &Path, dst: &Path) -> io::Result<()> {
    walk_tree(src, dst, true)
}

fn walk_tree(src: &Path, dst: &Path, hard: bool) -> io::Result<()> {
    for entry in walkdir::WalkDir::new(src).follow_links(false) {
        let entry = entry.map_err(io::Error::other)?;
        let target = dst.join(entry.path().strip_prefix(src).map_err(io::Error::other)?);
        let ft = entry.file_type();
        if ft.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &target)?;
            #[cfg(not(unix))]
            std::fs::copy(entry.path(), &target).map(drop)?;
        } else if !hard || std::fs::hard_link(entry.path(), &target).is_err() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Seeds `entry` from `src` atomically: build beside it, then rename, so a
/// concurrent seeder or reader never sees a partial tree.
fn seed(entry: &Path, src: &Path) -> io::Result<()> {
    let tmp = entry.with_extension(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    // A real copy, not links: main's live `node_modules` may be edited in
    // place later and must not reach the store.
    let built = walk_tree(src, &tmp.join(DIR), false);
    let done = built.and_then(|()| std::fs::rename(&tmp, entry));
    if done.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        // Lost the race to another seeder: the entry is there, which is fine.
        if entry.join(DIR).is_dir() {
            return Ok(());
        }
    }
    done
}

/// Provisions `node_modules` in each project of `worktree` that has none,
/// from the store entry for its lockfile (seeded from `main`'s install when
/// the lockfiles match). Returns one log line per decision. Best effort:
/// failures leave the project for a normal install.
pub fn provision(main: &Path, worktree: &Path) -> Vec<String> {
    provision_in(&store_root(), main, worktree)
}

pub fn provision_in(store: &Path, main: &Path, worktree: &Path) -> Vec<String> {
    let mut log = Vec::new();
    for rel in project_dirs(worktree) {
        let (wt_dir, main_dir) = (worktree.join(&rel), main.join(&rel));
        let shown = if rel.as_os_str().is_empty() {
            ".".to_string()
        } else {
            rel.display().to_string()
        };
        if wt_dir.join(DIR).symlink_metadata().is_ok() {
            continue;
        }
        let Some(key) = lockfile_key(&wt_dir) else {
            continue;
        };
        let entry = store.join(&key);
        if !entry.join(DIR).is_dir() {
            let main_nm = main_dir.join(DIR);
            if main != worktree
                && lockfile_key(&main_dir).as_deref() == Some(&key)
                && is_real_dir(&main_nm)
            {
                let _ = std::fs::create_dir_all(store);
                if let Err(e) = seed(&entry, &main_nm) {
                    log.push(format!("deps: {shown}: could not seed store: {e}"));
                    continue;
                }
            } else {
                log.push(format!(
                    "deps: {shown}: no shared install for this lockfile (the main checkout's \
                     lockfile differs or it has no node_modules); run a fresh install"
                ));
                continue;
            }
        }
        // Mark the entry as recently used so `unreferenced` leaves it be.
        if let Ok(f) = std::fs::File::open(&entry) {
            let _ = f.set_modified(SystemTime::now());
        }
        let dst = wt_dir.join(DIR);
        let linked = link_tree(&entry.join(DIR), &dst)
            .and_then(|()| std::fs::write(dst.join(MARKER), key.as_bytes()));
        match linked {
            Ok(()) => log.push(format!("deps: {shown}: node_modules shared from {key}")),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dst);
                log.push(format!(
                    "deps: {shown}: sharing failed ({e}); run a fresh install"
                ));
            }
        }
    }
    log
}

/// `true` for a `node_modules` that [`provision`] materialised.
#[must_use]
pub fn is_provisioned(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

/// Store keys still wanted by some checkout in `checkouts`.
#[must_use]
pub fn referenced_keys(checkouts: &[PathBuf]) -> HashSet<String> {
    checkouts
        .iter()
        .flat_map(|c| {
            project_dirs(c)
                .into_iter()
                .filter_map(|r| lockfile_key(&c.join(r)))
        })
        .collect()
}

/// Store entries in `store` no checkout references and untouched for at least
/// `min_age`, with their size. Half-built `<key>.tmp-<pid>` dirs are reclaimed
/// once older than [`TMP_STALE_AFTER`] (their seeder crashed).
#[must_use]
pub fn unreferenced(
    store: &Path,
    referenced: &HashSet<String>,
    min_age: Duration,
) -> Vec<(PathBuf, u64)> {
    let Ok(rd) = std::fs::read_dir(store) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let tmp = n.contains(".tmp-");
            let age = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            if tmp {
                age >= TMP_STALE_AFTER
            } else {
                !referenced.contains(&n) && age >= min_age
            }
        })
        .map(|e| (e.path(), crate::worktree::dir_size(&e.path())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn project(dir: &Path, lock: &str, with_nm: bool) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(dir.join("package-lock.json"), lock).unwrap();
        if with_nm {
            let pkg = dir.join("node_modules/a");
            std::fs::create_dir_all(&pkg).unwrap();
            std::fs::write(pkg.join("index.js"), "x").unwrap();
            std::os::unix::fs::symlink("a/index.js", dir.join("node_modules/link")).unwrap();
        }
    }

    #[test]
    fn matching_lockfile_shares_bytes_and_marks() {
        let t = tempfile::tempdir().unwrap();
        let (main, wt1, wt2, store) = (
            t.path().join("main"),
            t.path().join("wt1"),
            t.path().join("wt2"),
            t.path().join("store"),
        );
        project(&main, "L", true);
        project(&wt1, "L", false);
        project(&wt2, "L", false);
        let log = provision_in(&store, &main, &wt1);
        assert!(log[0].contains("shared"), "{log:?}");
        provision_in(&store, &main, &wt2);
        let f = |p: &Path| std::fs::metadata(p.join("node_modules/a/index.js")).unwrap();
        assert_eq!(f(&wt1).ino(), f(&wt2).ino());
        assert_ne!(f(&wt1).ino(), f(&main).ino(), "store is seeded by copy");
        assert!(f(&wt1).nlink() >= 3);
        assert!(is_provisioned(&wt1.join("node_modules")));
        assert!(!is_provisioned(&main.join("node_modules")));
        assert!(
            std::fs::symlink_metadata(wt1.join("node_modules/link"))
                .unwrap()
                .is_symlink()
        );
    }

    #[test]
    fn differing_lockfile_is_not_shared_and_says_why() {
        let t = tempfile::tempdir().unwrap();
        let (main, wt, store) = (t.path().join("m"), t.path().join("w"), t.path().join("s"));
        project(&main, "L1", true);
        project(&wt, "L2", false);
        let log = provision_in(&store, &main, &wt);
        assert!(log[0].contains("lockfile differs"), "{log:?}");
        assert!(!wt.join("node_modules").exists());
    }

    #[test]
    fn existing_node_modules_is_left_alone() {
        let t = tempfile::tempdir().unwrap();
        let (main, wt, store) = (t.path().join("m"), t.path().join("w"), t.path().join("s"));
        project(&main, "L", true);
        project(&wt, "L", true);
        assert!(provision_in(&store, &main, &wt).is_empty());
    }

    #[test]
    fn unreferenced_entries_are_found() {
        let t = tempfile::tempdir().unwrap();
        let (main, wt, store) = (t.path().join("m"), t.path().join("w"), t.path().join("s"));
        project(&main, "L", true);
        project(&wt, "L", false);
        provision_in(&store, &main, &wt);
        let used = referenced_keys(&[main.clone(), wt.clone()]);
        assert!(unreferenced(&store, &used, Duration::ZERO).is_empty());
        project(&wt, "changed", false);
        std::fs::remove_dir_all(&main).unwrap();
        let none = referenced_keys(&[wt]);
        assert_eq!(unreferenced(&store, &none, Duration::ZERO).len(), 1);
        // Fresh entries survive the age threshold (other projects may use them).
        assert!(unreferenced(&store, &none, STALE_AFTER).is_empty());
    }

    #[test]
    fn crashed_seeder_tmp_dirs_are_reaped_by_age() {
        let t = tempfile::tempdir().unwrap();
        let tmp = t.path().join("abc-linux-x86_64.tmp-99");
        std::fs::create_dir_all(tmp.join("node_modules")).unwrap();
        let none = HashSet::new();
        assert!(unreferenced(t.path(), &none, Duration::ZERO).is_empty());
        let old = SystemTime::now() - 2 * TMP_STALE_AFTER;
        std::fs::File::open(&tmp)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(unreferenced(t.path(), &none, Duration::ZERO).len(), 1);
    }

    #[test]
    fn main_edits_do_not_leak_into_store() {
        let t = tempfile::tempdir().unwrap();
        let (main, wt, store) = (t.path().join("m"), t.path().join("w"), t.path().join("s"));
        project(&main, "L", true);
        project(&wt, "L", false);
        provision_in(&store, &main, &wt);
        std::fs::write(main.join("node_modules/a/index.js"), "edited").unwrap();
        assert_eq!(
            std::fs::read(wt.join("node_modules/a/index.js")).unwrap(),
            b"x"
        );
    }
}
