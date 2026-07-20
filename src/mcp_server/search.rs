use super::*;

impl AgentflareMcp {
    pub fn search_impl(&self, req: SearchRequest) -> Result<String, ErrorData> {
        let search_type = req.r#type.as_deref().unwrap_or("store");
        match search_type {
            "code" => self.search_code(&req),
            "web" => self.search_web(&req),
            _ => self.search_store(&req),
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
                let doc = store
                    .doc_get(&m.id)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    .unwrap_or_else(|| panic!("doc_search returned id {} but doc_get failed", m.id));

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
            }).to_string());
        }

        let output = std::process::Command::new("lean-ctx")
            .arg("grep")
            .arg(q)
            .current_dir(&root)
            .output()
            .map_err(|e| ErrorData::internal_error(
                format!("failed to run lean-ctx grep: {e}"), None
            ))?;

        if !output.status.success() && output.stdout.is_empty() {
            return Ok(serde_json::json!({
                "source": "code",
                "query": q,
                "results": [],
                "total": 0,
            }).to_string());
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
            // Parse file:line:content — lean-ctx grep output
            if let Some((file, rest)) = line.split_once(':') {
                if let Some((line_str, _content)) = rest.split_once(':') {
                    if let Ok(line_num) = line_str.parse::<usize>() {
                        results.push(serde_json::json!({
                            "file": file,
                            "line": line_num,
                            "text": rest.splitn(2, ':').nth(1).unwrap_or("").trim(),
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
        }).to_string())
    }

    fn search_web(&self, _req: &SearchRequest) -> Result<String, ErrorData> {
        Ok(serde_json::json!({
            "source": "web",
            "note": "Web search is not yet implemented in the Rust MCP server. Use the web_search or rivalsearch tools via the agent orchestration layer instead.",
            "results": [],
        }).to_string())
    }
}
