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
            "social" => self.search_social(&req).await,
            "news" => self.search_news(&req).await,
            "github" => self.search_github(&req).await,
            "academic" => self.search_academic(&req).await,
            "datasets" => self.search_datasets(&req).await,
            "websites" => self.search_websites(&req).await,
            "weather" => self.search_weather(&req).await,
            "financial" => self.search_financial(&req).await,
            "crypto" => self.search_crypto(&req).await,
            "fx" => self.search_fx(&req).await,
            "indicators" => self.search_indicators(&req).await,
            "youtube" => self.search_youtube(&req).await,
            "bluesky" => self.search_bluesky(&req).await,
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown type '{other}' — use store|memory|code|web|social|news|github|academic|datasets|websites|weather|financial|crypto|fx|indicators|youtube|bluesky"
                ),
                None,
            )),
        }
    }

    async fn gateway_execute(
        &self,
        server: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let guard = self
            .ensure_gateway_registry()
            .await
            .map_err(|e| e.to_string())?;
        let reg = guard.as_ref().expect("ensured above");
        reg.execute(server, tool, args)
            .await
            .map_err(|e| gateway_registry::redact_error_for_llm(&e.to_string()))
    }

    fn search_store(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(20);
        // Local query rewriting (AI Search § query rewriting, local rule-based + sparse)
        let effective_q = agentflare_store::fastembed::try_rewrite_query(q)
            .unwrap_or_else(|| q.to_string());
        let fts_q_raw = effective_q.as_str();

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
            // Similarity cache (AI Search § similarity cache) — 5 min TTL via store_kv; bypass when filters present
            let use_cache = req.meta.is_none() && req.path_glob.is_none() && req.min_score.is_none();
            if use_cache {
                if let Some(cached) = store.search_cache_get(q, &ws_id) {
                    let mut grouped: std::collections::BTreeMap<String, Vec<serde_json::Value>> =
                        std::collections::BTreeMap::new();
                    for m in cached.iter().take(limit) {
                        if let Some(doc) = store.doc_get(&m.id).ok().flatten() {
                            let entry = serde_json::json!({
                                "id": doc.id, "path": doc.path, "title": doc.title,
                                "doc_type": doc.doc_type, "snippet": m.snippet, "score": m.score,
                                "source": doc.source, "mime": doc.mime, "size": doc.size,
                                "created_at": doc.created_at, "updated_at": doc.updated_at,
                            });
                            grouped.entry(if doc.doc_type.is_empty() { "unknown".into() } else { doc.doc_type.clone() }).or_default().push(entry);
                        }
                    }
                    if !grouped.is_empty() {
                        let result = serde_json::json!({ "query": q, "source": "store", "total": grouped.values().map(|v| v.len()).sum::<usize>(), "groups": grouped, "cached": true });
                        return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
                    }
                }
            }
            // ponytail: no valid FTS5 tokens (e.g. query is only quote chars) -- return
            // no matches instead of falling back to the unsanitized raw query.
            let Some(fts_q) = fts_query(fts_q_raw, Default::default()) else {
                let result = serde_json::json!({
                    "query": q,
                    "source": "store",
                    "total": 0,
                    "groups": {},
                });
                return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
            };
            // Filtered path (AI Search § filtering + path filtering): when meta/path_glob present,
            // use doc_search_filtered (exact meta, GLOB) and skip chunk hybrid + cache.
            let matches = if req.meta.is_some() || req.path_glob.is_some() {
                let meta_vec: Option<Vec<(String, String)>> = req.meta.as_ref().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
                let mut hits = store
                    .doc_search_filtered(&ws_id, fts_q_raw, limit, meta_vec.as_deref(), req.path_glob.as_deref())
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                if let Some(min) = req.min_score {
                    hits.retain(|m| m.score >= min);
                }
                hits
            } else {
                // Local-first hybrid: doc BM25 + chunk BM25 fused via RRF (K=60).
                let doc_hits = store
                    .doc_search(&ws_id, &fts_q, limit)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            let chunk_hits = {
                let qvec = agentflare_store::fastembed::try_embed(fts_q_raw);
                if let Some(ref vec) = qvec {
                    store
                        .chunk_hybrid_search(&ws_id, fts_q_raw, Some(vec), limit)
                        .unwrap_or_else(|_| store.chunk_search(&ws_id, fts_q_raw, limit).unwrap_or_default())
                } else {
                    store.chunk_search(&ws_id, fts_q_raw, limit).unwrap_or_default()
                }
            };
                let mut out = if chunk_hits.is_empty() {
                    doc_hits
                } else if doc_hits.is_empty() {
                    chunk_hits
                } else {
                    let doc_ids: Vec<String> = doc_hits.iter().map(|m| m.id.clone()).collect();
                    let chunk_ids: Vec<String> = chunk_hits.iter().map(|m| m.id.clone()).collect();
                    let fused = agentflare_store::retrieval::rrf_fuse(&doc_ids, &chunk_ids, 60.0);
                    let doc_by_id: std::collections::HashMap<_, _> =
                        doc_hits.into_iter().map(|m| (m.id.clone(), m)).collect();
                    let chunk_by_id: std::collections::HashMap<_, _> =
                        chunk_hits.into_iter().map(|m| (m.id.clone(), m)).collect();
                    let mut o: Vec<agentflare_store::documents::DocMatch> = Vec::new();
                    for (id, _) in fused {
                        if let Some(m) = doc_by_id.get(&id).cloned().or_else(|| chunk_by_id.get(&id).cloned()) {
                            o.push(m);
                            if o.len() >= limit { break; }
                        }
                    }
                    o
                };
                if let Some(min) = req.min_score {
                    out.retain(|m| m.score >= min);
                }
                // Rerank toggle (default true when model available)
                let do_rerank = req.rerank.unwrap_or(true) && out.len() > 1;
                if do_rerank {
                    let docs_for_rerank: Vec<String> = out
                        .iter()
                        .map(|m| if m.snippet.is_empty() { m.path.clone() } else { format!("{} — {}", m.path, m.snippet) })
                        .collect();
                    if let Some(reranked) = agentflare_store::fastembed::try_rerank(q, docs_for_rerank) {
                        let mut by_doc: std::collections::HashMap<String, agentflare_store::documents::DocMatch> =
                            out.into_iter().map(|m| {
                                let key = if m.snippet.is_empty() { m.path.clone() } else { format!("{} — {}", m.path, m.snippet) };
                                (key, m)
                            }).collect();
                        let mut reranked_out = Vec::new();
                        for (doc_text, _score) in reranked {
                            if let Some(m) = by_doc.remove(&doc_text) {
                                reranked_out.push(m);
                            }
                        }
                        reranked_out.extend(by_doc.into_values());
                        out = reranked_out;
                    }
                }
                if use_cache {
                    store.search_cache_put(q, &ws_id, &out);
                }
                out
            };
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

    async fn search_code(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(50);
        let root = Self::repo_root();

        let args = serde_json::json!({
            "pattern": q,
            "path": root.to_string_lossy(),
            "max_results": limit,
        });

        match self.gateway_execute("leanctx", "ctx_search", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "code", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "code", "query": q, "error": format!("leanctx ctx_search failed: {e}"), "results": []}).to_string()),
        }
    }

    async fn search_web(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 20);
        let args = serde_json::json!({
            "query": q,
            "num_results": limit,
            "extract_content": false,
            "follow_links": false,
        });
        match self.gateway_execute("rivalsearch", "web_search", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "web", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "web", "query": q, "error": format!("rivalsearch web_search failed: {e}"), "results": []}).to_string()),
        }
    }

    // ── RivalSearch-backed arms ──────────────────────────────────────────────

    /// Shared response handling for the six `rivalsearch`-backed arms below —
    /// each only differs in source label, tool name, and args.
    async fn rivalsearch_arm(
        &self,
        source: &str,
        tool: &str,
        q: &str,
        args: serde_json::Value,
    ) -> String {
        match self.gateway_execute("rivalsearch", tool, args).await {
            Ok(val) => {
                serde_json::json!({"source": source, "query": q, "results": val}).to_string()
            }
            Err(e) => serde_json::json!({
                "source": source, "query": q,
                "error": format!("rivalsearch {tool} failed: {e}"), "results": []
            })
            .to_string(),
        }
    }

    async fn search_social(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 25);
        let args = serde_json::json!({
            "query": q,
            "max_results_per_platform": limit,
        });
        Ok(self
            .rivalsearch_arm("social", "social_search", q, args)
            .await)
    }

    async fn search_news(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50);
        let args = serde_json::json!({
            "query": q,
            "max_results": limit,
        });
        Ok(self
            .rivalsearch_arm("news", "news_aggregation", q, args)
            .await)
    }

    async fn search_github(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50);
        let args = serde_json::json!({
            "query": q,
            "max_results": limit,
        });
        Ok(self
            .rivalsearch_arm("github", "github_search", q, args)
            .await)
    }

    async fn search_academic(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50);
        let args = serde_json::json!({
            "operation": "academic_search",
            "query": q,
            "max_results": limit,
        });
        Ok(self
            .rivalsearch_arm("academic", "scientific_research", q, args)
            .await)
    }

    async fn search_datasets(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(10).clamp(1, 50);
        let args = serde_json::json!({
            "operation": "dataset_discovery",
            "query": q,
            "max_results": limit,
        });
        Ok(self
            .rivalsearch_arm("datasets", "scientific_research", q, args)
            .await)
    }

    async fn search_websites(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        let limit = req.limit.unwrap_or(5).clamp(1, 20);
        let args = serde_json::json!({
            "url": q,
            "max_pages": limit,
            "mode": "research",
        });
        Ok(self
            .rivalsearch_arm("websites", "map_website", q, args)
            .await)
    }

    // ── Inline API arms (OpenResearch replacements) ──────────────────────────

    async fn search_weather(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_weather).await
    }

    // q = "location" or "location, days" — trailing segment is only a day
    // count if it parses as one, so "Paris, France" keeps its country.
    fn fetch_weather(q: &str) -> String {
        let (location, days) = match q.rsplit_once(',') {
            Some((head, tail)) => match tail.trim().parse::<u32>() {
                Ok(d) => (head.trim(), d.clamp(1, 16)),
                Err(_) => (q, 7),
            },
            None => (q, 7),
        };

        let geo_url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
            urlencoding(location)
        );
        let geo_resp = match http_agent().get(&geo_url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "weather", "query": q, "error": format!("geocoding failed: {e}")}).to_string(),
        };
        let (Some(lat), Some(lon)) = (
            geo_resp["results"][0]["latitude"].as_f64(),
            geo_resp["results"][0]["longitude"].as_f64(),
        ) else {
            return serde_json::json!({"source": "weather", "query": q, "error": "location not found".to_string()}).to_string();
        };
        let name = geo_resp["results"][0]["name"].as_str().unwrap_or(location);

        let forecast_url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m&daily=temperature_2m_max,temperature_2m_min,precipitation_sum,weather_code&timezone=auto&forecast_days={days}"
        );
        let forecast = match http_agent().get(&forecast_url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "weather", "query": q, "error": format!("forecast failed: {e}")}).to_string(),
        };

        serde_json::json!({"source": "weather", "query": q, "location": name, "result": forecast})
            .to_string()
    }

    async fn search_financial(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_financial).await
    }

    // Ticker→CIK is a static whole-file mapping, not queryable by ticker via URL —
    // resolve it locally first, then fetch that filer's submissions.
    fn fetch_financial(q: &str) -> String {
        let ticker = q.to_uppercase();
        let ua = "agentflare/1.0 (github.com/getappz/agentflare)";
        let tickers = match cached_sec_tickers(ua) {
            Ok(v) => v,
            Err(e) => return serde_json::json!({"source": "financial", "query": q, "error": format!("SEC ticker lookup failed: {e}")}).to_string(),
        };
        let cik = tickers
            .as_object()
            .and_then(|m| {
                m.values()
                    .find(|v| v["ticker"].as_str() == Some(ticker.as_str()))
            })
            .and_then(|v| v["cik_str"].as_u64());
        let Some(cik) = cik else {
            return serde_json::json!({"source": "financial", "query": q, "error": format!("no SEC filer found for ticker '{ticker}'")}).to_string();
        };

        let url = format!("https://data.sec.gov/submissions/CIK{cik:010}.json");
        let resp = match http_agent().get(&url).set("User-Agent", ua).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "financial", "query": q, "error": format!("SEC lookup failed: {e}")}).to_string(),
        };

        serde_json::json!({"source": "financial", "query": q, "result": resp}).to_string()
    }

    async fn search_crypto(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_crypto).await
    }

    // q = coin id or ticker symbol e.g. "bitcoin" or "btc" — CoinGecko's `ids` param
    // wants the id slug, so map common ticker symbols to it first.
    fn fetch_crypto(q: &str) -> String {
        let key = q.to_lowercase();
        let coin = CRYPTO_SYMBOL_TO_ID
            .iter()
            .find_map(|(sym, id)| (*sym == key).then_some(*id))
            .unwrap_or(key.as_str());
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            urlencoding(coin)
        );
        let resp = match http_agent().get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "crypto", "query": q, "error": format!("CoinGecko lookup failed: {e}")}).to_string(),
        };

        serde_json::json!({"source": "crypto", "query": q, "result": resp}).to_string()
    }

    async fn search_fx(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_fx).await
    }

    // q = "USD" or "USD,EUR" or "USD,EUR,GBP"
    fn fetch_fx(q: &str) -> String {
        let parts: Vec<&str> = q.split(',').map(|s| s.trim()).collect();
        let base = parts.first().copied().unwrap_or("USD");
        let symbols = if parts.len() > 1 {
            parts[1..].join(",")
        } else {
            String::new()
        };

        let base = urlencoding(base);
        let symbols = urlencoding(&symbols);
        let url = if symbols.is_empty() {
            format!("https://api.frankfurter.dev/latest?base={base}")
        } else {
            format!("https://api.frankfurter.dev/latest?base={base}&symbols={symbols}")
        };
        let resp = match http_agent().get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "fx", "query": q, "error": format!("FX lookup failed: {e}")}).to_string(),
        };

        serde_json::json!({"source": "fx", "query": q, "result": resp}).to_string()
    }

    async fn search_indicators(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_indicators).await
    }

    // q = keyword to find World Bank indicators
    fn fetch_indicators(q: &str) -> String {
        let url = format!(
            "https://api.worldbank.org/v2/indicator?format=json&per_page=20&search={}",
            urlencoding(q)
        );
        let resp = match http_agent().get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "indicators", "query": q, "error": format!("World Bank lookup failed: {e}")}).to_string(),
        };

        serde_json::json!({"source": "indicators", "query": q, "result": resp}).to_string()
    }

    /// Fetch YouTube video transcript via the public yt transcript API (no key).
    async fn search_youtube(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_youtube).await
    }

    fn fetch_youtube(q: &str) -> String {
        // q = YouTube video URL or ID
        let video_id = extract_youtube_id(q).unwrap_or(q);
        let url = format!(
            "https://youtubetranscriptapi.vercel.app/api/transcript?videoId={}",
            urlencoding(video_id)
        );
        match http_agent().get(&url).call() {
            Ok(r) => {
                let text = r.into_string().unwrap_or_default();
                serde_json::json!({"source": "youtube", "query": q, "transcript": text}).to_string()
            }
            Err(e) => serde_json::json!({"source": "youtube", "query": q, "error": format!("transcript fetch failed: {e}")}).to_string(),
        }
    }

    async fn search_bluesky(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        blocking_task(q, Self::fetch_bluesky).await
    }

    // q = Bluesky handle (with or without @)
    fn fetch_bluesky(q: &str) -> String {
        let handle = urlencoding(q.trim_start_matches('@'));
        let url =
            format!("https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor={handle}");
        let profile = match http_agent().get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return serde_json::json!({"source": "bluesky", "query": q, "error": format!("profile fetch failed: {e}")}).to_string(),
        };

        // also fetch recent posts
        let feed_url = format!(
            "https://public.api.bsky.app/xrpc/app.bsky.feed.getAuthorFeed?actor={handle}&limit=10"
        );
        let feed = http_agent()
            .get(&feed_url)
            .call()
            .ok()
            .and_then(|r| r.into_json::<serde_json::Value>().ok());

        serde_json::json!({"source": "bluesky", "query": q, "profile": profile, "feed": feed})
            .to_string()
    }
}

