use nsc_backend::memory::{headroom_in, MIN_FREE};
use std::fs;
use std::path::Path;

fn cgroup(root: &Path, rel: &str, max: &str, current: u64, inactive: u64) {
    let dir = root.join(rel);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("memory.max"), format!("{max}\n")).unwrap();
    fs::write(dir.join("memory.current"), format!("{current}\n")).unwrap();
    fs::write(dir.join("memory.stat"), format!("anon 1\ninactive_file {inactive}\nfile 9\n")).unwrap();
}

#[test]
fn headroom_is_limit_minus_working_set() {
    let t = tempfile::tempdir().unwrap();
    cgroup(t.path(), "pod/app", "4294967296", 3 << 30, 1 << 30);
    assert_eq!(headroom_in(t.path(), "0::/pod/app\n"), Some(2 << 30));
}

#[test]
fn the_tightest_parent_limit_counts() {
    let t = tempfile::tempdir().unwrap();
    cgroup(t.path(), "pod", "2147483648", 2147483648 - MIN_FREE / 2, 0);
    cgroup(t.path(), "pod/app", "max", 1, 0);
    assert_eq!(headroom_in(t.path(), "0::/pod/app"), Some(MIN_FREE / 2));
}

#[test]
fn no_limit_or_no_cgroup_is_unknown() {
    let t = tempfile::tempdir().unwrap();
    cgroup(t.path(), "app", "max", 5, 0);
    assert_eq!(headroom_in(t.path(), "0::/app"), None);
    assert_eq!(headroom_in(t.path(), "0::/missing"), None);
    assert_eq!(headroom_in(t.path(), "1:memory:/x"), None);
}
