use nsc_backend::shell::{quote, terminal_run, terminal_script};

const HOSTILE: &str = "x'; touch /tmp/nsc-pwn; echo '$(id)`id`\"";

#[test]
fn quote_survives_single_quotes() {
    assert_eq!(quote("a b"), "'a b'");
    assert_eq!(quote("it's"), "'it'\\''s'");
}

#[test]
fn launch_strings_hold_a_hostile_name_as_one_word() {
    let e = terminal_run(HOSTILE, HOSTILE);
    assert!(e.starts_with(&format!("nix-shell -p {} --command ", quote(HOSTILE))), "{e}");
    assert!(e.contains(&quote(&format!("{}; \"$SHELL\"", quote(HOSTILE)))), "{e}");
}

#[test]
fn terminal_script_quotes_every_value() {
    let argv = vec![HOSTILE.to_string(), "a b".to_string()];
    let s = terminal_script(HOSTILE, HOSTILE, Some(HOSTILE), &argv);
    assert_eq!(
        s,
        format!(
            "export PATH={q} XDG_DATA_DIRS={q}; cd {q}; {q} 'a b'; exec \"${{SHELL:-sh}}\"",
            q = quote(HOSTILE)
        )
    );
}

#[cfg(unix)]
#[test]
fn terminal_script_runs_the_command_then_a_shell_with_the_environment() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let hostile = format!("x'; touch {}/pwned; echo '", dir.path().display());
    // The "shell" it ends in records what it inherited.
    let shell = dir.path().join("shell");
    std::fs::write(&shell, format!("#!/bin/sh
printf '%s|%s' \"$PATH\" \"$XDG_DATA_DIRS\" > {}
", quote(&out.to_string_lossy()))).unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let script = terminal_script("/p:/bin", "/s", None, &["echo".into(), hostile.clone()]);
    let st = std::process::Command::new("sh").arg("-c").arg(script).env("SHELL", &shell).stdout(std::process::Stdio::null()).status().unwrap();
    assert!(st.success());
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "/p:/bin|/s");
    assert!(!dir.path().join("pwned").exists());
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
