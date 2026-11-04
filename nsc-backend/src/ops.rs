//! Profile operations. Each takes the process-wide operation lock for its
//! whole duration and streams output through the caller's `on_event`.

use crate::profile;
use crate::run::{self, Cancel, Event};
use anyhow::{bail, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// `--all`, element names and `--regex` arrived in Nix 2.20.
const NAMED_ELEMENTS: (u32, u32) = (2, 20);
/// `json-log-path` is in the 2.30 release notes (doc/manual/source/release-notes/rl-2.30.md).
const JSON_LOG_PATH: (u32, u32) = (2, 30);

/// Set once `nix profile add` turned out to be unknown.
static USE_INSTALL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct Nix {
    pub program: PathBuf,
    /// `None` when unknown; treated as a current Nix.
    pub version: Option<(u32, u32)>,
}

impl Nix {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Nix {
            program: program.into(),
            version: None,
        }
    }

    pub async fn detect() -> Self {
        Self::detect_at("nix").await
    }

    pub async fn detect_at(program: impl Into<PathBuf>) -> Self {
        let mut nix = Self::new(program);
        let out = tokio::process::Command::new(&nix.program)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => {
                nix.version = parse_version(&String::from_utf8_lossy(&o.stdout));
            }
            Ok(o) => log::warn!("nix --version failed: {}", o.status),
            Err(e) => log::warn!("nix --version: {e}"),
        }
        nix
    }

    fn named_elements(&self) -> bool {
        self.version.map_or(true, |v| v >= NAMED_ELEMENTS)
    }

    pub fn has_json_log_path(&self) -> bool {
        self.version.map_or(true, |v| v >= JSON_LOG_PATH)
    }
}

/// `nix (Nix) 2.34.9` -> (2, 34).
pub fn parse_version(out: &str) -> Option<(u32, u32)> {
    let v = out.split_whitespace().last()?;
    let mut parts = v.split('.');
    let num = |s: &str| {
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    };
    Some((num(parts.next()?)?, num(parts.next()?)?))
}

/// Runs `nix <args>`; the flag tells whether Nix said `word` is not a command.
async fn nix_run(
    nix: &Nix,
    args: &[String],
    cancel: &Cancel,
    on_event: &mut (impl FnMut(Event) + Send),
    unrecognised: &str,
) -> Result<(bool, bool)> {
    let mut seen = false;
    let wrapped = |e: Event| {
        if let Event::Line(_, l) = &e {
            seen |= l.contains(unrecognised);
        }
        on_event(e);
    };
    let status = if nix.has_json_log_path() {
        run::run_with_progress(nix.program.as_os_str(), args, cancel, wrapped).await?
    } else {
        run::run(nix.program.as_os_str(), args, cancel, wrapped).await?
    };
    Ok((status.success(), seen))
}

fn args(parts: &[&str], rest: &[String]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).chain(rest.iter().cloned()).collect()
}

/// `nix profile add <installable>`, falling back to `install` for
/// Nix versions that predate `add`.
pub async fn add(
    nix: &Nix,
    installable: &str,
    cancel: &Cancel,
    on_event: impl FnMut(Event) + Send,
) -> Result<bool> {
    add_many(nix, &[installable.to_string()], cancel, on_event).await
}

/// One `nix profile add a b c` for all installables: Nix commits the
/// whole command or none of it.
pub async fn add_many(
    nix: &Nix,
    installables: &[String],
    cancel: &Cancel,
    mut on_event: impl FnMut(Event) + Send,
) -> Result<bool> {
    if installables.is_empty() {
        return Ok(true);
    }
    let _lock = run::lock().await;
    let rest = installables.to_vec();
    let verb = if USE_INSTALL.load(Ordering::Relaxed) { "install" } else { "add" };
    let (ok, unknown) = nix_run(
        nix,
        &args(&["profile", verb], &rest),
        cancel,
        &mut on_event,
        "'add' is not a recognised command",
    )
    .await?;
    if ok || !unknown || verb == "install" {
        return Ok(ok);
    }
    USE_INSTALL.store(true, Ordering::Relaxed);
    let (ok, _) = nix_run(nix, &args(&["profile", "install"], &rest), cancel, &mut on_event, "").await?;
    Ok(ok)
}

/// Element selectors for remove/upgrade: names, or attrPaths before Nix 2.20
/// (where `names` are list indices).
async fn selectors(nix: &Nix, names: &[String]) -> Result<Vec<String>> {
    if nix.named_elements() {
        return Ok(names.to_vec());
    }
    let elements = profile::list(nix).await?;
    names
        .iter()
        .map(|n| {
            match elements.iter().find(|e| &e.name == n).and_then(|e| e.attr_path.clone()) {
                Some(a) => Ok(a),
                None => bail!("profile element {n} not found"),
            }
        })
        .collect()
}

/// One `nix profile remove a b` for all `names`.
pub async fn remove(
    nix: &Nix,
    names: &[String],
    cancel: &Cancel,
    mut on_event: impl FnMut(Event) + Send,
) -> Result<bool> {
    let _lock = run::lock().await;
    remove_locked(nix, names, cancel, &mut on_event).await
}

async fn remove_locked(
    nix: &Nix,
    names: &[String],
    cancel: &Cancel,
    on_event: &mut (impl FnMut(Event) + Send),
) -> Result<bool> {
    if names.is_empty() {
        return Ok(true);
    }
    let sel = selectors(nix, names).await?;
    Ok(nix_run(nix, &args(&["profile", "remove"], &sel), cancel, on_event, "").await?.0)
}

pub async fn upgrade_all(nix: &Nix, cancel: &Cancel, mut on_event: impl FnMut(Event) + Send) -> Result<bool> {
    let _lock = run::lock().await;
    upgrade_locked(nix, None, cancel, &mut on_event).await
}

pub async fn upgrade(
    nix: &Nix,
    names: &[String],
    cancel: &Cancel,
    mut on_event: impl FnMut(Event) + Send,
) -> Result<bool> {
    let _lock = run::lock().await;
    upgrade_locked(nix, Some(names), cancel, &mut on_event).await
}

/// Removes `remove_first`, then upgrades everything, under one lock; stops if
/// the removal fails.
pub async fn remove_then_upgrade_all(
    nix: &Nix,
    remove_first: &[String],
    cancel: &Cancel,
    mut on_event: impl FnMut(Event) + Send,
) -> Result<bool> {
    let _lock = run::lock().await;
    if !remove_locked(nix, remove_first, cancel, &mut on_event).await? {
        return Ok(false);
    }
    upgrade_locked(nix, None, cancel, &mut on_event).await
}

async fn upgrade_locked(
    nix: &Nix,
    names: Option<&[String]>,
    cancel: &Cancel,
    on_event: &mut (impl FnMut(Event) + Send),
) -> Result<bool> {
    let mut a = args(&["profile", "upgrade"], &[]);
    match names {
        None if nix.named_elements() => a.push("--all".into()),
        None => a.push(".*".into()),
        Some([]) => return Ok(true),
        Some(n) => a.extend(selectors(nix, n).await?),
    }
    Ok(nix_run(nix, &a, cancel, on_event, "").await?.0)
}
