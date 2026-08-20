//! cgroup-aware CPU count detection (#44).
//!
//! `num_cpus::get()` reads the process's CPU affinity mask
//! (`sched_getaffinity` on Linux), which reflects how many *cores* the
//! process could theoretically run on - not how much CPU *time* a cgroup
//! CPU quota (`cpu.max` / `cpu.cfs_quota_us`) actually entitles it to. A pod
//! capped at `400m` (0.4 CPU) on a 16-core node still reports 16 from
//! `num_cpus::get()`, so any pool sized off it is roughly 40x
//! oversubscribed relative to what the container can actually execute
//! concurrently before the kernel's CFS bandwidth controller starts
//! throttling it.
//!
//! This reads the quota directly: cgroup v2's unified `cpu.max` first, then
//! cgroup v1's split `cpu.cfs_quota_us` / `cpu.cfs_period_us`, falling back
//! to `num_cpus::get()` when neither is present or parseable (bare metal,
//! most developer machines, or a container runtime not enforcing a CPU
//! quota).
//!
//! Used directly by `src/main.rs` to size the Tokio runtime's worker thread
//! count. `src/config/performance.rs` (rayon/processing pool sizing) is
//! owned by another agent for this change and still calls `num_cpus::get()`
//! directly - see the final report recommending it adopt this same helper.

use std::fs;
use std::path::Path;

const CGROUP_V2_CPU_MAX: &str = "/sys/fs/cgroup/cpu.max";
const CGROUP_V1_QUOTA: &str = "/sys/fs/cgroup/cpu/cpu.cfs_quota_us";
const CGROUP_V1_PERIOD: &str = "/sys/fs/cgroup/cpu/cpu.cfs_period_us";

/// The effective number of CPUs available to this process: the cgroup CPU
/// quota when one is set and enforced (capped at the host's actual core
/// count, in case of a misconfigured/absurd quota), otherwise
/// `num_cpus::get()`. Always returns at least 1.
pub fn effective_cpu_count() -> usize {
    let host_cpus = num_cpus::get().max(1);

    cgroup_cpu_quota(
        Path::new(CGROUP_V2_CPU_MAX),
        Path::new(CGROUP_V1_QUOTA),
        Path::new(CGROUP_V1_PERIOD),
    )
    .map(|quota| quota.clamp(1, host_cpus))
    .unwrap_or(host_cpus)
}

/// Reads and parses whichever cgroup CPU-quota interface is present,
/// preferring v2. Paths are parameterized (rather than hardcoded) so tests
/// can point this at fixture files instead of the real `/sys/fs/cgroup`.
fn cgroup_cpu_quota(v2_path: &Path, v1_quota_path: &Path, v1_period_path: &Path) -> Option<usize> {
    if let Ok(contents) = fs::read_to_string(v2_path)
        && let Some(cpus) = parse_cgroup_v2_cpu_max(&contents)
    {
        return Some(cpus);
    }

    if let (Ok(quota), Ok(period)) = (
        fs::read_to_string(v1_quota_path),
        fs::read_to_string(v1_period_path),
    ) && let Some(cpus) = parse_cgroup_v1_quota(&quota, &period)
    {
        return Some(cpus);
    }

    None
}

/// Parses cgroup v2's `cpu.max` (`"$MAX $PERIOD"`, or `"max $PERIOD"` for
/// no limit) into a whole-CPU count, rounded up so a quota like
/// `"150000 100000"` (1.5 CPUs) reserves capacity for 2 threads rather than
/// truncating down to 1 and leaving half a CPU's worth of quota unused.
fn parse_cgroup_v2_cpu_max(contents: &str) -> Option<usize> {
    let mut fields = contents.split_whitespace();
    let quota = fields.next()?;
    let period: u64 = fields.next()?.parse().ok()?;

    if quota == "max" {
        return None; // No limit set - fall through to num_cpus.
    }

    let quota: u64 = quota.parse().ok()?;
    quota_to_cpu_count(quota, period)
}

