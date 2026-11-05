use nsc_backend::queue::{Action::*, *};

fn it(key: &str, action: Action) -> Item<()> {
    Item { id: 0, key: key.into(), action, payload: () }
}

fn keys(b: &Batch<()>) -> Vec<&str> {
    vec![b.item.key.as_str()]
}

/// Starts the next item and returns the id of the batch that is running last.
fn start(q: &mut Queue<()>) -> Option<Batch<()>> {
    q.start(MAX_SLOTS, true).batch()
}

/// Ends the only running job.
fn complete(q: &mut Queue<()>, r: BatchResult) {
    let id = q.running()[0].id();
    q.complete(id, r);
}

fn id_of(q: &Queue<()>, key: &str) -> u64 {
    q.entries().into_iter().find(|e| e.key == key).unwrap().id
}

#[test]
fn ids_are_assigned_on_enqueue_and_stay_stable() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    let (a, b) = (id_of(&q, "a"), id_of(&q, "b"));
    assert_ne!(a, b);
    start(&mut q);
    assert_eq!(id_of(&q, "a"), a);
    complete(&mut q, BatchResult::Cancelled);
    assert_eq!(id_of(&q, "a"), a);
    assert!(q.retry(a, |_| {}));
    assert_eq!(id_of(&q, "a"), a);
}

#[test]
fn exclusive_runs_alone_and_is_a_barrier() {
    let mut q = Queue::new();
    for (k, a) in [("a", Install), ("x", Exclusive), ("b", Install), ("y", Exclusive)] {
        q.enqueue(it(k, a));
    }
    assert_eq!(keys(&start(&mut q).unwrap()), ["a"]);
    complete(&mut q, BatchResult::Done);
    let b = start(&mut q).unwrap();
    assert_eq!((b.action, keys(&b)), (Exclusive, vec!["x"]));
    complete(&mut q, BatchResult::Done);
    assert_eq!(keys(&start(&mut q).unwrap()), ["b"]);
    complete(&mut q, BatchResult::Done);
    assert_eq!(keys(&start(&mut q).unwrap()), ["y"]);
}

#[test]
fn history_orders_entries_and_keeps_states() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("x", Exclusive));
    q.enqueue(it("c", Remove));
    start(&mut q);
    let id = q.running()[0].id();
    q.log_push(id, "hello");
    complete(&mut q, BatchResult::Done);
    start(&mut q);
    complete(&mut q, BatchResult::Failed("bad".into()));
    start(&mut q);
    let order: Vec<_> = q.entries().into_iter().map(|e| (e.key, e.state)).collect();
    assert_eq!(
        order,
        [
            ("c".to_string(), EntryState::Running),
            ("x".to_string(), EntryState::Failed("bad".into())),
            ("a".to_string(), EntryState::Done),
        ]
    );
    let a = id_of(&q, "a");
    assert_eq!(q.log_since(a, 0).0, ["hello"]);
}

#[test]
fn cancelling_a_queued_entry_records_it_as_cancelled() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    start(&mut q);
    q.enqueue(it("c", Install));
    q.enqueue(it("d", Install));
    let c = id_of(&q, "c");
    assert!(q.cancel_queued_id(c).is_some());
    assert!(q.cancel_queued_id(c).is_none());
    assert_eq!(q.cancel_queued("d").len(), 1);
    let states: Vec<_> = q.entries().into_iter().map(|e| (e.key, e.state)).collect();
    assert!(states.contains(&("c".to_string(), EntryState::Cancelled)));
    assert!(states.contains(&("d".to_string(), EntryState::Cancelled)));
    assert_eq!(q.active(), 2);
}

#[test]
fn retry_requeues_with_log_unless_the_same_work_is_active() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    start(&mut q);
    let id = q.running()[0].id();
    q.log_push(id, "first try");
    complete(&mut q, BatchResult::Failed("boom".into()));
    let a = id_of(&q, "a");
    assert!(q.enqueue(it("a", Install)));
    assert!(!q.retry(a, |_| {}), "same key and action is queued");
    assert!(q.entry_by_id(a).is_some(), "the entry stays");
    start(&mut q);
    complete(&mut q, BatchResult::Done);
    // Done entries are not retried.
    let done = q.entries().into_iter().find(|e| e.state == EntryState::Done).unwrap().id;
    assert!(!q.retry(done, |_| {}));
    assert!(q.retry(a, |_| {}));
    assert_eq!(q.status("a", Install), Some(Status::Queued));
    let (lines, _) = q.log_since(a, 0);
    assert_eq!(lines[0], "first try");
    assert_eq!(lines.len(), 2);
    assert!(lines[1].starts_with("---"));
}

#[test]
fn remove_and_clear_finished() {
    let mut q = Queue::new();
    for k in ["a", "b", "c"] {
        q.enqueue(it(k, Install));
    }
    for _ in 0..3 {
        start(&mut q);
        complete(&mut q, BatchResult::Done);
    }
    let a = id_of(&q, "a");
    assert!(q.remove(a));
    assert!(!q.remove(a));
    assert!(q.log(a).is_none());
    assert_eq!(q.clear_finished(), 2);
    assert!(q.entries().is_empty());
}

