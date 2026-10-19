//! Explicit, evidence-linked promotion of project observations.
use super::{observations, relations};
use serde::Deserialize;
use serde_json::{Value, json};

/// Observation type reserved for promoted project lessons.
pub(super) const LESSON_TYPE: &str = "lesson";
/// Relation from a supporting observation to the lesson it justifies.
const EVIDENCE_RELATION: &str = "evidence_for";

#[derive(Debug, Deserialize)]
pub struct PromoteInput {
    pub title: String,
    pub content: String,
    pub project: Option<String>,
    pub evidence_ids: Vec<i64>,
    pub confidence: Option<f64>,
    pub session_id: Option<String>,
    pub topic_key: Option<String>,
    pub run_id: String,
}

fn run_receipt(report: &Value, run_id: &str) -> Result<Value, String> {
    if report["run"]["run_id"].as_str() != Some(run_id) {
        return Err("run_id must identify the stored evidence report".into());
    }
    if !matches!(
        report["run"]["status"].as_str(),
        Some("completed" | "failed" | "cancelled")
    ) {
        return Err("promotion requires a terminal run with recorded execution evidence".into());
    }
    let entry = report["journal_evidence"]
        .as_array()
        .and_then(|entries| {
            entries.iter().find(|entry| {
                matches!(
                    entry["execution_outcome"].as_str(),
                    Some("succeeded" | "failed")
                )
            })
        })
        .ok_or("promotion requires a recorded run/check outcome, not an agent claim")?;
    Ok(json!({"run_id":run_id,"run_status":report["run"]["status"],
        "journal_index":entry["journal_index"],"execution_outcome":entry["execution_outcome"]}))
}

/// Promote a project lesson: saved as a project-scoped `lesson` observation
/// and linked by an `evidence_for` relation (carrying `confidence`) from each
/// supporting observation. Evidence must be live, project-scoped and in the
/// same project. A lesson's effective confidence is the minimum confidence over
/// its immutable `evidence_for` receipts. Execution provenance does not verify
/// the lesson's semantic correctness; confidence is an explicit estimate.
pub(super) fn promote_with_conn(
    conn: &rusqlite::Connection,
    input: PromoteInput,
    report: &Value,
) -> Result<String, String> {
    if input.title.trim().is_empty() || input.content.trim().is_empty() {
        return Err("title and content are required".into());
    }
    let project = input
        .project
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or("project is required: lessons are project-scoped")?;
    let confidence = input
        .confidence
        .ok_or("confidence is required (0.0..1.0)")?;
    if !(confidence > 0.0 && confidence <= 1.0) {
        return Err("confidence must be in (0.0, 1.0]".into());
    }
    let evidence: Vec<i64> = input
        .evidence_ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if evidence.is_empty() {
        return Err(
            "evidence_ids is required: a lesson needs at least one supporting observation".into(),
        );
    }
    let receipt = run_receipt(report, &input.run_id)?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("begin transaction: {e}"))?;
    let mut sources = Vec::new();
    for id in &evidence {
        let obs = observations::get(&tx, *id)
            .map_err(|e| format!("evidence lookup: {e}"))?
            .filter(|o| o.deleted_at.is_none())
            .ok_or_else(|| format!("evidence observation {id} not found"))?;
        if obs.scope != "project" {
            return Err(format!(
                "evidence observation {id} has scope '{}'; only project-scoped evidence can back a lesson",
                obs.scope
            ));
        }
        if obs.project.as_deref().map(str::to_lowercase) != Some(project.to_lowercase()) {
            return Err(format!(
                "evidence observation {id} belongs to a different project"
            ));
        }
        sources.push(obs);
    }

    let outcome = observations::save_lesson(
        &tx,
        input.session_id.as_deref(),
        &input.title,
        &input.content,
        project,
        input.topic_key.as_deref(),
    )
    .map_err(|e| format!("save failed: {e}"))?;
    let (status, id) = match outcome {
        observations::SaveOutcome::Created(id) => ("promoted", id),
        observations::SaveOutcome::Updated(id) => ("updated", id),
        observations::SaveOutcome::Duplicate(id) => ("duplicate", id),
    };
    // save() dedups by content hash and topic_key across types; never attach
    // evidence to (or rewrite) an observation that is not already a lesson.
    let saved = observations::get(&tx, id)
        .map_err(|e| format!("lesson lookup: {e}"))?
        .ok_or("lesson vanished after save")?;
    if saved.r#type != LESSON_TYPE {
        return Err(format!(
            "an existing '{}' observation (id {id}) has the same content or topic_key; refusing to promote over it",
            saved.r#type
        ));
    }
    for source in sources {
        if source.id == id {
            return Err("a lesson cannot be evidence for itself".into());
        }
        let mut evidence = receipt.clone();
        evidence["observation_hash"] = json!(observations::hash_normalized(
            &source.title,
            &source.content
        ));
        evidence["observation_revision"] = json!(source.revision_count);
        relations::create_promotion_evidence(
            &tx,
            source.id,
            id,
            input.session_id.as_deref(),
            &evidence.to_string(),
            confidence,
        )
        .map_err(|e| format!("create relation: {e}"))?;
    }
    let effective: f64 = tx
        .query_row(
            "SELECT MIN(confidence) FROM memory_relations WHERE target_id = ?1 AND relation = ?2",
            rusqlite::params![id, EVIDENCE_RELATION],
            |r| r.get(0),
        )
        .map_err(|e| format!("confidence lookup: {e}"))?;
    tx.commit().map_err(|e| format!("commit promote: {e}"))?;
    if status != "duplicate" {
        super::mcp::index_embedding(conn, id, &input.title, &input.content, status == "updated");
    }
    Ok(
        json!({"status": status, "id": id, "evidence_ids": evidence, "confidence": effective, "run_id":input.run_id})
            .to_string(),
    )
}

