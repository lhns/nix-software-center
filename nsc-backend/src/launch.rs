//! Starting a built package that is not installed: find its app or program in the build
//! outputs and run it with the outputs on `PATH` and `XDG_DATA_DIRS`.
//!
//! Desktop entries are read here instead of going through gtk-launch or GIO: D-Bus
//! activation cannot find the service file of a package that is not installed, so
//! `DBusActivatable` is ignored on purpose.

use crate::ops::{self, Nix};
use crate::run::{Cancel, Event};
use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What to start.
#[derive(Debug, Clone, PartialEq)]
pub struct Exec {
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Needs a terminal (`Terminal=true`, and programs without an app).
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Desktop,
    Program,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub kind: Kind,
    /// The `.desktop` file or the program.
    pub path: PathBuf,
    pub exec: Exec,
}

/// Environment values for the started program: the outputs' `bin` and `share` come first.
#[derive(Debug, Clone, PartialEq)]
pub struct Env {
    pub path: OsString,
    pub xdg_data_dirs: OsString,
}

fn prepend(outs: &[PathBuf], sub: &str, current: Option<OsString>) -> OsString {
    let mut dirs: Vec<PathBuf> = outs.iter().map(|o| o.join(sub)).filter(|d| d.is_dir()).collect();
    if let Some(c) = current.filter(|c| !c.is_empty()) {
        dirs.extend(std::env::split_paths(&c));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

/// The environment for `outs`, on top of the given current values (`None`: unset).
pub fn env_for(outs: &[PathBuf], path: Option<OsString>, xdg_data_dirs: Option<OsString>) -> Env {
    let xdg = xdg_data_dirs.filter(|x| !x.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    Env { path: prepend(outs, "bin", path), xdg_data_dirs: prepend(outs, "share", Some(xdg)) }
}

/// `env_for` on this process's environment.
pub fn current_env(outs: &[PathBuf]) -> Env {
    env_for(outs, std::env::var_os("PATH"), std::env::var_os("XDG_DATA_DIRS"))
}

#[derive(Debug, Default)]
struct Entry {
    exec: Option<String>,
    name: String,
    icon: Option<String>,
    path: Option<String>,
    terminal: bool,
    hidden: bool,
}

/// The `[Desktop Entry]` group; keys with a locale are skipped.
fn parse_entry(text: &str) -> Entry {
    let mut e = Entry::default();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            continue;
        }
        let Some((k, v)) = line.split_once('=').filter(|_| inside) else { continue };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Exec" => e.exec = Some(v.to_string()),
            "Name" => e.name = v.to_string(),
            "Icon" => e.icon = Some(v.to_string()).filter(|i| !i.is_empty()),
            "Path" => e.path = Some(v.to_string()).filter(|p| !p.is_empty()),
            "Terminal" => e.terminal = v == "true",
            "NoDisplay" | "Hidden" => e.hidden |= v == "true",
            _ => {}
        }
    }
    e
}

