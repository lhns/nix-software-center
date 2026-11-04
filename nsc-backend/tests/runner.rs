//! Process runner and ops against fake `nix` shell scripts.
#![cfg(unix)]

use nsc_backend::ops::{self, Nix};
use nsc_backend::run::{self, Cancel, Event, Stream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

// Serializes tests: writing an executable while another thread forks can make
// exec fail with ETXTBSY, and some tests change the process environment.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

#[tokio::test]
async fn large_output_on_both_streams_does_not_deadlock() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let s = script(
        dir.path(),
        "big",
        "head -c 3000000 /dev/zero | tr '\\0' a | fold -w 99 >&2\nyes line-out | head -n 200000",
    );
    let (mut out, mut err_bytes) = (0usize, 0usize);
    let status = run::run(s.as_os_str(), &[], &Cancel::new(), |e| {
        if let Event::Line(stream, l) = e {
            match stream {
                Stream::Stdout => out += 1,
                Stream::Stderr => err_bytes += l.len(),
            }
        }
    })
    .await
    .unwrap();
    assert!(status.success());
    assert_eq!(out, 200_000);
    assert_eq!(err_bytes, 3_000_000);
}

#[tokio::test]
async fn invalid_utf8_and_crlf_lines() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let s = script(dir.path(), "bytes", "printf 'a\\377b\\r\\nlast'");
    let mut lines = Vec::new();
    run::run(s.as_os_str(), &[], &Cancel::new(), |e| lines.push(e)).await.unwrap();
    assert_eq!(
        lines,
        [
            Event::Line(Stream::Stdout, "a\u{fffd}b".into()),
            Event::Line(Stream::Stdout, "last".into())
        ]
    );
}

#[tokio::test]
async fn a_grandchild_holding_the_pipe_does_not_hang_the_run() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let s = script(dir.path(), "bg", "echo hi
sleep 30 &
exit 0");
    let start = std::time::Instant::now();
    let mut lines = Vec::new();
    let st = run::run(s.as_os_str(), &[], &Cancel::new(), |e| lines.push(e)).await.unwrap();
    assert!(st.success());
    assert_eq!(lines, [Event::Line(Stream::Stdout, "hi".into())]);
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn exit_codes_map_to_bool() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let nix = Nix::new(script(dir.path(), "nix", "case \"$*\" in *remove*) exit 3;; esac"));
    let c = Cancel::new();
    assert!(ops::upgrade_all(&nix, &c, |_| {}).await.unwrap());
    assert!(!ops::remove(&nix, &["x".into()], &c, |_| {}).await.unwrap());
    // A failed removal stops the combined operation before the upgrade.
    assert!(!ops::remove_then_upgrade_all(&nix, &["x".into()], &c, |_| {}).await.unwrap());
    let missing = Nix::new(dir.path().join("does-not-exist"));
    assert!(ops::upgrade_all(&missing, &c, |_| {}).await.is_err());
}

#[tokio::test]
async fn cancel_sends_sigint_to_the_process_group_once() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    // The inner shell stands in for a helper the program started; it only
    // sees the signal when it goes to the whole group.
    let s = script(
        dir.path(),
        "slow",
        r#"trap 'echo parent-int; sleep 0.2; exit 42' INT
sh -c 'trap "echo child-int > $0; exit 0" INT; echo ready; while :; do sleep 0.05; done' "$1"
echo not-reached"#,
    );
    let mark = dir.path().join("mark");
    let cancel = Cancel::new();
    let mut lines = Vec::new();
    let status = run::run(
        s.as_os_str(),
        &[mark.display().to_string()],
        &cancel,
        |e| {
            if let Event::Line(_, l) = e {
                if l == "ready" {
                    cancel.cancel();
                    cancel.cancel();
                }
                lines.push(l);
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(status.code(), Some(42));
    assert!(cancel.is_cancelled());
    assert_eq!(lines, ["ready", "parent-int"]);
    assert_eq!(std::fs::read_to_string(mark).unwrap().trim(), "child-int");
}

fn have_python() -> bool {
    std::process::Command::new("python3").arg("--version").output().is_ok()
}

const PY_CLIENT: &str = r#"
import os, re, socket
path = re.search(r"json-log-path = (\S+)", os.environ["NIX_CONFIG"]).group(1)
a = socket.socket(socket.AF_UNIX); a.connect(path)
b = socket.socket(socket.AF_UNIX); b.connect(path)
a.sendall(b'{"action":"start","id":4294967297,"level":3,"parent":0,"text":"building a","type":105}\n')
b.sendall(b'{"action":"start","id":8589934593,"level":3,"parent":0,"text":"building b","type":105}\n')
a.sendall(b'{"action":"stop","id":4294967297}\n')
b.sendall(b'{"action":"stop","id":8589934593}\n')
print(path)
"#;

#[tokio::test]
async fn progress_socket_reaches_the_child_and_streams_back() {
    if !have_python() {
        eprintln!("python3 missing, skipping");
        return;
    }
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("client.py"), PY_CLIENT).unwrap();
    let s = script(
        dir.path(),
        "nix",
        "echo \"cfg:$NIX_CONFIG\"\npython3 \"$(dirname \"$0\")/client.py\"",
    );
    std::env::set_var("NIX_CONFIG", "experimental-features = nix-command");
    std::env::set_var("XDG_RUNTIME_DIR", dir.path());

    let mut events = Vec::new();
    let status = run::run_with_progress(s.as_os_str(), &[], &Cancel::new(), |e| events.push(e))
        .await
        .unwrap();
    assert!(status.success());

    let stdout: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Line(Stream::Stdout, l) => Some(l.as_str()),
            _ => None,
        })
        .collect();
    assert!(stdout.iter().any(|l| l.contains("cfg:experimental-features = nix-command")));
    let sock = stdout.last().unwrap();
    assert!(sock.starts_with(dir.path().to_str().unwrap()), "{sock}");
    assert!(!Path::new(sock).exists(), "socket is removed afterwards");

    let progress: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Progress(p) => Some(p),
            _ => None,
        })
        .collect();
    let last = progress.last().expect("progress events");
    assert_eq!((last.started, last.stopped), (2, 2));
    assert!(progress.iter().any(|p| p.status.as_deref() == Some("building b")));

    // Plain `run` leaves the environment alone.
    let mut events = Vec::new();
    run::run(s.as_os_str(), &[], &Cancel::new(), |e| events.push(e)).await.ok();
    assert!(events.iter().all(|e| !matches!(e, Event::Progress(_))));
}

