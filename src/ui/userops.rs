//! Profile-mode user operations on top of `nsc-backend`, shown in the rebuild dialog.

use super::{rebuild::RebuildMsg, window::REBUILD_BROKER};
use anyhow::{bail, Result};
use log::*;
use nsc_backend::{
    ops::{self, Nix},
    pkgdb, profile,
    run::{Cancel, Event},
};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

static NIX: OnceLock<Nix> = OnceLock::new();

/// Detected on first use; two racing first uses just detect twice.
async fn nix() -> &'static Nix {
    if let Some(nix) = NIX.get() {
        return nix;
    }
    let detected = Nix::detect().await;
    NIX.get_or_init(|| detected)
}

/// Installed profile entries as `key -> version`, keys as upstream names them.
/// Version: package database for nixpkgs entries, else the store path's.
pub async fn installed_versions(pkgdb_path: &str) -> Result<HashMap<String, String>> {
    let elements = profile::list(nix().await).await?;
    let pool = pkgdb::connect(Path::new(pkgdb_path)).await;
    if let Err(e) = &pool {
        warn!("Cannot open package database {}: {}", pkgdb_path, e);
    }
    let mut out = HashMap::new();
    for pkg in profile::installed(&elements) {
        let dbversion = match (&pkg.nixpkgs_attr, &pool) {
            (Some(attr), Ok(pool)) => pkgdb::version(pool, attr).await,
            _ => None,
        };
        // `hello` and `hello-1` share a key; the first one stays.
        out.entry(pkg.key)
            .or_insert_with(|| dbversion.or(pkg.store_version).unwrap_or_default());
    }
    Ok(out)
}

/// Element names behind `keys`, looked up in the profile as it is now.
async fn element_names(keys: &[String]) -> Result<Vec<String>> {
    let installed = profile::installed(&profile::list(nix().await).await?);
    let mut names = vec![];
    for key in keys {
        let before = names.len();
        names.extend(installed.iter().filter(|p| &p.key == key).map(|p| p.element.clone()));
        if names.len() == before {
            bail!("{} is not in the profile", key);
        }
    }
    Ok(names)
}

static NEXT_OP: AtomicU64 = AtomicU64::new(1);
static CURRENT: Mutex<Option<(u64, Cancel)>> = Mutex::new(None);

/// A running profile operation the dialog's Cancel button can stop. Create it
/// before spawning, so a cancel never finds a gap.
pub struct Op {
    id: u64,
    cancel: Cancel,
}

impl Op {
    pub fn begin() -> Self {
        let op = Op {
            id: NEXT_OP.fetch_add(1, Ordering::Relaxed),
            cancel: Cancel::new(),
        };
        *CURRENT.lock().unwrap() = Some((op.id, op.cancel.clone()));
        REBUILD_BROKER.send(RebuildMsg::SetCancellable(true));
        op
    }
}

impl Drop for Op {
    fn drop(&mut self) {
        let mut cur = CURRENT.lock().unwrap();
        if cur.as_ref().map_or(false, |(id, _)| *id == self.id) {
            *cur = None;
        }
        REBUILD_BROKER.send(RebuildMsg::SetCancellable(false));
    }
}

/// Asks the running profile operation to stop; false when there is none.
pub fn cancel_current() -> bool {
    match CURRENT.lock().unwrap().as_ref() {
        Some((_, cancel)) => {
            cancel.cancel();
            true
        }
        None => false,
    }
}

fn show(e: Event) {
    match e {
        Event::Line(_, line) => REBUILD_BROKER.send(RebuildMsg::UpdateText(line)),
        Event::Progress(s) => REBUILD_BROKER.send(RebuildMsg::Progress(s)),
    }
}

/// Success of the operation; an error is shown in the dialog and counts as failure.
fn done(r: Result<bool>) -> bool {
    match r {
        Ok(ok) => ok,
        Err(e) => {
            warn!("Profile operation failed: {}", e);
            REBUILD_BROKER.send(RebuildMsg::UpdateText(e.to_string()));
            false
        }
    }
}

pub async fn add(op: &Op, attr: &str) -> bool {
    done(ops::add(nix().await, &format!("nixpkgs#{attr}"), &op.cancel, show).await)
}

pub async fn remove(op: &Op, key: &str) -> bool {
    done(async {
        let names = element_names(&[key.to_string()]).await?;
        ops::remove(nix().await, &names, &op.cancel, show).await
    }
    .await)
}

/// Upgrades everything, after removing the entries behind `remove_first`.
pub async fn upgrade_all(op: &Op, remove_first: &[String]) -> bool {
    done(async {
        let nix = nix().await;
        if remove_first.is_empty() {
            return ops::upgrade_all(nix, &op.cancel, show).await;
        }
        let names = element_names(remove_first).await?;
        ops::remove_then_upgrade_all(nix, &names, &op.cancel, show).await
    }
    .await)
}
