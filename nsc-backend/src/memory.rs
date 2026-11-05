//! Memory gate for starting install jobs.
//!
//! Desktop containers set `memory.oom.group`, so an OOM kill takes the whole
//! desktop down. A job must therefore wait until the cgroup this process
//! lives in has room for it. Host memory says nothing about that limit.

use std::fs;
use std::path::Path;

/// Free memory required before another job starts alongside a running one.
pub const MIN_FREE: u64 = 1 << 30;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Bytes the cgroup can still use before its limit, or the tightest limit of
/// its parents. `None` when no limit applies or none can be read.
pub fn headroom() -> Option<u64> {
    let own = fs::read_to_string("/proc/self/cgroup").ok()?;
    headroom_in(Path::new(CGROUP_ROOT), &own)
}

/// True when another job may start. Unknown limits admit.
pub fn has_room() -> bool {
    headroom().map_or(true, |free| free >= MIN_FREE)
}

/// `root` is the cgroup mount, `proc_cgroup` the content of `/proc/self/cgroup`.
pub fn headroom_in(root: &Path, proc_cgroup: &str) -> Option<u64> {
    // cgroup v2 is the line `0::<path>`.
    let rel = proc_cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    let mut dir = root.join(rel.trim_start_matches('/'));
    let mut tightest: Option<u64> = None;
    loop {
        if let Some(free) = level(&dir) {
            tightest = Some(tightest.map_or(free, |t| t.min(free)));
        }
        if dir == root || !dir.pop() || !dir.starts_with(root) {
            return tightest;
        }
    }
}

/// Headroom at one cgroup; `None` when it has no numeric `memory.max`.
fn level(dir: &Path) -> Option<u64> {
    let max: u64 = fs::read_to_string(dir.join("memory.max")).ok()?.trim().parse().ok()?;
    let current: u64 = fs::read_to_string(dir.join("memory.current")).ok()?.trim().parse().ok()?;
    // Page cache the kernel can drop is not pressure; the kernel's own working set leaves it out too.
    let inactive = fs::read_to_string(dir.join("memory.stat"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("inactive_file ")?.trim().parse::<u64>().ok())
        })
        .unwrap_or(0);
    Some(max.saturating_sub(current.saturating_sub(inactive)))
}
