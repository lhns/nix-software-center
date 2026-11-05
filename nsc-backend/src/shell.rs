//! Shell command strings for launching apps. Every part that comes from package
//! data is single-quoted, so a name can never become shell syntax.

/// Single-quoted for a POSIX shell.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `bash -c` script that starts a GTK app by desktop ID with the package's
/// share directory on `XDG_DATA_DIRS`.
pub fn gtk_launch(installable: &str, desktop_id: &str) -> String {
    format!(
        "env XDG_DATA_DIRS=$XDG_DATA_DIRS:$(nix eval {} --raw)/share gtk-launch {}",
        quote(&format!("{installable}.outPath")),
        quote(desktop_id)
    )
}

/// A terminal command that runs `program` in a shell with the package and then stays in `$SHELL`.
/// `nix_env` selects `nix-shell -p <pkg>` over `nix shell <installable>`.
pub fn terminal_run(nix_env: bool, pkg: &str, installable: &str, program: &str) -> String {
    let script = quote(&format!("{}; \"$SHELL\"", quote(program)));
    if nix_env {
        format!("nix-shell -p {} --command {}", quote(pkg), script)
    } else {
        format!("nix shell {} --command bash -c {}", quote(installable), script)
    }
}
