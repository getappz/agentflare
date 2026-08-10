//! Resource-aware sizing for `work_max_concurrency` — ported from
//! codegraph's `ResolverPool.resolvePoolSize`/`memory-budget.ts`
//! (`.refs/codegraph/src/resolution/`): a CPU term and a memory term, the
//! smaller wins. CPU alone over-subscribes a memory-constrained box (each
//! concurrency slot runs a full headless coding-agent process, not a
//! thread); memory alone ignores that the box also has a core budget.
//!
//! The memory term needs an honest reading of *available* memory, which
//! plain host-wide free memory isn't: on Linux it's blind to a container's
//! cgroup limit, and on macOS `free_count` alone undercounts because macOS
//! deliberately keeps RAM full of reclaimable cache. Each platform gets its
//! own accurate reading below instead of one generic host-wide number.

use std::fs;

/// Conservative per-work-item memory footprint. Each concurrency slot runs
/// a full headless coding-agent CLI (its own process tree — model context,
/// plus tool subprocesses like builds/tests/git), not a lightweight worker
/// thread, so this is deliberately a floor: undercounting starves the box,
/// overcounting just under-utilizes it.
const PER_WORKER_BYTES: u64 = 512 * 1024 * 1024;

/// Fraction of measured memory headroom to spend on workers, leaving the
/// rest for the daemon itself, the dashboard server, and everything else on
/// the box — the same 70/30 split codegraph's resolver-pool uses.
const MEMORY_BUDGET_FRACTION: f64 = 0.7;

/// Ceiling on the CPU term so a huge box doesn't default to dozens of
/// concurrent coding-agent processes; matches codegraph's own long-standing
/// cap for the same reasoning (each worker has real per-worker cost, not
/// just a cheap thread).
const MAX_CPU_WORKERS: usize = 6;

/// No platform-specific memory reading available: don't let an absent
/// measurement wedge concurrency down to the floor of 1, fall back to
/// CPU-only sizing instead (the memory term never constrains).
#[cfg(not(target_os = "linux"))]
const UNMEASURED_MEMORY_SENTINEL: u64 = u64::MAX;

/// Pure sizing decision — CPU term and memory term from injected inputs,
/// smaller wins. Unlike the resolver-pool this ports from (which returns
/// `None` below a threshold so callers fall back to a sequential path),
/// agentflare has no such fallback: a resource-starved box still needs to
/// make forward progress on its work queue, so this floors at 1 rather than
/// disabling the pool.
pub fn resolve_pool_size(available_parallelism: usize, memory_budget_bytes: u64) -> usize {
    let cpu_term = available_parallelism
        .saturating_sub(1)
        .clamp(1, MAX_CPU_WORKERS);
    let spendable = (memory_budget_bytes as f64 * MEMORY_BUDGET_FRACTION) as u64;
    let mem_term = (spendable / PER_WORKER_BYTES).max(1) as usize;
    cpu_term.min(mem_term)
}

/// Parses a cgroup numeric value file's content: `None` for `"max"`
/// (unlimited) or anything unparseable.
fn parse_cgroup_bytes(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw == "max" {
        return None;
    }
    raw.parse::<u64>().ok()
}

fn read_cgroup_bytes(path: &str) -> Option<u64> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| parse_cgroup_bytes(&s))
}