fn fake_nix(dir: &Path) -> PathBuf {
    std::fs::write(
        dir.join("list.json"),
        r#"{"elements":[{"attrPath":"legacyPackages.x86_64-linux.hello","originalUrl":"flake:nixpkgs","storePaths":[]}],"version":2}"#,
    )
    .unwrap();
    script(
        dir,
        "nix",
        r#"D="$(dirname "$0")"
echo "$*" >> "$D/argv.log"
case "$NIX_CONFIG" in *json-log-path*) echo "socket" >> "$D/cfg.log";; *) echo "nosocket" >> "$D/cfg.log";; esac
case "$1 $2" in "profile list") cat "$D/list.json";; esac"#,
    )
}

#[tokio::test]
async fn current_nix_uses_names_and_all() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut nix = Nix::new(fake_nix(dir.path()));
    nix.version = Some((2, 34));
    let c = Cancel::new();
    assert!(ops::remove(&nix, &["hello".into(), "hello-1".into()], &c, |_| {}).await.unwrap());
    assert!(ops::upgrade_all(&nix, &c, |_| {}).await.unwrap());
    assert!(ops::upgrade(&nix, &["hello".into()], &c, |_| {}).await.unwrap());
    assert!(ops::upgrade(&nix, &[], &c, |_| {}).await.unwrap());
    assert_eq!(
        read(dir.path(), "argv.log"),
        "profile remove hello hello-1\nprofile upgrade --all --impure\nprofile upgrade hello --impure\n"
    );
    assert_eq!(read(dir.path(), "cfg.log"), "socket\nsocket\nsocket\n");
}

#[tokio::test]
async fn old_nix_uses_attr_paths_regex_and_no_socket() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut nix = Nix::new(fake_nix(dir.path()));
    nix.version = Some((2, 19));
    let c = Cancel::new();
    assert!(ops::remove(&nix, &["0".into()], &c, |_| {}).await.unwrap());
    assert!(ops::upgrade_all(&nix, &c, |_| {}).await.unwrap());
    assert!(ops::remove(&nix, &["7".into()], &c, |_| {}).await.is_err());
    assert_eq!(
        read(dir.path(), "argv.log"),
        "profile list --json\nprofile remove legacyPackages.x86_64-linux.hello\nprofile upgrade .* --impure\nprofile list --json\n"
    );
    assert!(read(dir.path(), "cfg.log").lines().all(|l| l == "nosocket"));
}

#[tokio::test]
async fn parse_version_and_gates() {
    assert_eq!(ops::parse_version("nix (Nix) 2.34.9\n"), Some((2, 34)));
    assert_eq!(ops::parse_version("nix (Nix) 2.30.0pre20250101_abcdef"), Some((2, 30)));
    assert_eq!(ops::parse_version("garbage"), None);
    let mut nix = Nix::new("nix");
    assert!(nix.has_json_log_path());
    nix.version = Some((2, 29));
    assert!(!nix.has_json_log_path());
    nix.version = Some((2, 30));
    assert!(nix.has_json_log_path());
}

#[tokio::test]
async fn operations_do_not_overlap() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let s = script(
        dir.path(),
        "nix",
        r#"D="$(dirname "$0")"
echo start >> "$D/order.log"; sleep 0.3; echo end >> "$D/order.log""#,
    );
    let nix = Nix::new(s);
    let c = Cancel::new();
    let (a, b) = tokio::join!(
        ops::upgrade_all(&nix, &c, |_| {}),
        ops::upgrade_all(&nix, &c, |_| {})
    );
    assert!(a.unwrap() && b.unwrap());
    assert_eq!(read(dir.path(), "order.log"), "start\nend\nstart\nend\n");
}

#[tokio::test]
async fn detect_reads_the_version() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let s = script(dir.path(), "nix", "echo 'nix (Nix) 2.34.9'");
    assert_eq!(Nix::detect_at(s).await.version, Some((2, 34)));
}

#[tokio::test]
async fn add_many_is_one_command_and_empty_runs_nothing() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let nix = Nix::new(fake_nix(dir.path()));
    let c = Cancel::new();
    let pkgs = ["nixpkgs#a".to_string(), "nixpkgs#b".to_string()];
    assert!(ops::add_many(&nix, &pkgs, &c, |_| {}).await.unwrap());
    assert!(ops::add_many(&nix, &[], &c, |_| {}).await.unwrap());
    assert_eq!(read(dir.path(), "argv.log"), "profile add nixpkgs#a nixpkgs#b --impure\n");
}
