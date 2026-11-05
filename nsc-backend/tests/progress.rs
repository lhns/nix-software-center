use nsc_backend::progress::{parse_line, Record, Tree};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn feed(tree: &mut Tree, stream: &str) {
    for l in stream.lines() {
        if let Some(r) = parse_line(l) {
            tree.apply(&r);
        }
    }
}

#[test]
fn parses_record_kinds_and_prefix() {
    let start = r#"{"action":"start","id":5,"level":3,"parent":2,"text":"building","type":105,"fields":["a",1]}"#;
    let Some(Record::Start { id, parent, kind, text, fields }) = parse_line(start) else {
        panic!()
    };
    assert_eq!((id, parent, kind, text.as_str()), (5, 2, 105, "building"));
    assert_eq!(fields.len(), 2);
    assert_eq!(parse_line(&format!("@nix {start}")), parse_line(start));
    assert_eq!(parse_line(r#"{"action":"stop","id":5}"#), Some(Record::Stop { id: 5 }));
    assert!(matches!(
        parse_line(r#"{"action":"result","id":5,"type":105,"fields":[1,2,0,0]}"#),
        Some(Record::Result { id: 5, kind: 105, .. })
    ));
    assert!(matches!(parse_line(r#"{"action":"msg","level":1,"msg":"hi"}"#), Some(Record::Msg { .. })));
    assert_eq!(parse_line("warning: something"), None);
    assert_eq!(parse_line(r#"{"action":"unknown"}"#), None);
    assert_eq!(parse_line("{broken"), None);
}

#[test]
fn recorded_profile_add_with_local_build() {
    let stream = fixture("recorded-2.34.9-profile-add.jsonl");
    let mut tree = Tree::default();
    let mut statuses = Vec::new();
    for l in stream.lines() {
        if let Some(r) = parse_line(l) {
            tree.apply(&r);
            if matches!(r, Record::Start { .. }) {
                statuses.push(tree.summary().status);
            }
        }
    }
    assert!(statuses.contains(&Some(
        "evaluating derivation 'path:/work/flake#packages.x86_64-linux.default'".into()
    )));
    assert!(statuses
        .iter()
        .any(|s| s.as_deref().is_some_and(|s| s.starts_with("building '/nix/store/"))));

    let s = tree.summary();
    assert_eq!(s.builds, (1, 2));
    assert_eq!(s.downloads, (0, 0));
    assert_eq!(s.bytes, (54, 54));
    assert_eq!(s.fraction, Some(0.5));
    assert_eq!((s.started, s.stopped), (11, 11));
    assert_eq!(s.status, None);
}

fn start(id: u64, parent: u64, kind: u64, text: &str) -> String {
    format!(r#"{{"action":"start","id":{id},"parent":{parent},"type":{kind},"text":"{text}","level":3}}"#)
}

fn progress(id: u64, d: u64, e: u64) -> String {
    format!(r#"{{"action":"result","id":{id},"type":105,"fields":[{d},{e},0,0]}}"#)
}

fn stop(id: u64) -> String {
    format!(r#"{{"action":"stop","id":{id}}}"#)
}

#[test]
fn interleaved_processes_do_not_collide() {
    // Same low counter value in two pids.
    let (a, b) = ((100u64 << 32) + 1, (200u64 << 32) + 1);
    let stream = [
        start(a, 0, 104, ""),
        start(b, 0, 104, ""),
        progress(a, 1, 4),
        progress(b, 2, 3),
        start(a + 1, a, 105, "building a"),
        start(b + 1, b, 105, "building b"),
        stop(a + 1),
        progress(a, 2, 4),
    ]
    .join("\n");
    let mut tree = Tree::default();
    feed(&mut tree, &stream);
    let s = tree.summary();
    assert_eq!(s.builds, (4, 7));
    assert_eq!(s.status.as_deref(), Some("building b"));
    assert_eq!((s.started, s.stopped), (4, 1));
    assert!((s.fraction.unwrap() - 4.0 / 7.0).abs() < 1e-9);
}

#[test]
fn downloads_and_bytes() {
    let stream = [
        start(1, 0, 103, ""),
        progress(1, 1, 3),
        start(2, 1, 108, "fetching hello"),
        start(3, 2, 101, "downloading hello"),
        progress(3, 500, 1000),
        start(4, 0, 101, "downloading other"),
        progress(4, 10, 20),
    ]
    .join("\n");
    let mut tree = Tree::default();
    feed(&mut tree, &stream);
    let s = tree.summary();
    assert_eq!(s.downloads, (1, 3));
    assert_eq!(s.bytes, (510, 1020));
    assert_eq!(s.builds, (0, 0));
    assert_eq!(s.status.as_deref(), Some("downloading other"));
    assert!((s.fraction.unwrap() - 1.0 / 3.0).abs() < 1e-9);
}

#[test]
fn nested_builds_are_not_top_level() {
    let stream = [start(1, 0, 104, ""), start(2, 1, 104, ""), progress(2, 5, 5), progress(1, 1, 2)].join("\n");
    let mut tree = Tree::default();
    feed(&mut tree, &stream);
    assert_eq!(tree.summary().builds, (1, 2));
}

#[test]
fn label_says_evaluating_until_totals_arrive() {
    use nsc_backend::progress::Summary;
    let mut s = Summary::default();
    assert_eq!(s.label(), "Evaluating…");
    s.status = Some("evaluating 'x'".into());
    assert_eq!(s.label(), "Evaluating… — evaluating 'x'");
    s.fraction = Some(0.5);
    s.builds = (1, 2);
    s.downloads = (0, 3);
    assert_eq!(s.label(), "evaluating 'x' — builds 1/2 — downloads 0/3");
}

#[test]
fn applying_once_every_activity_has_stopped() {
    let mut tree = Tree::default();
    let applying = |t: &Tree| t.summary().applying;
    assert!(!applying(&tree), "nothing reported yet is still evaluating");
    feed(&mut tree, &start(1, 0, 0, "evaluating"));
    assert!(!applying(&tree));
    feed(&mut tree, &start(2, 0, 104, "builds"));
    feed(&mut tree, &progress(2, 0, 1));
    feed(&mut tree, &stop(1));
    assert!(!applying(&tree), "a build is still running");
    feed(&mut tree, &progress(2, 1, 1));
    feed(&mut tree, &stop(2));
    let s = tree.summary();
    assert!(s.applying);
    assert_eq!(s.label(), "Applying…");
    feed(&mut tree, &start(3, 0, 0, "running hook"));
    assert!(!applying(&tree), "new activity ends the state");
    assert!(tree.summary().label().starts_with("running hook"));
}

#[test]
fn recorded_run_ends_applying() {
    let mut tree = Tree::default();
    feed(&mut tree, &fixture("recorded-2.34.9-profile-add.jsonl"));
    assert!(tree.summary().applying);
}
