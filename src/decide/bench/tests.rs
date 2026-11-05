use super::*;

fn run(site: &str, cases: &[Case], th: &Thresholds) -> Vec<SiteReport> {
    run_site(site, cases, &mut FakeBackend::new(Profile::default()), th)
}

fn json_without_overhead(r: Vec<SiteReport>) -> String {
    let mut sites = r;
    for s in &mut sites {
        s.overhead_pct = None;
    }
    render_json(&Report {
        schema: REPORT_SCHEMA,
        backend: "t".into(),
        sites,
    })
}

#[test]
fn builtin_fixtures_cover_every_site_with_an_adversarial_set() {
    for site in SITES {
        let cases = builtin_cases(site);
        assert!(cases.iter().any(|c| !c.adversarial), "{site} main");
        assert!(cases.iter().any(|c| c.adversarial), "{site} adversarial");
        let reports = run(site, &cases, &Thresholds::default());
        assert_eq!(reports.len(), 2, "{site}");
        assert!(reports.iter().all(|r| r.labeled == r.cases));
    }
}

#[test]
fn fake_backend_runs_are_deterministic() {
    let cases = builtin_cases("router");
    let a = json_without_overhead(run("router", &cases, &Thresholds::default()));
    let b = json_without_overhead(run("router", &cases, &Thresholds::default()));
    assert_eq!(a, b);
}

/// Latency of call `nth` is `nth + 1` ms: the nearest-rank p50/p95 of 1..=100 are 50 and 95.
struct Ramp;
impl Backend for Ramp {
    fn name(&self) -> String {
        "ramp".into()
    }
    fn ask(&mut self, case: &Case, nth: usize) -> Result<Reply, backend::Failure> {
        Ok(Reply {
            label: case.baseline.clone(),
            confidence: Some(1.0),
            latency_us: (nth as u64 + 1) * 1000,
            cost: 0.0,
        })
    }
    fn force(&mut self, _: Option<FailureMode>) {}
    fn cold_calls(&self) -> usize {
        0
    }
}

fn cases_of(n: usize) -> Vec<Case> {
    (0..n)
        .map(|i| Case {
            site: "router".into(),
            baseline: format!("b{}", i % 3),
            ..Case::default()
        })
        .collect()
}

#[test]
fn mocked_latency_reproduces_known_p50_p95() {
    let r = run_site("router", &cases_of(100), &mut Ramp, &Thresholds::default());
    let warm = &r[0].latency_warm;
    assert_eq!((warm.p50_ms, warm.p95_ms), (Some(50.0), Some(95.0)));
    assert_eq!(r[0].latency_cold.n, 0);
}

#[test]
fn harness_overhead_is_under_five_percent_of_latency() {
    let r = run("router", &cases_of(2000), &Thresholds::default());
    let ovh = r[0].overhead_pct.expect("latency was reported");
    assert!(ovh < 5.0, "overhead {ovh}%");
}

#[test]
fn every_failure_mode_fails_open_to_the_baseline() {
    let r = run("router", &builtin_cases("router"), &Thresholds::default());
    for set in &r {
        assert_eq!(set.failure_modes.len(), 4);
        assert!(
            set.failure_modes
                .iter()
                .all(|m| m.fail_open_rate == Some(1.0))
        );
    }
    let timeout = r[0]
        .failure_modes
        .iter()
        .find(|m| m.mode == "timeout")
        .unwrap();
    assert_eq!(timeout.mean_fallback_ms, Some(3000.0));
}

#[test]
fn metrics_match_hand_computation() {
    // router r01..r12, min_confidence 0.7: r12 (0.55) falls back to baseline "medium".
    let r = run("router", &builtin_cases("router"), &Thresholds::default());
    let main = &r[0];
    assert_eq!(main.labeled, 12);
    // Jev final: r08 easy (wrong), r12 baseline medium (wrong) -> 10/12 correct.
    assert!((main.jev.accuracy.unwrap() - 10.0 / 12.0).abs() < 1e-9);
    // Baseline wrong on r04 r07 r10 r12 -> 8/12.
    assert!((main.baseline.accuracy.unwrap() - 8.0 / 12.0).abs() < 1e-9);
}

#[test]
fn token_counting_is_chars_over_four_rounded_up() {
    assert_eq!(count_tokens(""), 0);
    assert_eq!(count_tokens("abcd"), 1);
    assert_eq!(count_tokens("abcde"), 2);
    assert_eq!(count_tokens("ééééé"), 2); // chars, not bytes
}

#[test]
fn avoided_tokens_count_only_applied_decisions() {
    let th =
        Thresholds::parse("[default]\nllm_tokens_avoided = 100\nmin_confidence = 0.9\n").unwrap();
    let r = run("router", &builtin_cases("router"), &th);
    // main router confidences >= 0.9: r01 r02 r03 r09 r11.
    assert_eq!(r[0].tokens_avoided, 500);
    assert!(r[0].tokens_added > 0);
}

#[test]
fn shipped_thresholds_file_parses_and_unknown_keys_are_rejected() {
    let t = Thresholds::parse(include_str!("../../../docs/decide-bench-thresholds.toml")).unwrap();
    assert_eq!(t.for_site("router").llm_tokens_avoided, Some(400));
    assert_eq!(t.for_site("sdd_judge").min_precision, Some(0.60));
    assert_eq!(t.for_site("router").min_precision, Some(0.70));
    assert!(Thresholds::parse("[default]\nbogus = 1\n").is_err());
}

#[test]
fn violated_thresholds_give_no_go_and_unmeasured_ones_fail() {
    let th = Thresholds::parse("[default]\nmin_agreement = 0.99\nmax_warm_p95_ms = 10\n").unwrap();
    let r = run("router", &builtin_cases("router"), &th);
    assert!(!r[0].verdict.go);
    assert_eq!(r[0].verdict.violations.len(), 2);
    let unlabeled = Thresholds::parse("[default]\nmin_precision = 0.5\n").unwrap();
    let r = run("router", &cases_of(5), &unlabeled);
    assert!(r[0].verdict.violations[0].contains("unmeasured"));
}

#[test]
fn captured_rows_parse_as_cases() {
    let line = r#"{"decision_id":null,"schema":1,"ts":1,"site":"router","features":{"prompt":"x"},"norm_key":"k","label":{"summary":"easy"},"confidence":0.9,"baseline":"haiku"}"#;
    let (cases, bad) = parse_cases(&format!("{line}\nnot json\n"));
    assert_eq!((cases.len(), bad), (1, 1));
    assert_eq!(cases[0].recorded_label().as_deref(), Some("easy"));
}

#[test]
fn table_has_one_row_per_site_set() {
    let r = run("router", &builtin_cases("router"), &Thresholds::default());
    let report = Report {
        schema: REPORT_SCHEMA,
        backend: "t".into(),
        sites: r,
    };
    let table = render_table(&report);
    assert_eq!(
        table.lines().filter(|l| l.starts_with("| router")).count(),
        2
    );
}
