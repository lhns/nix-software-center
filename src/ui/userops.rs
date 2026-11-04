//! Profile-mode user operations on top of `nsc-backend`. Upgrades show in the
//! rebuild dialog; installs and removals go through the app-wide queue and its panel.

use super::{
    pkgpage::{PkgAction, PkgMsg, WorkPkg},
    queuepanel::QueuePanelMsg,
    rebuild::RebuildMsg,
    window::{PKGPAGE_BROKER, QUEUE_PANEL_BROKER, REBUILD_BROKER},
};
use anyhow::{bail, Result};
use log::*;
use nsc_backend::{
    ops::{self, Nix},
    pkgdb::{self, Unavailable},
    profile,
    queue::{self, Action, BatchResult, Item, Queue, Status},
    run::{Cancel, Event},
};
use std::{
    collections::{HashMap, VecDeque},
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

/// The queue and the cancel handle of its running batch, under one lock so a
/// cancel can neither miss a batch that just started nor hit the next one.
#[derive(Default)]
struct QueueState {
    queue: Queue<WorkPkg>,
    cancel: Option<Cancel>,
}

static QUEUE: Mutex<Option<QueueState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut QueueState) -> R) -> R {
    f(QUEUE.lock().unwrap().get_or_insert_with(Default::default))
}

fn with_queue<R>(f: impl FnOnce(&mut Queue<WorkPkg>) -> R) -> R {
    with_state(|s| f(&mut s.queue))
}

fn action_of(a: &PkgAction) -> Action {
    match a {
        PkgAction::Install => Action::Install,
        PkgAction::Remove => Action::Remove,
    }
}

fn changed() {
    QUEUE_PANEL_BROKER.send(QueuePanelMsg::Refresh);
    PKGPAGE_BROKER.send(PkgMsg::QueueChanged);
}

/// Queues a profile install or removal for the package `work.pkg`. False when
/// the same action for it is already queued or running.
pub fn enqueue(work: WorkPkg) -> bool {
    let item = Item {
        key: work.pkg.clone(),
        action: action_of(&work.action),
        payload: work,
    };
    let (added, spawn) = with_queue(|q| {
        let added = q.enqueue(item);
        (added, added && q.claim_runner())
    });
    if added {
        changed();
    }
    if spawn {
        relm4::spawn(run_queue());
    }
    added
}

/// What the package's page shows while it is in the queue.
pub fn state_label(key: &str) -> Option<&'static str> {
    with_queue(|q| q.key_status(key)).map(|s| match s {
        (Action::Install, Status::Queued) => "Queued",
        (Action::Install, Status::Running) => "Installing…",
        (Action::Remove, Status::Queued) => "Removal queued",
        (Action::Remove, Status::Running) => "Removing…",
    })
}

/// Drops the queued items for `key` (returned, to clear their busy marks) or,
/// when it is part of the running batch, cancels that batch.
pub fn cancel_item(key: &str) -> Vec<WorkPkg> {
    let gone = with_state(|s| {
        let gone = s.queue.cancel_queued(key);
        if gone.is_empty() && matches!(s.queue.key_status(key), Some((_, Status::Running))) {
            if let Some(c) = &s.cancel {
                c.cancel();
            }
        }
        gone
    });
    if !gone.is_empty() {
        changed();
    }
    gone.into_iter().map(|i| i.payload).collect()
}

/// Cancels the running batch; its items are reported as cancelled, not retried.
pub fn cancel_queue_batch() {
    if let Some(c) = &with_state(|s| s.cancel.clone()) {
        c.cancel();
    }
}

pub struct Snapshot {
    /// The running batch, e.g. "Installing hello, jq"; `None` when idle.
    pub title: Option<String>,
    pub waiting: usize,
}

pub fn snapshot() -> Snapshot {
    with_queue(|q| Snapshot {
        title: q.running().map(|b| {
            let names: Vec<_> = b
                .items
                .iter()
                .map(|i| if i.payload.pname.is_empty() { i.key.as_str() } else { i.payload.pname.as_str() })
                .collect();
            let what = match b.action {
                Action::Install => "Installing",
                Action::Remove => "Removing",
            };
            match names[..] {
                [one] if b.retry => format!("{what} {one} on its own"),
                [one] => format!("{what} {one}"),
                _ if names.len() <= 3 => format!("{what} {}", names.join(", ")),
                _ => format!("{what} {} packages", names.len()),
            }
        }),
        waiting: q.waiting(),
    })
}

