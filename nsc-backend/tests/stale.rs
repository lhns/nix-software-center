use nsc_backend::sources::{self, stale, STALE_AFTER};
use std::time::Duration;

// One test: it sets the process-wide cache directory.
#[test]
fn a_failed_fetch_is_not_retried_at_every_startup() {
    let cache = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CACHE_HOME", cache.path());
    assert!(stale("src", STALE_AFTER), "never fetched");

    let err = sources::error_file("src");
    std::fs::create_dir_all(err.parent().unwrap()).unwrap();
    std::fs::write(&err, "error: build failed").unwrap();
    assert!(!stale("src", STALE_AFTER), "failed just now: wait for the next day");
    assert!(stale("src", Duration::ZERO), "retry once the error is older than the limit");
}
