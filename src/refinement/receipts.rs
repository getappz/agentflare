//! Durable invocation reservations and receipts for refinement runs.
use flare_workflow::refinement::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn identity(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>, RefinementError> {
    serde_json::to_vec(value).map_err(|e| RefinementError::Persistence(e.to_string()))
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn ensure_plain(path: &Path) -> Result<(), RefinementError> {
    for part in path.ancestors() {
        match std::fs::symlink_metadata(part) {
            Ok(meta) => {
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let linked = meta.file_type().is_symlink();
                if linked {
                    return Err(RefinementError::Invalid(format!(
                        "linked path: {}",
                        part.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), RefinementError> {
    ensure_plain(path)?;
    crate::atomic_fs::try_atomic_write(path, &json_bytes(value)?, None)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallReservation {
    pub id: String,
    pub fingerprint: String,
    pub allowance_usd: Option<f64>,
    pub deadline_ms: u64,
}
pub enum Reservation {
    Fresh(CallReservation),
    Cached(Receipt),
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BudgetSummary {
    pub calls: usize,
    pub charged_usd: Option<f64>,
    pub known_usd: f64,
    pub unknown_cost_calls: usize,
    pub deadline_ms: u64,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    reservation: CallReservation,
    receipt: Option<Receipt>,
    receipt_hash: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct LedgerState {
    version: u32,
    identity: String,
    limits: Limits,
    deadline_ms: u64,
    entries: BTreeMap<String, Entry>,
}

pub struct RunLedger {
    root: PathBuf,
    identity: String,
    limits: Limits,
}
impl RunLedger {
    pub fn open(root: &Path, identity: &str, limits: &Limits) -> Result<Self, RefinementError> {
        ensure_plain(root)?;
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        let ledger = Self {
            root: dunce::canonicalize(root)?,
            identity: identity.into(),
            limits: limits.clone(),
        };
        ledger.with_state(|_| Ok(()))?;
        Ok(ledger)
    }
    fn with_state<T>(
        &self,
        f: impl FnOnce(&mut LedgerState) -> Result<T, RefinementError>,
    ) -> Result<T, RefinementError> {
        let path = self.root.join("ledger.json");
        let lock_path = self.root.join("ledger.lock");
        ensure_plain(&lock_path)?;
        ensure_plain(&path)?;
        let lock: File = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let mut state = if path.exists() {
            serde_json::from_slice::<LedgerState>(&std::fs::read(&path)?)
                .map_err(|e| RefinementError::Persistence(e.to_string()))?
        } else {
            LedgerState {
                version: 1,
                identity: self.identity.clone(),
                limits: self.limits.clone(),
                deadline_ms: now_ms()
                    .saturating_add(self.limits.total_timeout_secs.saturating_mul(1000)),
                entries: BTreeMap::new(),
            }
        };
        if state.version != 1
            || state.identity != self.identity
            || json_bytes(&state.limits)? != json_bytes(&self.limits)?
        {
            return Err(RefinementError::Changed("request or ledger limits".into()));
        }
        let result = f(&mut state)?;
        write_json(&path, &state)?;
        Ok(result)
    }
    fn charges(state: &LedgerState) -> BudgetSummary {
        let mut summary = BudgetSummary {
            calls: state.entries.len(),
            deadline_ms: state.deadline_ms,
            ..Default::default()
        };
        let mut charged = 0.0;
        for entry in state.entries.values() {
            match entry.receipt.as_ref().and_then(|r| r.cost_usd) {
                Some(cost) => {
                    summary.known_usd += cost;
                    charged += cost;
                }
                None => {
                    summary.unknown_cost_calls += 1;
                    charged += entry.reservation.allowance_usd.unwrap_or(0.0);
                }
            }
        }
        summary.charged_usd =
            if state.limits.max_cost_usd.is_some() || summary.unknown_cost_calls == 0 {
                Some(charged)
            } else {
                None
            };
        summary
    }
    fn verify_entry(&self, entry: &Entry) -> Result<(), RefinementError> {
        if let Some(receipt) = &entry.receipt {
            if entry.receipt_hash.as_deref() != Some(identity(&json_bytes(receipt)?).as_str()) {
                return Err(RefinementError::Changed("receipt bytes".into()));
            }
            if let Some(path) = &receipt.artifact_path {
                ensure_plain(path)?;
                if !dunce::canonicalize(path)?.starts_with(&self.root)
                    || receipt.artifact_identity.as_deref()
                        != Some(super::workspace::artifact_digest(path)?.as_str())
                {
                    return Err(RefinementError::Changed(format!(
                        "trial artifacts: {}",
                        receipt.id
                    )));
                }
            }
        }
        Ok(())
    }
    pub fn reserve(&self, inv: &Invocation) -> Result<Reservation, RefinementError> {
        let fingerprint = identity(&json_bytes(inv)?);
        self.with_state(|state| {
            if inv.identity != self.identity {
                return Err(RefinementError::Changed("invocation identity".into()));
            }
            if let Some(entry) = state.entries.get(&inv.id) {
                if entry.reservation.fingerprint != fingerprint {
                    return Err(RefinementError::Changed(inv.id.clone()));
                }
                self.verify_entry(entry)?;
                return entry
                    .receipt
                    .clone()
                    .map(Reservation::Cached)
                    .ok_or_else(|| RefinementError::Interrupted(inv.id.clone()));
            }
            if now_ms() >= state.deadline_ms
                || state.entries.len() >= state.limits.max_calls as usize
            {
                return Err(RefinementError::Exhausted("calls or deadline".into()));
            }
            let allowance = if let Some(total) = state.limits.max_cost_usd {
                let remaining = total - Self::charges(state).charged_usd.unwrap_or(total);
                if remaining <= 0.0 {
                    return Err(RefinementError::Exhausted("dollars".into()));
                }
                Some(remaining / (state.limits.max_calls as usize - state.entries.len()) as f64)
            } else {
                None
            };
            let r = CallReservation {
                id: inv.id.clone(),
                fingerprint,
                allowance_usd: allowance,
                deadline_ms: state.deadline_ms,
            };
            state.entries.insert(
                inv.id.clone(),
                Entry {
                    reservation: r.clone(),
                    receipt: None,
                    receipt_hash: None,
                },
            );
            Ok(Reservation::Fresh(r))
        })
    }
    pub fn complete(
        &self,
        reservation: &CallReservation,
        receipt: &Receipt,
    ) -> Result<(), RefinementError> {
        if receipt.id != reservation.id
            || receipt.cost_usd.is_some_and(|c| !c.is_finite() || c < 0.0)
        {
            return Err(RefinementError::Invalid("invalid usage receipt".into()));
        }
        self.with_state(|state| {
            let entry = state
                .entries
                .get_mut(&reservation.id)
                .ok_or_else(|| RefinementError::Persistence("missing reservation".into()))?;
            if entry.reservation.fingerprint != reservation.fingerprint {
                return Err(RefinementError::Changed(reservation.id.clone()));
            }
            if let Some(existing) = &entry.receipt {
                if json_bytes(existing)? != json_bytes(receipt)? {
                    return Err(RefinementError::Changed("completed receipt".into()));
                }
            } else {
                entry.receipt_hash = Some(identity(&json_bytes(receipt)?));
                entry.receipt = Some(receipt.clone());
            }
            Ok(())
        })
    }
    pub fn summary(&self) -> Result<BudgetSummary, RefinementError> {
        self.with_state(|s| Ok(Self::charges(s)))
    }
    pub fn verify(&self) -> Result<(), RefinementError> {
        self.with_state(|s| {
            for e in s.entries.values() {
                self.verify_entry(e)?;
            }
            Ok(())
        })
    }
}
