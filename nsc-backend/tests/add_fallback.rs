//! Own test binary: the `add` -> `install` choice is cached per process.
#![cfg(unix)]

use nsc_backend::ops::{self, Nix};
use nsc_backend::run::{Cancel, Event};
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn add_falls_back_to_install_and_remembers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nix");
    std::fs::write(
        &path,
        r#"#!/bin/sh
D="$(dirname "$0")"
echo "$*" >> "$D/argv.log"
if [ "$2" = add ]; then
  echo "error: 'add' is not a recognised command" >&2
  exit 1
fi
echo "installed $3"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let nix = Nix::new(&path);
    let c = Cancel::new();
    let mut lines = Vec::new();
    let ok = ops::add(&nix, "nixpkgs#hello", &c, |e| lines.push(e)).await.unwrap();
    assert!(ok);
    assert!(lines.contains(&Event::Line(nsc_backend::run::Stream::Stdout, "installed nixpkgs#hello".into())));

    assert!(ops::add(&nix, "nixpkgs#jq", &c, |_| {}).await.unwrap());
    let log = std::fs::read_to_string(dir.path().join("argv.log")).unwrap();
    assert_eq!(
        log,
        "profile add nixpkgs#hello --impure\nprofile install nixpkgs#hello --impure\nprofile install nixpkgs#jq --impure\n"
    );

    // Any other failure is reported as is, with a single attempt.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nix");
    std::fs::write(&path, "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/argv.log\"\necho boom >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ok = ops::add(&Nix::new(&path), "x", &Cancel::new(), |_| {}).await.unwrap();
    assert!(!ok);
    let log = std::fs::read_to_string(dir.path().join("argv.log")).unwrap();
    assert_eq!(log.lines().count(), 1);
}
