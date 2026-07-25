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
            "youtube" => self.search_youtube(&req),
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
            .map_err(|e| e.to_string())
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
        match self.gateway_execute("rivalsearch", "social_search", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "social", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "social", "query": q, "error": format!("rivalsearch social_search failed: {e}"), "results": []}).to_string()),
        }
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
        match self.gateway_execute("rivalsearch", "news_aggregation", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "news", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "news", "query": q, "error": format!("rivalsearch news_aggregation failed: {e}"), "results": []}).to_string()),
        }
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
        match self.gateway_execute("rivalsearch", "github_search", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "github", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "github", "query": q, "error": format!("rivalsearch github_search failed: {e}"), "results": []}).to_string()),
        }
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
        match self.gateway_execute("rivalsearch", "scientific_research", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "academic", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "academic", "query": q, "error": format!("rivalsearch scientific_research failed: {e}"), "results": []}).to_string()),
        }
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
        match self.gateway_execute("rivalsearch", "scientific_research", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "datasets", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "datasets", "query": q, "error": format!("rivalsearch scientific_research failed: {e}"), "results": []}).to_string()),
        }
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
        match self.gateway_execute("rivalsearch", "map_website", args).await {
            Ok(val) => Ok(serde_json::json!({"source": "websites", "query": q, "results": val}).to_string()),
            Err(e) => Ok(serde_json::json!({"source": "websites", "query": q, "error": format!("rivalsearch map_website failed: {e}"), "results": []}).to_string()),
        }
    }

    // ── Inline API arms (OpenResearch replacements) ──────────────────────────

    async fn search_weather(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = "location" or "location, days" — use Open-Meteo geocoding + forecast
        let parts: Vec<&str> = q.splitn(2, ',').map(|s| s.trim()).collect();
        let location = parts[0];
        let days = parts
            .get(1)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(7)
            .clamp(1, 16);

        let geo_url = format!(
            "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
            urlencoding(location)
        );
        let geo_resp = match ureq::get(&geo_url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "weather", "query": q, "error": format!("geocoding failed: {e}")}).to_string()),
        };
        let Some(lat) = geo_resp["results"][0]["latitude"].as_f64() else {
            return Ok(serde_json::json!({"source": "weather", "query": q, "error": "location not found".to_string()}).to_string());
        };
        let lon = geo_resp["results"][0]["longitude"].as_f64().unwrap_or(0.0);
        let name = geo_resp["results"][0]["name"].as_str().unwrap_or(location);

        let forecast_url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m&daily=temperature_2m_max,temperature_2m_min,precipitation_sum,weather_code&timezone=auto&forecast_days={days}"
        );
        let forecast = match ureq::get(&forecast_url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "weather", "query": q, "error": format!("forecast failed: {e}")}).to_string()),
        };

        Ok(serde_json::json!({"source": "weather", "query": q, "location": name, "result": forecast}).to_string())
    }

    async fn search_financial(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = ticker or company name — try SEC EDGAR company search
        let ticker = q.to_uppercase();
        let url = format!("https://data.sec.gov/submissions/CIKLookup/{ticker}.json");
        let resp = match ureq::get(&url)
            .set("User-Agent", "agentflare/1.0 (github.com/getappz/agentflare)")
            .call()
        {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "financial", "query": q, "error": format!("SEC lookup failed: {e}")}).to_string()),
        };

        Ok(serde_json::json!({"source": "financial", "query": q, "result": resp}).to_string())
    }

    async fn search_crypto(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = coin name/symbol e.g. "bitcoin" or "btc"
        let coin = q.to_lowercase();
        let url =
            format!("https://api.coingecko.com/api/v3/simple/price?ids={coin}&vs_currencies=usd");
        let resp = match ureq::get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "crypto", "query": q, "error": format!("CoinGecko lookup failed: {e}")}).to_string()),
        };

        Ok(serde_json::json!({"source": "crypto", "query": q, "result": resp}).to_string())
    }

    async fn search_fx(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = "USD" or "USD,EUR" or "USD,EUR,GBP"
        let parts: Vec<&str> = q.split(',').map(|s| s.trim()).collect();
        let base = parts.first().copied().unwrap_or("USD");
        let symbols = if parts.len() > 1 {
            parts[1..].join(",")
        } else {
            String::new()
        };

        let url = if symbols.is_empty() {
            format!("https://api.frankfurter.dev/latest?base={base}")
        } else {
            format!("https://api.frankfurter.dev/latest?base={base}&symbols={symbols}")
        };
        let resp = match ureq::get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "fx", "query": q, "error": format!("FX lookup failed: {e}")}).to_string()),
        };

        Ok(serde_json::json!({"source": "fx", "query": q, "result": resp}).to_string())
    }

    async fn search_indicators(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = keyword to find World Bank indicators
        let url = format!(
            "https://api.worldbank.org/v2/indicator?format=json&per_page=20&search={}",
            urlencoding(q)
        );
        let resp = match ureq::get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "indicators", "query": q, "error": format!("World Bank lookup failed: {e}")}).to_string()),
        };

        Ok(serde_json::json!({"source": "indicators", "query": q, "result": resp}).to_string())
    }

    /// Fetch YouTube video transcript via the public yt transcript API (no key).
    fn search_youtube(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = YouTube video URL or ID
        let video_id = extract_youtube_id(q).unwrap_or(q);
        let url =
            format!("https://youtubetranscriptapi.vercel.app/api/transcript?videoId={video_id}");
        match ureq::get(&url).call() {
            Ok(r) => {
                let text = r.into_string().unwrap_or_default();
                Ok(serde_json::json!({"source": "youtube", "query": q, "transcript": text}).to_string())
            }
            Err(e) => Ok(serde_json::json!({"source": "youtube", "query": q, "error": format!("transcript fetch failed: {e}")}).to_string()),
        }
    }

    async fn search_bluesky(&self, req: &SearchRequest) -> Result<String, ErrorData> {
        let q = req.query.trim();
        if q.is_empty() {
            return Err(ErrorData::invalid_params("query must not be empty", None));
        }
        // q = Bluesky handle (with or without @)
        let handle = q.trim_start_matches('@');
        let url =
            format!("https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor={handle}");
        let profile = match ureq::get(&url).call() {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap_or_default(),
            Err(e) => return Ok(serde_json::json!({"source": "bluesky", "query": q, "error": format!("profile fetch failed: {e}")}).to_string()),
        };

        // also fetch recent posts
        let feed_url = format!(
            "https://public.api.bsky.app/xrpc/app.bsky.feed.getAuthorFeed?actor={handle}&limit=10"
        );
        let feed = ureq::get(&feed_url)
            .call()
            .ok()
            .and_then(|r| r.into_json::<serde_json::Value>().ok());

        Ok(
            serde_json::json!({"source": "bluesky", "query": q, "profile": profile, "feed": feed})
                .to_string(),
        )
    }
}

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
            _ => {
                for byte in std::ascii::escape_default(b) {
                    out.push(byte as char);
                }
            }
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
    // youtube.com/watch?v=VIDEO_ID
    if (input.contains("youtube.com/watch") || input.contains("youtube.com/shorts/"))
        && let Some(query) = input.split('?').nth(1)
    {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix("v=") {
                return val.split(&['?', '&', '#']).next();
            }
        }
    }
    // bare 11-char ID
    if input.len() == 11
        && input
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Some(input);
    }
    None
}