/// Parses cgroup v1's split `cpu.cfs_quota_us` / `cpu.cfs_period_us` the
/// same way. `cfs_quota_us == -1` means "no limit" (v1's spelling of v2's
/// `"max"`).
fn parse_cgroup_v1_quota(quota: &str, period: &str) -> Option<usize> {
    let quota: i64 = quota.trim().parse().ok()?;
    let period: u64 = period.trim().parse().ok()?;

    if quota < 0 {
        return None;
    }

    quota_to_cpu_count(quota as u64, period)
}

fn quota_to_cpu_count(quota: u64, period: u64) -> Option<usize> {
    if period == 0 {
        return None;
    }
    Some(quota.div_ceil(period).max(1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn parses_v2_fractional_quota_rounds_up() {
        // 400m (0.4 CPU): 40000us quota / 100000us period.
        assert_eq!(parse_cgroup_v2_cpu_max("40000 100000\n"), Some(1));
    }

    #[test]
    fn parses_v2_multi_cpu_quota_rounds_up() {
        // 2.5 CPUs -> rounds up to 3.
        assert_eq!(parse_cgroup_v2_cpu_max("250000 100000\n"), Some(3));
    }

    #[test]
    fn parses_v2_exact_whole_cpu_quota() {
        assert_eq!(parse_cgroup_v2_cpu_max("200000 100000\n"), Some(2));
    }

    #[test]
    fn v2_max_means_no_limit() {
        assert_eq!(parse_cgroup_v2_cpu_max("max 100000\n"), None);
    }

    #[test]
    fn v2_malformed_contents_is_none() {
        assert_eq!(parse_cgroup_v2_cpu_max("garbage"), None);
        assert_eq!(parse_cgroup_v2_cpu_max(""), None);
    }

    #[test]
    fn parses_v1_quota() {
        assert_eq!(parse_cgroup_v1_quota("40000", "100000"), Some(1));
        assert_eq!(parse_cgroup_v1_quota("250000", "100000"), Some(3));
    }

    #[test]
    fn v1_negative_quota_means_no_limit() {
        assert_eq!(parse_cgroup_v1_quota("-1", "100000"), None);
    }

    /// Unique per-test scratch directory under the OS temp dir, so parallel
    /// `cargo test` runs (this module's tests write real files) don't
    /// collide with each other.
    fn scratch_dir(label: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "emgr-cgroup-test-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn cgroup_cpu_quota_prefers_v2_when_present() {
        let dir = scratch_dir("prefers-v2");
        let v2 = dir.join("cpu.max");
        let v1_quota = dir.join("cpu.cfs_quota_us");
        let v1_period = dir.join("cpu.cfs_period_us");
        std::fs::write(&v2, "200000 100000\n").unwrap();
        std::fs::write(&v1_quota, "40000\n").unwrap();
        std::fs::write(&v1_period, "100000\n").unwrap();

        assert_eq!(cgroup_cpu_quota(&v2, &v1_quota, &v1_period), Some(2));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cgroup_cpu_quota_falls_back_to_v1_when_v2_absent() {
        let dir = scratch_dir("falls-back-v1");
        let v2 = dir.join("does-not-exist");
        let v1_quota = dir.join("cpu.cfs_quota_us");
        let v1_period = dir.join("cpu.cfs_period_us");
        std::fs::write(&v1_quota, "150000\n").unwrap();
        std::fs::write(&v1_period, "100000\n").unwrap();

        assert_eq!(cgroup_cpu_quota(&v2, &v1_quota, &v1_period), Some(2));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cgroup_cpu_quota_none_when_nothing_present() {
        let dir = scratch_dir("nothing-present");
        let v2 = dir.join("cpu.max");
        let v1_quota = dir.join("cpu.cfs_quota_us");
        let v1_period = dir.join("cpu.cfs_period_us");

        assert_eq!(cgroup_cpu_quota(&v2, &v1_quota, &v1_period), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn effective_cpu_count_is_always_at_least_one() {
        assert!(effective_cpu_count() >= 1);
    }
}
