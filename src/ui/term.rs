//! Starting things in a terminal.

use log::*;
use std::{env, fs, process::Command};

/// Executable file called `name` in a `PATH` directory (`command -v` is a shell builtin).
pub fn in_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    env::var_os("PATH").map_or(false, |path| {
        env::split_paths(&path).any(|dir| {
            fs::metadata(dir.join(name)).map_or(false, |m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Runs `cmd` with `sh -c` in a terminal: `$TERMINAL`, `xdg-terminal-exec`, then the first known one.
pub fn launchterm(cmd: &str) {
    let term = env::var("TERMINAL").ok().filter(|t| !t.is_empty());
    let (prog, args): (String, Vec<&str>) = if let Some(t) = term {
        (t, vec!["-e"])
    } else if in_path("xdg-terminal-exec") {
        ("xdg-terminal-exec".into(), vec![])
    } else if let Some((p, a)) = [
        ("konsole", vec!["-e"]),
        ("gnome-console", vec!["-e"]),
        ("kgx", vec!["-e"]),
        ("gnome-terminal", vec!["--"]),
        ("xfce4-terminal", vec!["-x"]),
        ("alacritty", vec!["-e"]),
        ("xterm", vec!["-e"]),
    ]
    .into_iter()
    .find(|(p, _)| in_path(p))
    {
        (p.into(), a)
    } else {
        warn!("No terminal found; set $TERMINAL");
        return;
    };
    if let Err(e) = Command::new(&prog).args(args).args(["sh", "-c", cmd]).spawn() {
        warn!("Failed to start terminal {}: {}", prog, e);
    }
}
