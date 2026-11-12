use nsc_backend::queue::{Action::*, *};

fn it(key: &str, action: Action) -> Item<()> {
    Item { id: 0, key: key.into(), action, payload: () }
}

fn key(b: &Batch<()>) -> &str {
    &b.item.key
}

fn start(q: &mut Queue<()>) -> Option<Batch<()>> {
    q.start(MAX_SLOTS, true).batch()
}

#[test]
fn dedupes_queued_and_running() {
    let mut q = Queue::new();
    assert!(q.enqueue(it("a", Install)));
    assert!(!q.enqueue(it("a", Install)));
    assert_eq!(q.status("a", Install), Some(Status::Queued));
    let a = start(&mut q).unwrap();
    assert_eq!(q.status("a", Install), Some(Status::Running));
    assert!(!q.enqueue(it("a", Install)));
    assert!(q.enqueue(it("a", Remove)));
    assert_eq!(q.key_status("a"), Some((Install, Status::Running)));
    q.complete(a.id(), BatchResult::Done);
    assert_eq!(q.key_status("a"), Some((Remove, Status::Queued)));
    assert_eq!(q.status("a", Install), None);
}

#[test]
fn four_run_at_once_and_the_fifth_waits_for_a_slot() {
    let mut q = Queue::new();
    for k in ["a", "b", "c", "d", "e"] {
        q.enqueue(it(k, Install));
    }
    let run: Vec<_> = (0..4).map(|_| start(&mut q).unwrap()).collect();
    assert_eq!(run.iter().map(key).collect::<Vec<_>>(), ["a", "b", "c", "d"]);
    assert_eq!(q.start(MAX_SLOTS, true), Start::Wait);
    // The slot limit is the caller's: a stock Nix passes 1.
    q.complete(run[1].id(), BatchResult::Done);
    assert_eq!(key(&start(&mut q).unwrap()), "e");
    assert_eq!(q.running().len(), 4);
}

#[test]
fn stock_nix_runs_one_job_at_a_time() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    let a = q.start(1, true).batch().unwrap();
    assert_eq!(q.start(1, true), Start::Wait);
    q.complete(a.id(), BatchResult::Done);
    assert_eq!(key(&q.start(1, true).batch().unwrap()), "b");
}

#[test]
fn one_failure_or_cancel_leaves_the_others_running() {
    let mut q = Queue::new();
    for k in ["a", "b", "c", "d"] {
        q.enqueue(it(k, Install));
    }
    let run: Vec<_> = (0..4).map(|_| start(&mut q).unwrap()).collect();
    let out = q.complete(run[1].id(), BatchResult::Failed("b is broken".into())).unwrap();
    assert_eq!((out.item.key.as_str(), out.result), ("b", Err("b is broken".into())));
    let out = q.complete(run[2].id(), BatchResult::Cancelled).unwrap();
    assert!(out.cancelled);
    assert_eq!(q.running().iter().map(|b| key(b)).collect::<Vec<_>>(), ["a", "d"]);
    assert!(q.complete(run[1].id(), BatchResult::Done).is_none(), "already ended");
}

#[test]
fn the_same_key_stays_serial() {
    let mut q = Queue::new();
    for (k, a) in [("a", Install), ("a", Remove), ("b", Install)] {
        q.enqueue(it(k, a));
    }
    let a = start(&mut q).unwrap();
    // The remove of "a" waits for the install; "b" does not.
    assert_eq!(key(&start(&mut q).unwrap()), "b");
    assert_eq!(q.start(MAX_SLOTS, true), Start::Wait);
    q.complete(a.id(), BatchResult::Done);
    let r = start(&mut q).unwrap();
    assert_eq!((r.action, key(&r)), (Remove, "a"));
}

#[test]
fn an_install_behind_its_waiting_remove_does_not_jump_it() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    let a = start(&mut q).unwrap();
    q.enqueue(it("a", Remove));
    q.complete(a.id(), BatchResult::Done);
    assert!(q.enqueue(it("a", Install)));
    assert_eq!(start(&mut q).unwrap().action, Remove);
    assert_eq!(q.start(MAX_SLOTS, true), Start::Wait);
}

#[test]
fn exclusive_waits_for_running_jobs_and_blocks_later_ones() {
    let mut q = Queue::new();
    for (k, a) in [("a", Install), ("u", Exclusive), ("b", Install)] {
        q.enqueue(it(k, a));
    }
    let a = start(&mut q).unwrap();
    assert_eq!(q.start(MAX_SLOTS, true), Start::Wait, "update waits for a, b waits for the update");
    q.complete(a.id(), BatchResult::Done);
    let u = start(&mut q).unwrap();
    assert_eq!(u.action, Exclusive);
    assert_eq!(q.start(MAX_SLOTS, true), Start::Wait, "nothing runs beside an update");
    q.complete(u.id(), BatchResult::Done);
    assert_eq!(key(&start(&mut q).unwrap()), "b");
}

#[test]
fn memory_gate_queues_instead_of_starting() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    q.enqueue(it("b", Install));
    q.enqueue(it("c", Install));
    // Nothing else runs, so the gate cannot help by waiting.
    let a = q.start(MAX_SLOTS, false).batch().unwrap();
    assert_eq!(q.start(MAX_SLOTS, false), Start::Wait);
    assert_eq!(q.waiting(), 2);
    assert_eq!(key(&q.start(MAX_SLOTS, true).batch().unwrap()), "b");
    assert_eq!(q.start(MAX_SLOTS, false), Start::Wait);
    q.complete(a.id(), BatchResult::Done);
    assert_eq!(key(&q.start(MAX_SLOTS, true).batch().unwrap()), "c");
}

#[test]
fn cancel_only_removes_queued_items() {
    let mut q = Queue::new();
    q.enqueue(it("a", Install));
    start(&mut q);
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
    assert_eq!(q.start(MAX_SLOTS, true), Start::Idle);
    assert!(q.claim_runner());
}

#[test]
fn a_run_shares_slots_with_installs_and_is_no_barrier() {
    let mut q = Queue::default();
    assert!(q.enqueue(it("run/a", Run)));
    assert!(q.enqueue(it("a", Install)));
    assert!(q.enqueue(it("run/b", Run)));
    assert!(!q.enqueue(it("run/a", Run)));
    let ran: Vec<_> = std::iter::from_fn(|| start(&mut q)).map(|b| (key(&b).to_string(), b.action)).collect();
    assert_eq!(ran, [("run/a".into(), Run), ("a".into(), Install), ("run/b".into(), Run)]);
    // One slot: a run waits for a running install like any other job.
    let mut q = Queue::default();
    q.enqueue(it("a", Install));
    q.enqueue(it("run/a", Run));
    assert!(q.start(1, true).batch().is_some());
    assert!(matches!(q.start(1, true), Start::Wait));
}
