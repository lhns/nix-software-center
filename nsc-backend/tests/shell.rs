use nsc_backend::shell::{gtk_launch, quote, terminal_run};

const HOSTILE: &str = "x'; touch /tmp/nsc-pwn; echo '$(id)`id`\"";

#[test]
fn quote_survives_single_quotes() {
    assert_eq!(quote("a b"), "'a b'");
    assert_eq!(quote("it's"), "'it'\\''s'");
}

#[test]
fn launch_strings_hold_a_hostile_name_as_one_word() {
    let s = gtk_launch("nixpkgs#hello", HOSTILE);
    assert!(s.ends_with(&format!("gtk-launch {}", quote(HOSTILE))), "{s}");
    let t = terminal_run(false, HOSTILE, "nixpkgs#hello", HOSTILE);
    assert!(t.contains(&quote(&format!("{}; \"$SHELL\"", quote(HOSTILE)))), "{t}");
    assert!(t.starts_with(&format!("nix shell {} --command bash -c ", quote("nixpkgs#hello"))));
    let e = terminal_run(true, HOSTILE, "unused", HOSTILE);
    assert!(e.starts_with(&format!("nix-shell -p {} --command ", quote(HOSTILE))), "{e}");
}

#[cfg(unix)]
#[test]
fn a_hostile_name_is_not_run_by_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pwned");
    let name = format!("x'; touch {}; echo '", marker.display());
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf %s {}", quote(&name)))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), name);
    assert!(!marker.exists());
}