/// Runs batches until the queue is empty. Every failure is reported and the
/// loop goes on.
async fn run_queue() {
    let nix = nix().await;
    loop {
        let cancel = Cancel::new();
        let Some(batch) = with_state(|s| {
            let batch = s.queue.start();
            s.cancel = batch.as_ref().map(|_| cancel.clone());
            batch
        }) else {
            break;
        };
        QUEUE_PANEL_BROKER.send(QueuePanelMsg::Progress(Default::default()));
        changed();

        let result = run_batch(nix, &batch, &cancel).await;
        let outcomes = with_state(|s| {
            s.cancel = None;
            s.queue.complete(result)
        });

        let mut failures = vec![];
        for o in &outcomes {
            let ok = o.result.is_ok();
            if let Err(text) = &o.result {
                if !cancel.is_cancelled() {
                    let what = match o.item.action {
                        Action::Install => "Installing",
                        Action::Remove => "Removing",
                    };
                    failures.push((format!("{what} {} failed", o.item.key), text.clone()));
                }
            }
            PKGPAGE_BROKER.send(PkgMsg::ProfileFinished(o.item.payload.clone(), ok));
        }
        if !failures.is_empty() {
            QUEUE_PANEL_BROKER.send(QueuePanelMsg::AddFailures(failures));
        }
        if !outcomes.is_empty() {
            // One refresh per command, not per package.
            PKGPAGE_BROKER.send(PkgMsg::ProfileBatchDone);
        }
        changed();
    }
    changed();
}

/// Longest output line kept for the panel's failure text.
const MAX_LINE: usize = 1000;

async fn run_batch(nix: &Nix, batch: &queue::Batch<WorkPkg>, cancel: &Cancel) -> BatchResult {
    let mut tail: VecDeque<String> = VecDeque::new();
    let mut sink = |e: Event| match e {
        Event::Line(_, line) => {
            if tail.len() == 12 {
                tail.pop_front();
            }
            tail.push_back(line.chars().take(MAX_LINE).collect());
        }
        Event::Progress(s) => QUEUE_PANEL_BROKER.send(QueuePanelMsg::Progress(s)),
    };
    let keys = batch.keys();
    let res = match batch.action {
        Action::Install => {
            let installables: Vec<String> = keys.iter().map(|k| format!("nixpkgs#{k}")).collect();
            ops::add_many(nix, &installables, cancel, &mut sink).await
        }
        Action::Remove => match element_names(&keys).await {
            Ok(names) => ops::remove(nix, &names, cancel, &mut sink).await,
            Err(e) => Err(e),
        },
    };
    match res {
        Ok(true) => BatchResult::Done,
        _ if cancel.is_cancelled() => BatchResult::Cancelled,
        Ok(false) => {
            let text = tail.iter().cloned().collect::<Vec<_>>().join("
");
            BatchResult::Failed(if text.is_empty() { "nix exited with an error".into() } else { text })
        }
        Err(e) => {
            warn!("Profile operation failed: {}", e);
            BatchResult::Failed(e.to_string())
        }
    }
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

/// Installed nixpkgs entries the next upgrade cannot take, as `key -> reason`.
pub async fn unavailable(pkgdb_path: &str) -> HashMap<String, String> {
    match unavailable_in(pkgdb_path).await {
        Ok(found) => found,
        Err(e) => {
            // Nix without `profile list --json`
            warn!("unavailable packages: {}; reading manifest.json", e);
            nix_data::cache::profile::unavailablepkgs()
                .await
                .unwrap_or_default()
        }
    }
}

async fn unavailable_in(pkgdb_path: &str) -> Result<HashMap<String, String>> {
    let installed = profile::installed(&profile::list(nix().await).await?);
    let mut attrs: Vec<String> = installed.into_iter().filter_map(|p| p.nixpkgs_attr).collect();
    attrs.sort();
    attrs.dedup();
    let pool = pkgdb::connect(Path::new(pkgdb_path)).await?;
    Ok(pkgdb::unavailable(&pool, &attrs)
        .await
        .into_iter()
        .map(|(attr, why)| {
            let why = match why {
                Unavailable::Missing => "not in this nixpkgs",
                Unavailable::Broken => "marked broken",
                Unavailable::Insecure => "marked insecure",
            };
            (attr, why.to_string())
        })
        .collect())
}
