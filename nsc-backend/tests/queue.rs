use nsc_backend::queue::{Action::*, *};

fn it(key: &str, action: Action) -> Item<()> {
    Item { key: key.into(), action, payload: () }
}

fn keys(b: &Batch<()>) -> Vec<&str> {
    b.items.iter().map(|i| i.key.as_str()).collect()
}

#[test]
fn dedupes_queued_and_running() {
    let mut q = Queue::new();
    assert!(q.enqueue(it("a", Install)));
    assert!(!q.enqueue(it("a", Install)));
    assert_eq!(q.status("a", Install), Some(Status::Queued));
    q.start();
    assert_eq!(q.status("a", Install), Some(Status::Running));
    assert!(!q.enqueue(it("a", Install)));
    assert!(q.enqueue(it("a", Remove)));
    assert_eq!(q.key_status("a"), Some((Install, Status::Running)));
    q.complete(BatchResult::Done);
    assert_eq!(q.key_status("a"), Some((Remove, Status::Queued)));
    assert_eq!(q.status("a", Install), None);
}

#[test]
fn batches_installs_then_removes_in_queue_order() {
    let mut q = Queue::new();
    for (k, a) in [("a", Install), ("x", Remove), ("b", Install), ("y", Remove), ("c", Install)] {
        q.enqueue(it(k, a));
    }
    let b = q.start().unwrap();
    assert_eq!((b.action, keys(&b), b.retry), (Install, vec!["a", "b", "c"], false));
    assert_eq!(q.waiting(), 2);
    q.complete(BatchResult::Done);
    let b = q.start().unwrap();
    assert_eq!((b.action, keys(&b)), (Remove, vec!["x", "y"]));
    q.complete(BatchResult::Done);
    assert!(q.start().is_none());
}

#[test]
fn a_package_is_not_moved_ahead_of_its_opposite() {
    let mut q = Queue::new();
    for (k, a) in [("a", Install), ("a", Remove), ("b", Install)] {
        q.enqueue(it(k, a));
    }
    let b = q.start().unwrap();
    assert_eq!(keys(&b), ["a", "b"]);
    q.complete(BatchResult::Done);
    // Install "a" again while its remove is still waiting.
    assert!(q.enqueue(it("a", Install)));
    let b = q.start().unwrap();
    assert_eq!((b.action, keys(&b)), (Remove, vec!["a"]));
    q.complete(BatchResult::Done);
    assert_eq!(keys(&q.start().unwrap()), ["a"]);
}

#[test]
fn an_install_behind_its_remove_waits_for_it() {
    let mut q = Queue::new();
    for (k, a) in [("b", Remove), ("a", Remove), ("a", Install), ("c", Remove)] {
        q.enqueue(it(k, a));
    }
    let b = q.start().unwrap();
    assert_eq!(keys(&b), ["b", "a", "c"]);
    q.complete(BatchResult::Done);
    assert_eq!(keys(&q.start().unwrap()), ["a"]);
}

#[test]
fn failed_batch_is_retried_per_package_before_newer_items() {
    let mut q = Queue::new();
    for k in ["a", "b", "c"] {
        q.enqueue(it(k, Install));
    }
    q.start();
    q.enqueue(it("d", Install));
    assert!(q.complete(BatchResult::Failed("boom".into())).is_empty());
    assert_eq!(q.status("b", Install), Some(Status::Queued));

    let mut seen = vec![];
    let mut results = vec![];
    while let Some(b) = q.start() {
        seen.push(keys(&b).join(","));
        let r = if b.items[0].key == "b" {
            BatchResult::Failed("b is broken".into())
        } else {
            BatchResult::Done
        };
        results.extend(q.complete(r));
    }
    assert_eq!(seen, ["a", "b", "c", "d"]);
    let res: Vec<_> = results.iter().map(|o| (o.item.key.as_str(), o.result.clone())).collect();
    assert_eq!(
        res,
        [("a", Ok(())), ("b", Err("b is broken".into())), ("c", Ok(())), ("d", Ok(()))]
    );
}

#[test]
fn single_failure_reports_directly_and_queue_continues() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("x", Remove));
    q.start();
    let out = q.complete(BatchResult::Failed("nope".into()));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].result, Err("nope".into()));
    assert_eq!(keys(&q.start().unwrap()), ["x"]);
}

#[test]
fn cancelled_batch_is_not_retried() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    q.start();
    let out = q.complete(BatchResult::Cancelled);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|o| o.result.is_err()));
    assert!(q.start().is_none());
}

#[test]
fn cancel_only_removes_queued_items() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    q.start();
    q.enqueue(it("c", Install));
    assert!(q.cancel_queued("a").is_empty());
    assert_eq!(q.cancel_queued("c").len(), 1);
    assert_eq!(q.status("c", Install), None);
    assert_eq!(q.status("a", Install), Some(Status::Running));
}

#[test]
fn only_one_caller_claims_the_runner() {
    let mut q: Queue<()> = Queue::new();
    assert!(q.claim_runner());
    assert!(!q.claim_runner());
    assert!(q.start().is_none());
    assert!(q.claim_runner());
}