/// Runs a blocking inline-API fetch (`fetch_fn`) off the Tokio runtime via
/// `spawn_blocking`, so a stalled upstream HTTP call parks a blocking-pool
/// thread instead of a runtime worker. `fetch_fn` never fails outright —
/// every arm encodes upstream errors into its own JSON `"error"` field — so
/// the only failure mode here is the blocking task itself panicking.
async fn blocking_task(q: &str, fetch_fn: fn(&str) -> String) -> Result<String, ErrorData> {
    let q = q.to_string();
    tokio::task::spawn_blocking(move || fetch_fn(&q))
        .await
        .map_err(|e| ErrorData::internal_error(format!("search task panicked: {e}"), None))
}

/// A shared `ureq` agent with explicit connect/read timeouts so a stalled
/// upstream (SEC, CoinGecko, etc.) can't hang the blocking-pool thread it
/// runs on indefinitely — same rationale as `channels::http_agent`.
fn http_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .build()
    })
}

/// SEC's ticker→CIK file (~1MB, ~10k entries) rarely changes; fetch it once
/// per process instead of on every `financial` query.
// flare-code: process-lifetime cache, no refresh — restart to pick up new
// listings. Add a TTL if long-running daemons need same-day freshness.
static SEC_TICKER_CACHE: std::sync::OnceLock<std::sync::Mutex<Option<serde_json::Value>>> =
    std::sync::OnceLock::new();

