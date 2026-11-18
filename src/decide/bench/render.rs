use super::Report;
use std::fmt::Write;

pub fn render_json(r: &Report) -> String {
    serde_json::to_string_pretty(r).expect("finite report")
}

fn f(x: Option<f64>, digits: usize) -> String {
    x.map_or("-".into(), |v| format!("{v:.digits$}"))
}

/// One markdown table: a row per site/set, then any violated thresholds.
pub fn render_table(r: &Report) -> String {
    let mut o = format!("backend: {}\n", r.backend);
    if r.synthetic {
        o.push_str("SYNTHETIC: simulated Jev answers and latency; measures the harness, not the sites (use --live).\n");
    }
    o.push('\n');
    o.push_str(
        "| site | set | n | agree | P | R | base P | base R | cold p50/p95 ms | warm p50/p95 ms | tok +/- | $/1k | failure modes (fail-open) | ovh % | verdict |\n",
    );
    o.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for s in &r.sites {
        let modes = s
            .failure_modes
            .iter()
            .map(|m| format!("{}:{}", m.mode, f(m.fail_open_rate, 2)))
            .collect::<Vec<_>>()
            .join(" ");
        let _ = writeln!(
            o,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {}/{} | {}/{} | +{}/-{} | {} | {} | {} | {} |",
            s.site,
            s.set,
            s.cases,
            f(s.agreement, 2),
            f(s.jev.precision, 2),
            f(s.jev.recall, 2),
            f(s.baseline.precision, 2),
            f(s.baseline.recall, 2),
            f(s.latency_cold.p50_ms, 0),
            f(s.latency_cold.p95_ms, 0),
            f(s.latency_warm.p50_ms, 0),
            f(s.latency_warm.p95_ms, 0),
            s.tokens_added,
            s.tokens_avoided,
            f(s.usd_per_1k, 4),
            modes,
            f(s.overhead_pct, 3),
            if s.verdict.go { "GO" } else { "NO-GO" },
        );
    }
    for s in r.sites.iter().filter(|s| !s.verdict.go) {
        for v in &s.verdict.violations {
            let _ = writeln!(o, "- {} ({}): {v}", s.site, s.set);
        }
    }
    o
}
