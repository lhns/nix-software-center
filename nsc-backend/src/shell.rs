//! Shell command strings for launching apps. Every part that comes from package
//! data is single-quoted, so a name can never become shell syntax.

/// Single-quoted for a POSIX shell.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `sh -c` script for a terminal that runs a not installed program: the package's `PATH`
/// and `XDG_DATA_DIRS` exported, the command, then a shell that still has them.
pub fn terminal_script(path: &str, xdg_data_dirs: &str, cwd: Option<&str>, argv: &[String]) -> String {
    let words: Vec<String> = argv.iter().map(|a| quote(a)).collect();
    let cd = cwd.map(|c| format!("cd {}; ", quote(c))).unwrap_or_default();
    format!(
        "export PATH={} XDG_DATA_DIRS={}; {cd}{}; exec \"${{SHELL:-sh}}\"",
        quote(path),
        quote(xdg_data_dirs),
        words.join(" ")
    )
}

/// A terminal command that runs `program` in `nix-shell -p <pkg>` and then stays in `$SHELL`.
pub fn terminal_run(pkg: &str, program: &str) -> String {
    let script = quote(&format!("{}; \"$SHELL\"", quote(program)));
    format!("nix-shell -p {} --command {}", quote(pkg), script)
}