fn cached_sec_tickers(user_agent: &str) -> Result<serde_json::Value, String> {
    let cache = SEC_TICKER_CACHE.get_or_init(|| std::sync::Mutex::new(None));
    {
        let guard = cache.lock().map_err(|e| e.to_string())?;
        if let Some(v) = guard.as_ref() {
            return Ok(v.clone());
        }
    }
    let resp = http_agent()
        .get("https://www.sec.gov/files/company_tickers.json")
        .set("User-Agent", user_agent)
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<serde_json::Value>()
        .map_err(|e| e.to_string())?;
    *cache.lock().map_err(|e| e.to_string())? = Some(resp.clone());
    Ok(resp)
}

/// Common ticker symbols → CoinGecko coin ids; CoinGecko's `ids` param wants
/// the id slug ("bitcoin"), not the ticker ("btc"). Full ids pass through as-is.
const CRYPTO_SYMBOL_TO_ID: &[(&str, &str)] = &[
    ("btc", "bitcoin"),
    ("eth", "ethereum"),
    ("usdt", "tether"),
    ("bnb", "binancecoin"),
    ("sol", "solana"),
    ("xrp", "ripple"),
    ("usdc", "usd-coin"),
    ("ada", "cardano"),
    ("doge", "dogecoin"),
    ("trx", "tron"),
    ("ton", "the-open-network"),
    ("dot", "polkadot"),
    ("matic", "matic-network"),
    ("ltc", "litecoin"),
    ("shib", "shiba-inu"),
    ("link", "chainlink"),
    ("avax", "avalanche-2"),
    ("bch", "bitcoin-cash"),
    ("xlm", "stellar"),
    ("atom", "cosmos"),
];