/// String-value escapes of the Desktop Entry spec.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => out.push('\\'),
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            // Not a string escape: the quoting layer reads it.
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Splits an Exec value into arguments: whitespace outside double quotes separates; inside
/// quotes a backslash escapes the next character.
fn split_exec(s: &str) -> Vec<String> {
    let mut args = vec![];
    let mut cur = String::new();
    let (mut quoted, mut in_arg) = (false, false);
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            '\\' if quoted => {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
                in_arg = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_arg {
                    args.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            c => {
                cur.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(cur);
    }
    args
}

/// Expands the field codes of one argument. A lone `%i` becomes two arguments; an argument that
/// only held dropped codes disappears.
fn expand(arg: &str, e: &Entry, file: &Path) -> Vec<String> {
    if arg == "%i" {
        return match &e.icon {
            Some(i) => vec!["--icon".into(), i.clone()],
            None => vec![],
        };
    }
    let mut out = String::new();
    let mut coded = false;
    let mut it = arg.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        coded = true;
        match it.next() {
            Some('%') => out.push('%'),
            Some('c') => out.push_str(&e.name),
            Some('k') => out.push_str(&file.to_string_lossy()),
            // Files, URLs, deprecated codes and an icon inside a word: nothing to pass.
            _ => {}
        }
    }
    if out.is_empty() && coded {
        vec![]
    } else {
        vec![out]
    }
}

fn exec_of(e: &Entry, file: &Path) -> Result<Exec> {
    let Some(raw) = &e.exec else { bail!("{} has no Exec", file.display()) };
    let argv: Vec<String> = split_exec(&unescape(raw)).iter().flat_map(|a| expand(a, e, file)).collect();
    if argv.is_empty() {
        bail!("{} has an empty Exec", file.display());
    }
    Ok(Exec { argv, cwd: e.path.as_ref().map(PathBuf::from), terminal: e.terminal })
}

/// What `file` (a `.desktop` file) runs.
pub fn desktop_exec(file: &Path) -> Result<Exec> {
    let text = std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    exec_of(&parse_entry(&text), file)
}

fn desktop_files(outs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = vec![];
    for out in outs {
        let Ok(dir) = std::fs::read_dir(out.join("share/applications")) else { continue };
        let mut found: Vec<PathBuf> = dir
            .flatten()
            .map(|d| d.path())
            .filter(|p| p.extension().map_or(false, |x| x == "desktop") && p.is_file())
            .collect();
        found.sort();
        files.extend(found);
    }
    files
}

/// The app to start: the AppStream desktop id if the outputs have it, else their only visible
/// desktop entry.
pub fn find_desktop(outs: &[PathBuf], desktop_id: Option<&str>) -> Option<PathBuf> {
    let files = desktop_files(outs);
    if let Some(id) = desktop_id.filter(|i| !i.is_empty()) {
        let id = if id.ends_with(".desktop") { id.to_string() } else { format!("{id}.desktop") };
        if let Some(f) = files.iter().find(|f| f.file_name().map_or(false, |n| n == id.as_str())) {
            return Some(f.clone());
        }
    }
    let mut visible = files.into_iter().filter(|f| {
        std::fs::read_to_string(f).map_or(false, |t| !parse_entry(&t).hidden)
    });
    let only = visible.next()?;
    visible.next().is_none().then_some(only)
}

/// `bin/<name>` in the first output that has it as a file.
pub fn find_program(outs: &[PathBuf], name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    outs.iter().map(|o| o.join("bin").join(name)).find(|p| p.is_file())
}

/// What to start from the build outputs: the desktop entry (see [`find_desktop`]), else the
/// package's main program. Asks Nix for the program name only when there is no entry.
pub async fn resolve(
    nix: &Nix,
    installable: &str,
    name: &str,
    outs: &[PathBuf],
    desktop_id: Option<&str>,
    cancel: &Cancel,
    on_event: impl FnMut(Event) + Send,
) -> Result<Target> {
    if let Some(file) = find_desktop(outs, desktop_id) {
        let exec = desktop_exec(&file)?;
        return Ok(Target { kind: Kind::Desktop, path: file, exec });
    }
    let program = ops::main_program(nix, installable, cancel, on_event).await.ok();
    match program.as_deref().and_then(|p| find_program(outs, p)) {
        Some(path) => Ok(Target {
            kind: Kind::Program,
            exec: Exec { argv: vec![path.to_string_lossy().into_owned()], cwd: None, terminal: true },
            path,
        }),
        None => bail!("{name} has no app or program to run; try Open interactive shell"),
    }
}

/// Starts `exec` for good: own session, so the app outlives the software center and a cancel
/// never reaches it. A bare program name is looked up in the new `PATH`.
pub fn spawn_detached(exec: &Exec, env: &Env) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Some((prog, rest)) = exec.argv.split_first() else { bail!("nothing to run") };
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .env("PATH", &env.path)
        .env("XDG_DATA_DIRS", &env.xdg_data_dirs)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = &exec.cwd {
        cmd.current_dir(cwd);
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().with_context(|| format!("failed to start {prog}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Runs `exec` here and waits, output inherited; for callers without a terminal, like the smoke test.
pub fn run_foreground(exec: &Exec, env: &Env) -> Result<std::process::ExitStatus> {
    use std::process::Command;
    let Some((prog, rest)) = exec.argv.split_first() else { bail!("nothing to run") };
    let mut cmd = Command::new(prog);
    cmd.args(rest).env("PATH", &env.path).env("XDG_DATA_DIRS", &env.xdg_data_dirs);
    if let Some(cwd) = &exec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.status().with_context(|| format!("failed to start {prog}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn out(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let o = root.join(name);
        for (rel, text) in files {
            let f = o.join(rel);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, text).unwrap();
        }
        o
    }

    fn argv(exec: &str, extra: &str) -> Vec<String> {
        let text = format!("[Desktop Entry]\nName=Demo App\nIcon=demo\n{extra}\nExec={exec}\n");
        exec_of(&parse_entry(&text), Path::new("/x/demo.desktop")).unwrap().argv
    }

    #[test]
    fn exec_quoting_and_field_codes() {
        assert_eq!(argv("demo --flag %U", ""), ["demo", "--flag"]);
        assert_eq!(argv(r#""/opt/my app/demo" "a \" b" %f"#, ""), ["/opt/my app/demo", "a \" b"]);
        // The string-level escape comes first, then the quoting one.
        assert_eq!(argv(r#""a \\\\ b""#, ""), [r"a \ b"]);
        assert_eq!(argv("demo %i %c %k 100%% --x=%F", ""), ["demo", "--icon", "demo", "Demo App", "/x/demo.desktop", "100%", "--x="]);
        assert_eq!(argv("demo %d %D %n %N %v %m", ""), ["demo"]);
    }

    #[test]
    fn icon_code_without_an_icon_is_dropped() {
        let e = parse_entry("[Desktop Entry]\nExec=demo %i end\n");
        assert_eq!(exec_of(&e, Path::new("f")).unwrap().argv, ["demo", "end"]);
    }

    #[test]
    fn path_terminal_and_other_groups() {
        let text = "[Desktop Action x]\nExec=wrong\n[Desktop Entry]\nName[de]=Falsch\nName=Right\nExec=demo\nPath=/tmp\nTerminal=true\n";
        let e = exec_of(&parse_entry(text), Path::new("f")).unwrap();
        assert_eq!(e, Exec { argv: vec!["demo".into()], cwd: Some("/tmp".into()), terminal: true });
        assert!(exec_of(&parse_entry("[Desktop Entry]\nName=x\n"), Path::new("f")).is_err());
    }

    #[test]
    fn desktop_id_wins_then_the_only_visible_entry() {
        let d = tempfile::tempdir().unwrap();
        let a = out(d.path(), "a", &[
            ("share/applications/one.desktop", "[Desktop Entry]\nExec=one\n"),
            ("share/applications/two.desktop", "[Desktop Entry]\nExec=two\n"),
            ("share/applications/hid.desktop", "[Desktop Entry]\nExec=h\nNoDisplay=true\n"),
        ]);
        let outs = [a.clone()];
        let by_id = |id| find_desktop(&outs, Some(id)).unwrap().file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(by_id("two"), "two.desktop");
        assert_eq!(by_id("two.desktop"), "two.desktop");
        // Two visible entries and no matching id: ambiguous.
        assert_eq!(find_desktop(&outs, Some("missing")), None);
        assert_eq!(find_desktop(&outs, None), None);

        let b = out(d.path(), "b", &[
            ("share/applications/solo.desktop", "[Desktop Entry]\nExec=solo\n"),
            ("share/applications/hid.desktop", "[Desktop Entry]\nExec=h\nHidden=true\n"),
        ]);
        assert_eq!(find_desktop(&[b.clone()], Some("missing")), Some(b.join("share/applications/solo.desktop")));
        assert_eq!(find_desktop(&[b.clone(), out(d.path(), "c", &[])], None), Some(b.join("share/applications/solo.desktop")));
        assert_eq!(find_desktop(&[out(d.path(), "empty", &[])], None), None);
    }

    #[test]
    fn programs_come_from_bin_only() {
        let d = tempfile::tempdir().unwrap();
        let a = out(d.path(), "a", &[("bin/tool", ""), ("lib/other", "")]);
        assert_eq!(find_program(&[a.clone()], "tool"), Some(a.join("bin/tool")));
        assert_eq!(find_program(&[a.clone()], "other"), None);
        assert_eq!(find_program(&[a.clone()], "../lib/other"), None);
        assert_eq!(find_program(&[a], ""), None);
    }

    #[test]
    fn env_puts_existing_dirs_first() {
        let d = tempfile::tempdir().unwrap();
        let a = out(d.path(), "a", &[("bin/x", ""), ("share/y", "")]);
        let b = out(d.path(), "b", &[("share/y", "")]);
        let e = env_for(&[a.clone(), b.clone()], Some("/usr/bin".into()), None);
        assert_eq!(e.path, OsString::from(format!("{}:/usr/bin", a.join("bin").display())));
        assert_eq!(
            e.xdg_data_dirs,
            OsString::from(format!("{}:{}:/usr/local/share:/usr/share", a.join("share").display(), b.join("share").display()))
        );
        assert_eq!(env_for(&[b], None, Some("/s".into())).path, OsString::new());
    }

    #[test]
    fn spawn_looks_up_a_bare_name_in_the_new_path() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let a = out(d.path(), "a", &[("bin/nsc-marker", "#!/bin/sh\necho ran > \"$1\"\n")]);
        fs::set_permissions(a.join("bin/nsc-marker"), fs::Permissions::from_mode(0o755)).unwrap();
        let marker = d.path().join("marker");
        let env = env_for(&[a], std::env::var_os("PATH"), None);
        let exec = Exec { argv: vec!["nsc-marker".into(), marker.to_string_lossy().into()], cwd: None, terminal: false };
        spawn_detached(&exec, &env).unwrap();
        for _ in 0..100 {
            if marker.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("the program did not run");
    }
}