#[test]
fn history_cap_drops_the_oldest_non_failed_first() {
    let mut q = Queue::new();
    q.enqueue(it("failed", Install));
    start(&mut q);
    complete(&mut q, BatchResult::Failed("no".into()));
    for n in 0..MAX_FINISHED + 5 {
        q.enqueue(it(&format!("k{n}"), Install));
        start(&mut q);
        complete(&mut q, BatchResult::Done);
    }
    let entries = q.entries();
    assert_eq!(entries.len(), MAX_FINISHED);
    assert!(entries.iter().any(|e| e.key == "failed"));
    assert!(!entries.iter().any(|e| e.key == "k0"));
    assert!(entries.iter().any(|e| e.key == format!("k{}", MAX_FINISHED + 4)));
}

#[test]
fn history_of_only_failures_drops_the_oldest() {
    let mut q = Queue::new();
    for n in 0..MAX_FINISHED + 1 {
        q.enqueue(it(&format!("k{n}"), Install));
        start(&mut q);
        complete(&mut q, BatchResult::Failed("no".into()));
    }
    let entries = q.entries();
    assert_eq!(entries.len(), MAX_FINISHED);
    assert!(!entries.iter().any(|e| e.key == "k0"));
}

#[test]
fn unseen_failures_count_until_marked_seen() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    start(&mut q);
    complete(&mut q, BatchResult::Done);
    q.enqueue(it("b", Install));
    start(&mut q);
    complete(&mut q, BatchResult::Cancelled);
    assert_eq!(q.unseen_failures(), 0);
    q.enqueue(it("c", Install));
    start(&mut q);
    complete(&mut q, BatchResult::Failed("no".into()));
    assert_eq!(q.unseen_failures(), 1);
    q.mark_failures_seen();
    assert_eq!(q.unseen_failures(), 0);
}

#[test]
fn log_lines_go_to_their_own_running_job_and_are_pulled_incrementally() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    let (ba, bb) = (start(&mut q).unwrap(), start(&mut q).unwrap());
    q.log_push(ba.id(), "one");
    q.log_push(bb.id(), "other");
    let (a, b) = (ba.id(), bb.id());
    let (lines, seen) = q.log_since(a, 0);
    assert_eq!((lines, seen), (vec!["one".to_string()], 1));
    q.log_push(a, "two");
    assert_eq!(q.log_since(a, seen), (vec!["two".to_string()], 2));
    assert_eq!(q.log_since(b, 0).0, ["other"]);
    assert_eq!(q.log_since(a, 2).0, Vec::<String>::new());
}

#[test]
fn log_limits_lines_bytes_and_line_length() {
    let mut l = LogBuffer::default();
    l.push(&"x".repeat(MAX_LINE_CHARS * 2));
    assert_eq!(l.bytes(), MAX_LINE_CHARS + 1);
    for n in 0..MAX_LOG_LINES + 10 {
        l.push(&n.to_string());
    }
    assert_eq!(l.len(), MAX_LOG_LINES);
    assert_eq!(l.total(), (MAX_LOG_LINES + 11) as u64);
    // A reader that fell behind gets what is left.
    assert_eq!(l.since(0).0.len(), MAX_LOG_LINES);

    let mut big = LogBuffer::default();
    for _ in 0..2000 {
        big.push(&"y".repeat(900));
    }
    assert!(big.bytes() <= MAX_LOG_BYTES);
    big.trim(FINISHED_LOG_BYTES);
    assert!(big.bytes() <= FINISHED_LOG_BYTES);
    assert!(!big.is_empty());
}

#[test]
fn finishing_trims_the_log_to_a_tail() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    start(&mut q);
    let a = id_of(&q, "a");
    for _ in 0..1000 {
        q.log_push(a, &"z".repeat(900));
    }
    assert!(q.log(a).unwrap().bytes() > FINISHED_LOG_BYTES);
    complete(&mut q, BatchResult::Done);
    assert!(q.log(a).unwrap().bytes() <= FINISHED_LOG_BYTES);
}

#[test]
fn progress_is_kept_per_running_job() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    q.enqueue(it("x", Exclusive));
    let (a, b) = (start(&mut q).unwrap().id(), start(&mut q).unwrap().id());
    q.set_progress(a, Some(0.5), "half".into());
    let e = q.entries();
    assert_eq!(e[0].progress, Some(Progress { fraction: Some(0.5), label: "half".into() }));
    assert_eq!(e[1].progress, Some(Progress::default()));
    assert_eq!((e[2].progress.clone(), e[2].ahead), (None, 2));
    q.complete(b, BatchResult::Done);
    assert!(q.entries().iter().all(|e| e.state != EntryState::Done || e.progress.is_none()));
}