/// `inactive_file` bytes from a cgroup `memory.stat` file — reclaimable
/// page cache the kernel evicts under pressure before touching a worker's
/// real memory, so it's credited back rather than counted as used.
fn parse_inactive_file(stat: &str) -> u64 {
    stat.lines()
        .find_map(|line| line.strip_prefix("inactive_file "))
        .and_then(|n| n.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn read_inactive_file(path: &str) -> u64 {
    fs::read_to_string(path)
        .map(|s| parse_inactive_file(&s))
        .unwrap_or(0)
}

/// Available headroom under the cgroup memory limit (v2, then v1), or
/// `None` when uncontained (no limit or unreadable).
#[cfg(target_os = "linux")]
fn cgroup_memory_available() -> Option<u64> {
    if let Some(max) = read_cgroup_bytes("/sys/fs/cgroup/memory.max") {
        let current = read_cgroup_bytes("/sys/fs/cgroup/memory.current").unwrap_or(0);
        let reclaimable = read_inactive_file("/sys/fs/cgroup/memory.stat");
        return Some(max.saturating_sub(current.saturating_sub(reclaimable)));
    }
    // v1 reports "no limit" as a huge sentinel; treat anything at or beyond
    // 2^60 bytes as uncontained.
    if let Some(limit) = read_cgroup_bytes("/sys/fs/cgroup/memory/memory.limit_in_bytes")
        && limit < (1u64 << 60)
    {
        let usage = read_cgroup_bytes("/sys/fs/cgroup/memory/memory.usage_in_bytes").unwrap_or(0);
        let reclaimable = read_inactive_file("/sys/fs/cgroup/memory/memory.stat");
        return Some(limit.saturating_sub(usage.saturating_sub(reclaimable)));
    }
    None
}

/// `MemAvailable` from `/proc/meminfo` — the kernel's own "safe to use
/// without swapping" estimate, unlike raw `MemFree` which excludes
/// reclaimable cache.
#[cfg(target_os = "linux")]
fn parse_mem_available(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

#[cfg(target_os = "linux")]
pub fn memory_budget_bytes() -> u64 {
    let free = fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| parse_mem_available(&s))
        .unwrap_or(0);
    match cgroup_memory_available() {
        Some(cgroup) => free.min(cgroup),
        None => free,
    }
}

/// Reads one `vm_stat` page-count field (e.g. `"Pages free"`), tolerant of
/// the trailing `.` and variable spacing `vm_stat` uses.
#[cfg(target_os = "macos")]
fn parse_vm_stat_field(output: &str, label: &str) -> u64 {
    output
        .lines()
        .find(|l| l.trim_start().starts_with(label))
        .and_then(|l| l.split(':').nth(1))
        .map(|v| v.trim().trim_end_matches('.'))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Reclaimable-inclusive available memory from `vm_stat` output: free +
/// inactive + speculative + purgeable pages — what Activity Monitor calls
/// "available", the same reclaimable-inclusive convention the Linux
/// `MemAvailable` reading uses.
#[cfg(target_os = "macos")]
fn parse_vm_stat(output: &str) -> Option<u64> {
    let page_size = output
        .find("page size of ")
        .and_then(|idx| output[idx + "page size of ".len()..].split_whitespace().next())
        .and_then(|tok| tok.parse::<u64>().ok())
        .unwrap_or(4096);
    let pages = parse_vm_stat_field(output, "Pages free")
        + parse_vm_stat_field(output, "Pages inactive")
        + parse_vm_stat_field(output, "Pages speculative")
        + parse_vm_stat_field(output, "Pages purgeable");
    let bytes = pages * page_size;
    (bytes > 0).then_some(bytes)
}

#[cfg(target_os = "macos")]
pub fn memory_budget_bytes() -> u64 {
    std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| parse_vm_stat(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or(UNMEASURED_MEMORY_SENTINEL)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn memory_budget_bytes() -> u64 {
    UNMEASURED_MEMORY_SENTINEL
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn big_dev_box_is_cpu_capped_at_six() {
        assert_eq!(resolve_pool_size(8, 16 * GB), 6);
        assert_eq!(resolve_pool_size(11, 16 * GB), 6);
    }

    #[test]
    fn true_two_core_box_still_gets_one_worker_not_zero() {
        assert_eq!(resolve_pool_size(2, 16 * GB), 1);
        assert_eq!(resolve_pool_size(1, 16 * GB), 1);
    }

    #[test]
    fn scarce_memory_shrinks_below_the_cpu_cap() {
        // 1GB budget * 0.7 / 512MB per worker = 1 (floor).
        assert_eq!(resolve_pool_size(8, 1 * GB), 1);
        // 2GB budget * 0.7 / 512MB per worker = 2 (1.4GB / 512MB).
        assert_eq!(resolve_pool_size(8, 2 * GB), 2);
    }

    #[test]
    fn memory_never_disables_the_pool_entirely() {
        assert_eq!(resolve_pool_size(8, 0), 1);
    }

    #[test]
    fn parse_cgroup_bytes_handles_max_and_numeric() {
        assert_eq!(parse_cgroup_bytes("max\n"), None);
        assert_eq!(parse_cgroup_bytes("123456\n"), Some(123456));
        assert_eq!(parse_cgroup_bytes("garbage"), None);
    }

    #[test]
    fn parse_inactive_file_finds_the_field_among_others() {
        let stat = "anon 100\ninactive_file 5242880\nactive_file 200\n";
        assert_eq!(parse_inactive_file(stat), 5242880);
        assert_eq!(parse_inactive_file("anon 100\n"), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parse_mem_available_reads_kb_field_as_bytes() {
        let meminfo = "MemTotal:       16000000 kB\nMemFree:         1000000 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(parse_mem_available(meminfo), Some(4_000_000 * 1024));
        assert_eq!(parse_mem_available("MemTotal: 1 kB\n"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_vm_stat_sums_reclaimable_pages() {
        let out = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
Pages free:                               100.\n\
Pages active:                             999.\n\
Pages inactive:                            50.\n\
Pages speculative:                         25.\n\
Pages purgeable:                           10.\n";
        // (100 + 50 + 25 + 10) * 16384
        assert_eq!(parse_vm_stat(out), Some(185 * 16384));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_vm_stat_none_when_unparseable() {
        assert_eq!(parse_vm_stat("garbage output"), None);
    }

    #[test]
    fn memory_budget_bytes_is_positive_and_finite_on_this_platform() {
        let b = memory_budget_bytes();
        assert!(b > 0);
    }
}