pub(super) fn observation_view(
    conn: &rusqlite::Connection,
    obs: &observations::Observation,
) -> rusqlite::Result<Value> {
    let mut value = json!(obs);
    if !observations::is_lesson(&obs.r#type) {
        return Ok(value);
    }
    let mut stmt = conn.prepare("SELECT source_id, confidence, evidence FROM memory_relations WHERE target_id=?1 AND relation='evidence_for' ORDER BY id")?;
    let rows = stmt.query_map([obs.id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<f64>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut evidence = Vec::new();
    let mut confidence: Option<f64> = None;
    for row in rows {
        let (source, score, receipt) = row?;
        let receipt = receipt.and_then(|text| serde_json::from_str::<Value>(&text).ok());
        if let (Some(score), Some(receipt)) = (
            score.filter(|s| s.is_finite() && *s > 0.0 && *s <= 1.0),
            receipt,
        ) {
            confidence = Some(confidence.map_or(score, |old| old.min(score)));
            evidence.push(json!({"observation_id":source,"confidence":score,"receipt":receipt}));
        }
    }
    value["promotion"] =
        json!({"confidence":confidence,"confidence_kind":"explicit estimate", "evidence":evidence});
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::super::mcp::{CurateInput, RememberInput, handle_curate, handle_remember};
    use super::*;
    use rusqlite::Connection;

    fn new_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        super::super::schema::migrate(&mut conn).unwrap();
        conn
    }

    fn report() -> Value {
        json!({"run":{"run_id":"fixture-run","status":"completed"},
            "journal_evidence":[{"journal_index":2,"execution_outcome":"succeeded"}]})
    }

    fn promote_with_conn(conn: &Connection, input: PromoteInput) -> Result<String, String> {
        super::promote_with_conn(conn, input, &report())
    }

    fn obs(conn: &Connection, title: &str, project: &str) -> i64 {
        match observations::save(
            conn,
            None,
            "discovery",
            title,
            "body",
            None,
            Some(project),
            None,
            None,
        )
        .unwrap()
        {
            observations::SaveOutcome::Created(id) => id,
            o => panic!("{o:?}"),
        }
    }

    fn promote_input(ev: Vec<i64>, conf: Option<f64>) -> PromoteInput {
        PromoteInput {
            title: "Always X".into(),
            content: "because Y".into(),
            project: Some("p".into()),
            evidence_ids: ev,
            confidence: conf,
            session_id: None,
            topic_key: None,
            run_id: "fixture-run".into(),
        }
    }

    #[test]
    fn promotion_requires_a_recorded_terminal_run_and_preserves_receipts() {
        let conn = new_db();
        let source = obs(&conn, "source", "p");
        for invalid in [
            json!({}),
            json!({"run":{"run_id":"other","status":"completed"}}),
            json!({"run":{"run_id":"fixture-run","status":"running"}}),
            json!({"run":{"run_id":"fixture-run","status":"completed"},"agent_claims":{"summary":"tests passed"}}),
        ] {
            assert!(
                super::promote_with_conn(&conn, promote_input(vec![source], Some(0.7)), &invalid)
                    .is_err()
            );
        }
        assert_eq!(lesson_count(&conn), 0);
        let out: Value = serde_json::from_str(
            &promote_with_conn(&conn, promote_input(vec![source], Some(0.7))).unwrap(),
        )
        .unwrap();
        let id = out["id"].as_i64().unwrap();
        assert!(promote_with_conn(&conn, promote_input(vec![source], Some(0.9))).is_err());
        let lesson = observations::get(&conn, id).unwrap().unwrap();
        let view = observation_view(&conn, &lesson).unwrap();
        assert_eq!(view["promotion"]["confidence"], 0.7);
        assert_eq!(
            view["promotion"]["evidence"][0]["receipt"]["run_id"],
            "fixture-run"
        );
        assert_eq!(
            view["promotion"]["evidence"][0]["receipt"]["journal_index"],
            2
        );
        assert_eq!(
            super::super::sync::export_all(&conn).unwrap().len(),
            1,
            "sync must not copy unsigned lessons"
        );
        assert!(
            relations::create(
                &conn,
                source,
                id,
                "evidence_for",
                None,
                None,
                None,
                Some(1.0)
            )
            .is_err()
        );
    }

    #[test]
    fn recall_distinguishes_raw_observations_and_returns_promotion_provenance() {
        use super::super::mcp::{RecallInput, recall_with_conn};
        let conn = new_db();
        let source = obs(&conn, "source", "p");
        let recall = |id, project: &str, kind: Option<&str>| {
            serde_json::from_str::<Value>(
                &recall_with_conn(
                    &conn,
                    RecallInput {
                        query: None,
                        id,
                        r#type: kind.map(str::to_owned),
                        project: Some(project.into()),
                        limit: None,
                    },
                )
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(recall(None, "p", Some("lesson")), json!([]));
        let out: Value = serde_json::from_str(
            &promote_with_conn(&conn, promote_input(vec![source], Some(0.7))).unwrap(),
        )
        .unwrap();
        let lesson = out["id"].as_i64().unwrap();
        assert!(recall(Some(source), "p", None).get("promotion").is_none());
        let lessons = recall(None, "p", Some("lesson"));
        assert_eq!(lessons[0]["promotion"]["confidence"], 0.7);
        assert_eq!(
            lessons[0]["promotion"]["evidence"][0]["observation_id"],
            source
        );
        assert_eq!(recall(Some(lesson), "q", None), Value::Null);
        #[cfg(not(feature = "semantic"))]
        assert!(
            super::super::embeddings::missing(&conn, 10)
                .unwrap()
                .iter()
                .any(|(id, _)| *id == lesson),
            "unindexed lessons remain eligible for existing backfill"
        );
    }

    #[test]
    fn promote_links_evidence_and_requires_it() {
        let conn = new_db();
        let a = obs(&conn, "a", "p");
        let b = obs(&conn, "b", "p");
        assert!(promote_with_conn(&conn, promote_input(vec![], Some(0.8))).is_err());
        assert!(promote_with_conn(&conn, promote_input(vec![a], None)).is_err());
        assert!(promote_with_conn(&conn, promote_input(vec![a], Some(1.5))).is_err());
        assert!(promote_with_conn(&conn, promote_input(vec![a, 9999], Some(0.8))).is_err());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE type='lesson'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "failed promotions must not leave a lesson behind");

        let out = promote_with_conn(&conn, promote_input(vec![a, b, a], Some(0.8))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "promoted");
        let id = v["id"].as_i64().unwrap();
        let rels: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_relations WHERE target_id=?1 AND relation='evidence_for' AND confidence=0.8",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rels, 2);
    }

    #[test]
    fn promote_rejects_cross_project_evidence() {
        let conn = new_db();
        let other = obs(&conn, "other", "q");
        assert!(promote_with_conn(&conn, promote_input(vec![other], Some(0.5))).is_err());
    }

    #[test]
    fn remember_rejects_reserved_lesson_type() {
        let err = handle_remember(RememberInput {
            title: "t".into(),
            content: "c".into(),
            r#type: "lesson".into(),
            session_id: None,
            project: None,
            topic_key: None,
            scope: None,
        })
        .unwrap_err();
        assert!(err.contains("promote"));
    }

    fn scoped_obs(conn: &Connection, title: &str, scope: &str) -> i64 {
        match observations::save(
            conn,
            None,
            "discovery",
            title,
            "body",
            None,
            Some("p"),
            Some(scope),
            None,
        )
        .unwrap()
        {
            observations::SaveOutcome::Created(id) => id,
            o => panic!("{o:?}"),
        }
    }

    fn lesson_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM observations WHERE type='lesson'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn promote_rejects_personal_scope_and_soft_deleted_evidence() {
        let conn = new_db();
        let personal = scoped_obs(&conn, "mine", "personal");
        assert!(promote_with_conn(&conn, promote_input(vec![personal], Some(0.5))).is_err());
        let gone = obs(&conn, "gone", "p");
        observations::soft_delete(&conn, gone).unwrap();
        assert!(promote_with_conn(&conn, promote_input(vec![gone], Some(0.5))).is_err());
        assert_eq!(lesson_count(&conn), 0);
    }

    #[test]
    fn promote_refuses_to_collide_with_non_lesson_and_rolls_back() {
        let conn = new_db();
        let a = obs(&conn, "a", "p");
        // Same title+content hash as an existing non-lesson observation.
        let existing = match observations::save(
            &conn,
            None,
            "decision",
            "Always X",
            "because Y",
            None,
            Some("p"),
            None,
            Some("tk"),
        )
        .unwrap()
        {
            observations::SaveOutcome::Created(id) => id,
            o => panic!("{o:?}"),
        };
        let err = promote_with_conn(&conn, promote_input(vec![a], Some(0.7))).unwrap_err();
        assert!(err.contains("decision"));
        let rels: i64 = conn
            .query_row("SELECT COUNT(*) FROM memory_relations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rels, 0);
        let dup: i64 = conn
            .query_row(
                "SELECT duplicate_count FROM observations WHERE id=?1",
                [existing],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dup, 0, "rolled back: no duplicate bump");

        // topic_key collision must not rewrite the non-lesson row either.
        let mut input = promote_input(vec![a], Some(0.7));
        input.title = "Different".into();
        input.content = "text".into();
        input.topic_key = Some("tk".into());
        assert!(promote_with_conn(&conn, input).is_err());
        let title: String = conn
            .query_row(
                "SELECT title FROM observations WHERE id=?1",
                [existing],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Always X");
    }

    #[test]
    fn promote_duplicate_and_topic_update_and_self_evidence() {
        let conn = new_db();
        let a = obs(&conn, "a", "p");
        let first = promote_with_conn(&conn, promote_input(vec![a], Some(0.9))).unwrap();
        let lesson: i64 = serde_json::from_str::<serde_json::Value>(&first).unwrap()["id"]
            .as_i64()
            .unwrap();

        // Same content again -> duplicate of the same lesson; lower confidence
        // on new evidence lowers the effective (minimum) confidence.
        let b = obs(&conn, "b", "p");
        let again = promote_with_conn(&conn, promote_input(vec![b], Some(0.4))).unwrap();
        let v: serde_json::Value = serde_json::from_str(&again).unwrap();
        assert_eq!(v["status"], "duplicate");
        assert_eq!(v["id"].as_i64().unwrap(), lesson);
        assert_eq!(v["confidence"].as_f64().unwrap(), 0.4);

        // The lesson cannot be its own evidence.
        assert!(promote_with_conn(&conn, promote_input(vec![lesson], Some(0.9))).is_err());

        // Revisions need a new explicit lesson; old receipts must stay intact.
        let mut t1 = promote_input(vec![a], Some(0.8));
        t1.title = "T".into();
        t1.content = "v1".into();
        t1.topic_key = Some("topic".into());
        let id1 = serde_json::from_str::<serde_json::Value>(&promote_with_conn(&conn, t1).unwrap())
            .unwrap()["id"]
            .as_i64()
            .unwrap();
        let mut t2 = promote_input(vec![a], Some(0.8));
        t2.title = "T".into();
        t2.content = "v2".into();
        t2.topic_key = Some("topic".into());
        assert!(promote_with_conn(&conn, t2).is_err());
        assert_eq!(
            observations::get(&conn, id1).unwrap().unwrap().content,
            "v1"
        );
    }

    #[test]
    fn curate_rejects_retyping_to_lesson() {
        let err = handle_curate(CurateInput {
            action: "update".into(),
            id: 1,
            title: None,
            content: None,
            r#type: Some("Lesson".into()),
            pinned: None,
        })
        .unwrap_err();
        assert!(err.contains("promote"));
    }
}
