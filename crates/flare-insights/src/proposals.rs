//! Proposals ledger: durable accept/reject/snooze decisions for reflect candidates (T12).

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::evidence::EvidenceRef;
use crate::store::{InsightsStore, StoreError};

/// Evidence kept per proposal; copied snapshots, so pruning sessions never orphans a decision.
const MAX_EVIDENCE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Proposed,
    Accepted,
    Rejected,
    Snoozed,
    Applied,
    Superseded,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Snoozed => "snoozed",
            Self::Applied => "applied",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "proposed" => Self::Proposed,
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "snoozed" => Self::Snoozed,
            "applied" => Self::Applied,
            "superseded" => Self::Superseded,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRow {
    pub id: i64,
    pub fingerprint: String,
    pub kind: String,
    pub project: String,
    pub title: String,
    pub proposed_content: String,
    pub target_path: Option<String>,
    pub status: ProposalStatus,
    pub session_count: i64,
    pub occurrence_count: i64,
    pub evidence: Vec<EvidenceRef>,
    pub model_context: Option<String>,
    pub applied_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub snoozed_until: Option<DateTime<Utc>>,
}

/// Stable identity of a candidate: sha256 hex over kind, project and normalised key.
pub fn fingerprint(kind: &str, project: &str, normalized_key: &str) -> String {
    let mut h = Sha256::new();
    for part in [kind, project, normalized_key] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

const COLS: &str = "id, fingerprint, kind, project, title, proposed_content, target_path, status, \
    session_count, occurrence_count, evidence_json, model_context, applied_hash, created_at, \
    updated_at, snoozed_until";

fn ts(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_default()
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<ProposalRow> {
    let status: String = r.get(7)?;
    let ev: String = r.get(10)?;
    Ok(ProposalRow {
        id: r.get(0)?,
        fingerprint: r.get(1)?,
        kind: r.get(2)?,
        project: r.get(3)?,
        title: r.get(4)?,
        proposed_content: r.get(5)?,
        target_path: r.get(6)?,
        status: ProposalStatus::parse(&status).unwrap_or(ProposalStatus::Proposed),
        session_count: r.get(8)?,
        occurrence_count: r.get(9)?,
        evidence: serde_json::from_str(&ev).unwrap_or_default(),
        model_context: r.get(11)?,
        applied_hash: r.get(12)?,
        created_at: ts(r.get(13)?),
        updated_at: ts(r.get(14)?),
        snoozed_until: r.get::<_, Option<String>>(15)?.map(ts),
    })
}

impl InsightsStore {
    /// Insert a candidate or fold it into the existing row. A Rejected row, or a Snoozed one
    /// still inside its snooze window, only bumps counts and evidence; its status is returned
    /// unchanged. Any other existing row also refreshes title/content.
    pub fn upsert_proposal_candidate(&self, p: &ProposalRow) -> Result<ProposalStatus, StoreError> {
        let now = Utc::now();
        let Some(old) = self.get_proposal_by_fingerprint(&p.fingerprint)? else {
            let ev: Vec<_> = p.evidence.iter().take(MAX_EVIDENCE).collect();
            self.conn.execute(
                "INSERT INTO insights_proposals(fingerprint, kind, project, title, proposed_content,
                 target_path, status, session_count, occurrence_count, evidence_json, model_context,
                 created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,'proposed',?7,?8,?9,?10,?11,?11)",
                params![
                    p.fingerprint,
                    p.kind,
                    p.project,
                    p.title,
                    p.proposed_content,
                    p.target_path,
                    p.session_count,
                    p.occurrence_count,
                    serde_json::to_string(&ev)?,
                    p.model_context,
                    now.to_rfc3339()
                ],
            )?;
            return Ok(ProposalStatus::Proposed);
        };
        let mut evidence = old.evidence.clone();
        for e in &p.evidence {
            let dup = evidence
                .iter()
                .any(|x| x.session_id == e.session_id && x.turn_seq == e.turn_seq);
            if !dup && evidence.len() < MAX_EVIDENCE {
                evidence.push(e.clone());
            }
        }
        let frozen = old.status == ProposalStatus::Rejected
            || (old.status == ProposalStatus::Snoozed
                && old.snoozed_until.is_some_and(|u| u > now));
        let (title, content) = if frozen {
            (&old.title, &old.proposed_content)
        } else {
            (&p.title, &p.proposed_content)
        };
        // An expired snooze resurfaces the proposal.
        let status = if old.status == ProposalStatus::Snoozed && !frozen {
            ProposalStatus::Proposed
        } else {
            old.status
        };
        self.conn.execute(
            "UPDATE insights_proposals SET title=?2, proposed_content=?3, session_count=?4,
             occurrence_count=?5, evidence_json=?6, updated_at=?7, status=?8 WHERE id=?1",
            params![
                old.id,
                title,
                content,
                old.session_count.max(p.session_count),
                old.occurrence_count.max(p.occurrence_count),
                serde_json::to_string(&evidence)?,
                now.to_rfc3339(),
                status.as_str()
            ],
        )?;
        Ok(status)
    }

    pub fn get_proposal(&self, id: i64) -> Result<Option<ProposalRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLS} FROM insights_proposals WHERE id=?1"),
                [id],
                from_row,
            )
            .optional()?)
    }

    pub fn get_proposal_by_fingerprint(&self, fp: &str) -> Result<Option<ProposalRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLS} FROM insights_proposals WHERE fingerprint=?1"),
                [fp],
                from_row,
            )
            .optional()?)
    }

    /// Returns false when no proposal has this id.
    pub fn set_proposal_status(
        &self,
        id: i64,
        status: ProposalStatus,
        snoozed_until: Option<DateTime<Utc>>,
    ) -> Result<bool, StoreError> {
        let n = self.conn.execute(
            "UPDATE insights_proposals SET status=?2, snoozed_until=?3, updated_at=?4 WHERE id=?1",
            params![
                id,
                status.as_str(),
                snoozed_until.map(|d| d.to_rfc3339()),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(n > 0)
    }

    pub fn list_proposals(
        &self,
        status: Option<ProposalStatus>,
    ) -> Result<Vec<ProposalRow>, StoreError> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {COLS} FROM insights_proposals WHERE (?1 IS NULL OR status=?1) ORDER BY id"
        ))?;
        let rows = st.query_map([status.map(ProposalStatus::as_str)], from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn cand(key: &str) -> ProposalRow {
        let now = Utc::now();
        ProposalRow {
            id: 0,
            fingerprint: fingerprint("memory_fact", "demo", key),
            kind: "memory_fact".into(),
            project: "demo".into(),
            title: "t".into(),
            proposed_content: "c".into(),
            target_path: None,
            status: ProposalStatus::Proposed,
            session_count: 2,
            occurrence_count: 3,
            evidence: vec![],
            model_context: None,
            applied_hash: None,
            created_at: now,
            updated_at: now,
            snoozed_until: None,
        }
    }

    #[test]
    fn rejected_fingerprint_stays_rejected_after_rescan() {
        let s = InsightsStore::open_in_memory().unwrap();
        let c = cand("k");
        assert_eq!(
            s.upsert_proposal_candidate(&c).unwrap(),
            ProposalStatus::Proposed
        );
        let id = s.list_proposals(None).unwrap()[0].id;
        s.set_proposal_status(id, ProposalStatus::Rejected, None)
            .unwrap();
        let mut again = cand("k");
        again.occurrence_count = 9;
        assert_eq!(
            s.upsert_proposal_candidate(&again).unwrap(),
            ProposalStatus::Rejected
        );
        let row = s.get_proposal(id).unwrap().unwrap();
        assert_eq!(row.status, ProposalStatus::Rejected);
        assert_eq!(row.occurrence_count, 9);
        assert_eq!(s.list_proposals(None).unwrap().len(), 1);
    }

    #[test]
    fn snooze_expires() {
        let s = InsightsStore::open_in_memory().unwrap();
        s.upsert_proposal_candidate(&cand("k")).unwrap();
        let id = s.list_proposals(None).unwrap()[0].id;
        let live = Some(Utc::now() + Duration::days(1));
        s.set_proposal_status(id, ProposalStatus::Snoozed, live)
            .unwrap();
        assert_eq!(
            s.upsert_proposal_candidate(&cand("k")).unwrap(),
            ProposalStatus::Snoozed
        );
        let past = Some(Utc::now() - Duration::days(1));
        s.set_proposal_status(id, ProposalStatus::Snoozed, past)
            .unwrap();
        let mut fresh = cand("k");
        fresh.title = "new".into();
        assert_eq!(
            s.upsert_proposal_candidate(&fresh).unwrap(),
            ProposalStatus::Proposed
        );
        assert_eq!(s.get_proposal(id).unwrap().unwrap().title, "new");
    }
}
