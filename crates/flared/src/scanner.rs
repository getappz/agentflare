//! Process audit: enumerate via sysinfo, classify into workload buckets,
//! mark protected classes, and summarize memory/CPU pressure.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::model::{Bucket, ProcInfo};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pressure {
    pub total_mem_bytes: u64,
    pub avail_mem_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub cpu_pct: f32,
    /// green | yellow | red
    pub level: String,
}

/// Bucket + protected flag for one process, from its name/cmdline.
/// Priority: agents > build > browsers > terminals > desktop > other.
/// Browsers, terminals, and desktop are always protected, as is anything
/// matching `protect_patterns`.
pub fn classify(name: &str, cmd: &str, cfg: &Config) -> (Bucket, bool) {
    fn matches(patterns: &[String], haystack: &str) -> bool {
        patterns.iter().any(|p| haystack.contains(&p.to_ascii_lowercase()))
    }
    let name_lc = name.to_ascii_lowercase();
    let cmd_lc = cmd.to_ascii_lowercase();

    let bucket = if matches(&cfg.agent_patterns, &name_lc) || matches(&cfg.agent_patterns, &cmd_lc)
    {
        Bucket::Agents
    } else if matches(&cfg.build_patterns, &name_lc) {
        Bucket::Build
    } else if matches(&cfg.browser_patterns, &name_lc) {
        Bucket::Browsers
    } else if matches(&cfg.terminal_patterns, &name_lc) {
        Bucket::Terminals
    } else if matches(&cfg.desktop_patterns, &name_lc) {
        Bucket::Desktop
    } else {
        Bucket::Other
    };

    // Protection is decided by the executable NAME alone, independent of the
    // bucket: a browser whose cmdline mentions an agent pattern still lands in
    // the Agents bucket, but must never lose its protected status.
    let protected = matches(&cfg.browser_patterns, &name_lc)
        || matches(&cfg.terminal_patterns, &name_lc)
        || matches(&cfg.desktop_patterns, &name_lc)
        || matches(&cfg.protect_patterns, &name_lc)
        || matches(&cfg.protect_patterns, &cmd_lc);
    (bucket, protected)
}

/// Name + start time of one live process, if it exists. Used to fingerprint
/// a pid at lease-registration time.
pub fn identity_of(pid: u32) -> Option<(String, u64)> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let target = Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    sys.process(target)
        .map(|p| (p.name().to_string_lossy().to_string(), p.start_time()))
}

/// Full audit: classified process table + pressure summary.
pub fn scan(cfg: &Config) -> (HashMap<u32, ProcInfo>, Pressure) {
    use sysinfo::{ProcessesToUpdate, System};

    let mut sys = System::new_all();
    // Two samples are required for meaningful CPU percentages.
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut procs = HashMap::new();
    for (pid, p) in sys.processes() {
        let name = p.name().to_string_lossy().to_string();
        let cmd = p
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let (bucket, protected) = classify(&name, &cmd, cfg);
        procs.insert(
            pid.as_u32(),
            ProcInfo {
                pid: pid.as_u32(),
                ppid: p.parent().map(|pp| pp.as_u32()),
                name,
                cmd,
                start_time: p.start_time(),
                cpu_pct: p.cpu_usage(),
                rss_bytes: p.memory(),
                bucket,
                protected,
            },
        );
    }

    let total_mem = sys.total_memory();
    let avail_mem = sys.available_memory();
    let swap_total = sys.total_swap();
    let swap_used = sys.used_swap();
    let cpu = sys.global_cpu_usage();
    let avail_frac = if total_mem > 0 { avail_mem as f64 / total_mem as f64 } else { 1.0 };
    let swap_frac = if swap_total > 0 { swap_used as f64 / swap_total as f64 } else { 0.0 };
    let level = if avail_frac < 0.05 || swap_frac > 0.9 {
        "red"
    } else if avail_frac < 0.15 || swap_frac > 0.5 || cpu > 90.0 {
        "yellow"
    } else {
        "green"
    };

    let pressure = Pressure {
        total_mem_bytes: total_mem,
        avail_mem_bytes: avail_mem,
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_used,
        cpu_pct: cpu,
        level: level.to_string(),
    };
    (procs, pressure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn browsers_are_protected() {
        let (bucket, protected) = classify("chrome.exe", "chrome.exe", &cfg());
        assert_eq!(bucket, Bucket::Browsers);
        assert!(protected);
    }

    #[test]
    fn terminals_are_protected() {
        let (bucket, protected) = classify("WindowsTerminal.exe", "wt", &cfg());
        assert_eq!(bucket, Bucket::Terminals);
        assert!(protected);
    }

    #[test]
    fn agents_are_classified_but_not_protected() {
        let (bucket, protected) = classify("claude.exe", "claude", &cfg());
        assert_eq!(bucket, Bucket::Agents);
        assert!(!protected);

        let (bucket, _) = classify("lean-ctx.exe", "lean-ctx serve", &cfg());
        assert_eq!(bucket, Bucket::Agents);
    }

    #[test]
    fn mcp_in_cmdline_classifies_as_agent_even_for_generic_exe() {
        let (bucket, _) = classify("node.exe", "node dist/mcp-server.js --stdio", &cfg());
        assert_eq!(bucket, Bucket::Agents);
    }

    #[test]
    fn build_tools_bucket() {
        let (bucket, protected) = classify("cargo.exe", "cargo build", &cfg());
        assert_eq!(bucket, Bucket::Build);
        assert!(!protected);
    }

    #[test]
    fn unknown_is_other_and_unprotected() {
        let (bucket, protected) = classify("randomthing.exe", "randomthing", &cfg());
        assert_eq!(bucket, Bucket::Other);
        assert!(!protected);
    }

    #[test]
    fn browser_with_agent_cmdline_stays_protected() {
        // Regression: agent pattern in the cmdline reclassifies the bucket,
        // but a browser executable must remain protected regardless.
        let (bucket, protected) =
            classify("chrome.exe", "chrome --load-extension mcp-helper", &cfg());
        assert_eq!(bucket, Bucket::Agents);
        assert!(protected);
    }

    #[test]
    fn user_protect_pattern_wins_over_agent_bucket() {
        let mut cfg = cfg();
        cfg.protect_patterns.push("my-precious".into());
        let (bucket, protected) = classify("my-precious-agent.exe", "mcp thing", &cfg);
        assert_eq!(bucket, Bucket::Agents);
        assert!(protected);
    }
}
