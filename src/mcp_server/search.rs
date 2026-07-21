use super::*;
use flare_search_kit::fts_query;

impl AgentflareMcp {
    pub async fn search_impl(&self, req: SearchRequest) -> Result<String, ErrorData> {
        let search_type = req.r#type.as_deref().unwrap_or("store");
        match search_type {
            "code" => self.search_code(&req).await,
            "memory" => self.search_memory(&req),
            "web" => self.search_web(&req).await,
            "store" => self.search_store(&req),
            other => Err(ErrorData::invalid_params(
                format!("unknown type '{other}' — use store|memory|code|web"),
                None,
            )),
        }
    }

    fn search_store(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(20);

        let ws_id = match self.with_backend_db(Self::resolve_workspace_id) {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => return Err(ErrorData::internal_error(e.to_string(), None)),
            Err(e) => return Err(e),
        };

        // Artifacts live in the artifacts store, not agentflare-store docs
        // (and asset docs carry empty FTS content — the bytes are in blobs),
        // so fold artifact matches in as their own group; without this the
        // "artifacts, notes" the tool description promises never match.
        let artifact_hits = self.artifact_search_hits(q, None).unwrap_or_default();

        self.with_store(|store| -> Result<String, ErrorData> {
            // ponytail: no valid FTS5 tokens (e.g. query is only quote chars) -- return
            // no matches instead of falling back to the unsanitized raw query.
            let Some(fts_q) = fts_query(q, Default::default()) else {
                let result = serde_json::json!({
                    "query": q,
                    "source": "store",
                    "total": 0,
                    "groups": {},
                });
                return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
            };
            let matches = store
                .doc_search(&ws_id, &fts_q, limit)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let mut grouped: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
                std::collections::BTreeMap::new();

            for m in matches {
                let Some(doc) = store
                    .doc_get(&m.id)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                else {
                    continue; // stale FTS row / doc deleted between search and get
                };

                let entry = serde_json::json!({
                    "id": doc.id,
                    "path": doc.path,
                    "title": doc.title,
                    "doc_type": doc.doc_type,
                    "snippet": m.snippet,
                    "score": m.score,
                    "source": doc.source,
                    "mime": doc.mime,
                    "size": doc.size,
                    "created_at": doc.created_at,
                    "updated_at": doc.updated_at,
                });
                grouped
                    .entry(if doc.doc_type.is_empty() {
                        "unknown".into()
                    } else {
                        doc.doc_type.clone()
                    })
                    .or_default()
                    .push(entry);
            }

            if !artifact_hits.is_empty() {
                grouped.insert(
                    "artifact".into(),
                    artifact_hits.into_iter().take(limit).collect(),
                );
            }

            let result = serde_json::json!({
                "query": q,
                "source": "store",
                "total": grouped.values().map(|v| v.len()).sum::<usize>(),
                "groups": grouped,
            });
            Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
        })?
    }

    fn search_memory(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(20);

        let brain = match crate::memory::store::open() {
            Ok(conn) => conn,
            Err(e) => {
                return Err(ErrorData::internal_error(
                    format!("failed to open brain.db: {e}"),
                    None,
                ));
            }
        };

        let observations = match crate::memory::search::search(&brain, q, None, None, limit) {
            Ok(obs) => obs,
            Err(e) => {
                return Err(ErrorData::internal_error(
                    format!("memory search failed: {e}"),
                    None,
                ));
            }
        };

        let mut grouped: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
            std::collections::BTreeMap::new();

        for obs in observations {
            let entry = serde_json::json!({
                "id": obs.id,
                "type": obs.r#type,
                "title": obs.title,
                "content": obs.content,
                "project": obs.project,
                "session_id": obs.session_id,
                "created_at": obs.created_at,
                "updated_at": obs.updated_at,
                "pinned": obs.pinned,
                "topic_key": obs.topic_key,
            });
            let key = if obs.r#type.is_empty() {
                "unknown".into()
            } else {
                obs.r#type.clone()
            };
            grouped.entry(key).or_default().push(entry);
        }

        Ok(serde_json::json!({
            "query": q,
            "source": "memory",
            "total": grouped.values().map(|v| v.len()).sum::<usize>(),
            "groups": grouped,
        })
        .to_string())
    }

    /// Delegates to the gateway's `leanctx` server (`ctx_search`, regex
    /// action) -- same pattern as the web arm; no subprocess, no output
    /// parsing. Unregistered/unavailable server degrades to an error payload.
    async fn search_code(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(50);
        let root = Self::repo_root();

        let guard = self.ensure_gateway_registry().await?;
        let reg = guard.as_ref().expect("ensured above");

        let args = serde_json::json!({
            "pattern": q,
            "path": root.to_string_lossy(),
            "max_results": limit,
        });

        match reg.execute("leanctx", "ctx_search", args).await {
            Ok(val) => Ok(serde_json::json!({
                "source": "code",
                "query": q,
                "results": val,
            })
            .to_string()),
            Err(e) => Ok(serde_json::json!({
                "source": "code",
                "query": q,
                "error": format!("leanctx ctx_search failed: {e}"),
                "results": [],
            })
            .to_string()),
        }
    }

    async fn search_web(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // rivalsearch web_search schema bounds num_results to 1..=20; clamp so an
        // out-of-range limit gets truncated instead of failing the whole call.
        let limit = req.limit.unwrap_or(10).clamp(1, 20);

        let guard = self.ensure_gateway_registry().await?;
        let reg = guard.as_ref().expect("ensured above");

        // rivalsearch's web_search takes `num_results` (1..=20), not
        // `max_results`; its crawl/extract flags default ON, far too heavy
        // for a search arm — ask for the plain result list.
        let args = serde_json::json!({
            "query": q,
            "num_results": limit,
            "extract_content": false,
            "follow_links": false,
        });
        match reg.execute("rivalsearch", "web_search", args).await {
            Ok(val) => Ok(serde_json::json!({
                "source": "web",
                "query": q,
                "results": val,
            })
            .to_string()),
            Err(e) => Ok(serde_json::json!({
                "source": "web",
                "query": q,
                "error": format!("rivalsearch web_search failed: {e}"),
                "results": [],
            })
            .to_string()),
        }
    }
}
