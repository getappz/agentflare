use super::*;

impl AgentflareMcp {
    pub async fn search_impl(&self, req: SearchRequest) -> Result<String, ErrorData> {
        let search_type = req.r#type.as_deref().unwrap_or("store");
        match search_type {
            "code" => self.search_code(&req),
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

        self.with_store(|store| -> Result<String, ErrorData> {
            let matches = store
                .doc_search(&ws_id, q, limit)
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

    fn search_code(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let root = Self::repo_root();
        if !root.exists() {
            return Ok(serde_json::json!({
                "source": "code",
                "query": q,
                "note": "No project root found. Run agentflare from within a git repo.",
                "results": [],
                "total": 0,
            })
            .to_string());
        }

        let output = std::process::Command::new("lean-ctx")
            .arg("grep")
            .arg(q)
            .current_dir(&root)
            .output()
            .map_err(|e| {
                ErrorData::internal_error(format!("failed to run lean-ctx grep: {e}"), None)
            })?;

        if !output.status.success() && output.stdout.is_empty() {
            return Ok(serde_json::json!({
                "source": "code",
                "query": q,
                "error": String::from_utf8_lossy(&output.stderr).trim(),
                "results": [],
                "total": 0,
            })
            .to_string());
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let mut results = Vec::new();
        let limit = req.limit.unwrap_or(50);
        let mut in_symbol = false;

        for line in raw.lines() {
            if results.len() >= limit {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Symbol context lines from lean-ctx
            if line.starts_with("[∈") {
                in_symbol = true;
                continue;
            }
            // Parse file:line content — lean-ctx grep output. Paths are
            // relative to `root` (current_dir), so the first ':' is never a
            // Windows drive letter. symbol_context tags only the first line
            // after a [∈ marker.
            if let Some((file, rest)) = line.split_once(':') {
                // Find the space separating line number from content
                if let Some(space_idx) = rest.find(' ') {
                    let line_str = &rest[..space_idx];
                    let content = rest[space_idx + 1..].trim();
                    if let Ok(line_num) = line_str.parse::<usize>() {
                        results.push(serde_json::json!({
                            "file": file,
                            "line": line_num,
                            "text": content,
                            "symbol_context": in_symbol,
                        }));
                    }
                }
            }
            in_symbol = false;
        }

        Ok(serde_json::json!({
            "source": "code",
            "query": q,
            "total": results.len(),
            "results": results,
        })
        .to_string())
    }

    async fn search_web(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10);

        let guard = self.ensure_gateway_registry().await?;
        let reg = guard.as_ref().expect("ensured above");

        let args = serde_json::json!({
            "query": q,
            "max_results": limit,
        });

        let result = match reg.execute("rivalsearch", "web_search", args).await {
            Ok(val) => val,
            Err(e) => {
                return Ok(serde_json::json!({
                    "source": "web",
                    "query": q,
                    "error": format!("rivalsearch web_search failed: {e}"),
                    "results": [],
                })
                .to_string());
            }
        };

        let result_str = serde_json::to_string_pretty(&result).unwrap_or_default();
        Ok(result_str)
    }
}