/// Percent-encode a string for URL query parameters (simple version, covers
/// the common cases without pulling in a full URL library as a new dep).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Extract a YouTube video ID from various URL formats or return None.
fn extract_youtube_id(input: &str) -> Option<&str> {
    let input = input.trim();
    // youtu.be/VIDEO_ID
    if let Some(id) = input
        .strip_prefix("https://youtu.be/")
        .or_else(|| input.strip_prefix("youtu.be/"))
    {
        return id.split(&['?', '&', '#']).next();
    }
    // youtube.com/shorts/VIDEO_ID (id lives in the path, not a query param)
    if let Some(rest) = input.split("youtube.com/shorts/").nth(1) {
        return rest.split(&['?', '&', '#', '/']).next();
    }
    // youtube.com/watch?v=VIDEO_ID
    if input.contains("youtube.com/watch")
        && let Some(query) = input.split('?').nth(1)
    {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("v=") {
                return val.split(&['?', '&', '#']).next();
            }
        }
    }
    // bare 11-char ID (YouTube ids are ASCII only)
    if input.len() == 11
        && input
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Some(input);
    }
    None
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn urlencoding_percent_encodes_reserved_bytes() {
        assert_eq!(urlencoding("bitcoin"), "bitcoin");
        assert_eq!(urlencoding("USD,EUR"), "USD%2CEUR");
        assert_eq!(urlencoding("hello world"), "hello%20world");
        assert_eq!(urlencoding("Paris, France"), "Paris%2C%20France");
        assert_eq!(urlencoding("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencoding("café"), "caf%C3%A9");
    }

    #[test]
    fn extract_youtube_id_handles_youtu_be() {
        assert_eq!(
            extract_youtube_id("https://youtu.be/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            extract_youtube_id("youtu.be/dQw4w9WgXcQ?t=30"),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn extract_youtube_id_handles_watch_url() {
        assert_eq!(
            extract_youtube_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10s"),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn extract_youtube_id_handles_shorts_url() {
        assert_eq!(
            extract_youtube_id("https://www.youtube.com/shorts/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            extract_youtube_id("https://www.youtube.com/shorts/dQw4w9WgXcQ?feature=share"),
            Some("dQw4w9WgXcQ")
        );
    }

    #[test]
    fn extract_youtube_id_handles_bare_ascii_id() {
        assert_eq!(extract_youtube_id("dQw4w9WgXcQ"), Some("dQw4w9WgXcQ"));
    }

    #[test]
    fn extract_youtube_id_rejects_non_ascii_bare_id() {
        // 11 *bytes* (the check is on input.len(), not char count) containing
        // one non-ASCII char ('é' = 2 bytes) — must be rejected, not treated
        // as alphanumeric via the Unicode-aware `char::is_alphanumeric`.
        let candidate = "café123456";
        assert_eq!(candidate.len(), 11);
        assert_eq!(extract_youtube_id(candidate), None);
    }

    #[test]
    fn extract_youtube_id_rejects_unrecognized_input() {
        assert_eq!(extract_youtube_id("not a video id"), None);
    }
}
