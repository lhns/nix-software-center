//! The app-wide work queue on top of `nsc-backend`: profile installs and
//! removals run as batches here, everything else as exclusive jobs (`jobs.rs`).
//! The Activity tab and its log modal show what this module holds.

use super::{
    activitylog::LogMsg,
    activitypage::ActivityMsg,
    jobs::{self, Job, JobKind, UpdateJob},
    pkgpage::{InstallType, PkgAction, PkgMsg, WorkPkg},
    updatepage::UpdateType,
    window::{SystemPkgs, UserPkgs, ACTIVITYLOG_BROKER, ACTIVITY_BROKER, PKGPAGE_BROKER},
};
use anyhow::{bail, Result};
use log::*;
use nix_data::config::configfile::NixDataConfig;
use nsc_backend::{
    ops::{self, Nix},
    pkgdb::{self, Unavailable},
    profile,
    progress::Summary,
    queue::{self, Action, BatchResult, Entry, Item, Queue, Status},
    run::{Cancel, Event},
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Mutex, OnceLock},
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
    let installed = installed_in_sources(&elements);
    super::sourceinfo::set_removed(installed.iter().filter(|p| p.source_removed).map(|p| p.key.clone()).collect());
    for pkg in installed {
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

fn installed_in_sources(elements: &[profile::ProfileElement]) -> Vec<profile::InstalledPkg> {
    let (srcs, unfree) = super::sourceinfo::profile_sources();
    profile::installed_in(elements, Some((&srcs, &unfree)))
}

/// Element names behind `keys`, looked up in the profile as it is now.
async fn element_names(keys: &[String]) -> Result<Vec<String>> {
    let installed = installed_in_sources(&profile::list(nix().await).await?);
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


/// Settings a job runs with, as they are when it is queued.
#[derive(Clone)]
pub struct Ctx {
    pub config: NixDataConfig,
    pub syspkgs: SystemPkgs,
    pub userpkgs: UserPkgs,
}

impl Ctx {
    fn job(&self, kind: JobKind) -> Job {
        Job {
            kind,
            config: self.config.clone(),
            syspkgs: self.syspkgs.clone(),
            userpkgs: self.userpkgs.clone(),
        }
    }
}

/// The queue and the cancel handle of its running batch, under one lock so a
/// cancel can neither miss a batch that just started nor hit the next one.
#[derive(Default)]
struct QueueState {
    queue: Queue<Job>,
    cancel: Option<Cancel>,
}

static QUEUE: Mutex<Option<QueueState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut QueueState) -> R) -> R {
    f(QUEUE.lock().unwrap().get_or_insert_with(Default::default))
}

pub(super) fn with_queue<R>(f: impl FnOnce(&mut Queue<Job>) -> R) -> R {
    with_state(|s| f(&mut s.queue))
}

/// Queue key of a package operation. Profile keys are the bare attribute;
/// the others carry their type so the same package in two places stays apart.
pub fn pkg_key(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> String {
    match (pkgtype, userpkgs) {
        (InstallType::System, _) => format!("system:{pkg}"),
        (InstallType::User, UserPkgs::Env) => format!("env:{pkg}"),
        (InstallType::User, UserPkgs::Profile) => pkg.to_string(),
    }
}

/// Tells every view that shows queue state.
pub(super) fn changed() {
    ACTIVITY_BROKER.send(ActivityMsg::Refresh);
    ACTIVITYLOG_BROKER.send(LogMsg::Refresh);
    PKGPAGE_BROKER.send(PkgMsg::QueueChanged);
}

/// Every Done, Failed or Cancelled item ends here, whatever stopped it, so
/// the busy marks of the installed page always clear.
fn finished(job: &Job, ok: bool) {
    if let JobKind::Pkg(work) = &job.kind {
        PKGPAGE_BROKER.send(PkgMsg::JobFinished(work.clone(), ok));
    }
}

fn enqueue_job(key: String, action: Action, job: Job) -> bool {
    let item = Item { id: 0, key, action, payload: job };
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

/// Queues a package install or removal. False when the same one is already
/// queued or running.
pub fn enqueue(work: WorkPkg, ctx: Ctx) -> bool {
    if work.block {
        return false;
    }
    if work.pkgtype == InstallType::System && !nsc_backend::sources::settings().system_packages {
        warn!("Refusing a system job: system packages are off");
        return false;
    }
    let key = pkg_key(&work.pkg, &work.pkgtype, &ctx.userpkgs);
    let action = match (&work.pkgtype, &ctx.userpkgs, &work.action) {
        (InstallType::User, UserPkgs::Profile, PkgAction::Install) => Action::Install,
        (InstallType::User, UserPkgs::Profile, PkgAction::Remove) => Action::Remove,
        _ => Action::Exclusive,
    };
    let job = ctx.job(JobKind::Pkg(work));
    enqueue_job(key, action, job)
}

/// Queues an update. False when the same update is already queued or running.
pub fn enqueue_update(update: UpdateJob, ctx: Ctx) -> bool {
    if update.scope == UpdateType::System && !nsc_backend::sources::settings().system_packages {
        warn!("Refusing a system update: system packages are off");
        return false;
    }
    let key = match update.scope {
        UpdateType::System => "update:system",
        UpdateType::User => "update:user",
        UpdateType::All => "update:all",
    };
    let job = ctx.job(JobKind::Update(update));
    enqueue_job(key.to_string(), Action::Exclusive, job)
}

/// What the package's page shows while it is in the queue.
pub fn state_label(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> Option<&'static str> {
    let key = pkg_key(pkg, pkgtype, userpkgs);
    with_queue(|q| q.key_item(&key).map(|(i, s)| (i.payload.is_remove(), s))).map(|s| match s {
        (false, Status::Queued) => "Queued",
        (false, Status::Running) => "Installing…",
        (true, Status::Queued) => "Removal queued",
        (true, Status::Running) => "Removing…",
    })
}

/// The queued or running entry for the package, to open its log.
pub fn active_id(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> Option<u64> {
    let key = pkg_key(pkg, pkgtype, userpkgs);
    with_queue(|q| q.key_item(&key).map(|(i, _)| i.id))
}

/// A queued entry can always be dropped; a running one only if its job can be stopped.
pub fn can_cancel(entry: &Entry<Job>) -> bool {
    entry.state == queue::EntryState::Queued || entry.payload.cancellable()
}

/// `can_cancel` for the package's queued or running entry.
pub fn pkg_can_cancel(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> bool {
    let key = pkg_key(pkg, pkgtype, userpkgs);
    with_queue(|q| q.key_item(&key).map(|(i, s)| s == Status::Queued || i.payload.cancellable())).unwrap_or(false)
}

/// Drops the queued entry, or asks the running one to stop. Cancelling a row
/// of a running batch stops that whole Nix command.
pub fn cancel(id: u64) {
    let gone = with_state(|s| {
        let gone = s.queue.cancel_queued_id(id);
        if gone.is_none() {
            let running = s.queue.running().and_then(|b| b.items.iter().find(|i| i.id == id));
            if running.map_or(false, |i| i.payload.cancellable()) {
                if let Some(c) = &s.cancel {
                    c.cancel();
                }
            }
        }
        gone
    });
    if let Some(item) = gone {
        finished(&item.payload, false);
        changed();
    }
}

/// Puts a failed or cancelled entry back in the queue; false when the same work is active.
pub fn retry(id: u64) -> bool {
    let (ok, spawn) = with_queue(|q| {
        // The installed page's busy mark belongs to the original click.
        let ok = q.retry(id, |job| {
            if let JobKind::Pkg(work) = &mut job.kind {
                work.notify = None;
            }
        });
        (ok, ok && q.claim_runner())
    });
    if ok {
        changed();
    }
    if spawn {
        relm4::spawn(run_queue());
    }
    ok
}

pub fn remove(id: u64) {
    if with_queue(|q| q.remove(id)) {
        changed();
    }
}

pub fn clear_finished() {
    if with_queue(|q| q.clear_finished()) > 0 {
        changed();
    }
}

pub fn entries() -> Vec<Entry<Job>> {
    with_queue(|q| q.entries())
}

pub fn entry(id: u64) -> Option<Entry<Job>> {
    with_queue(|q| q.entry_by_id(id))
}

pub fn log_since(id: u64, seen: u64) -> (Vec<String>, u64) {
    with_queue(|q| q.log_since(id, seen))
}

/// Running plus queued items, and failures nobody has looked at.
pub fn counts() -> (usize, usize) {
    with_queue(|q| (q.active(), q.unseen_failures()))
}

pub fn mark_failures_seen() {
    with_queue(|q| q.mark_failures_seen());
}

/// Work is running or waiting.
pub fn is_busy() -> bool {
    with_queue(|q| q.active() > 0)
}

pub fn has_running() -> bool {
    with_queue(|q| q.running().is_some())
}

/// Drops everything waiting and asks the running batch to stop.
pub fn stop_all() {
    let (gone, cancel) = with_state(|s| (s.queue.cancel_all_queued(), s.cancel.clone()));
    for item in &gone {
        finished(&item.payload, false);
    }
    if let Some(c) = cancel {
        c.cancel();
    }
    changed();
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
        changed();

        let result = run_batch(nix, &batch, &cancel).await;
        let outcomes = with_state(|s| {
            s.cancel = None;
            s.queue.complete(result)
        });

        for o in &outcomes {
            finished(&o.item.payload, o.result.is_ok());
        }
        if !outcomes.is_empty() {
            // One refresh per command, not per package.
            PKGPAGE_BROKER.send(PkgMsg::ProfileBatchDone);
        }
        changed();
    }
    changed();
}

/// Output of the running command: every line goes to the log of its entries;
/// the last few explain a failure.
#[derive(Default)]
struct Lines {
    tail: VecDeque<String>,
}

impl Lines {
    fn push(&mut self, line: &str) {
        let line: String = line.chars().take(queue::MAX_LINE_CHARS).collect();
        with_queue(|q| q.log_push(&line));
        if self.tail.len() == 12 {
            self.tail.pop_front();
        }
        self.tail.push_back(line);
    }

    fn text(&self) -> String {
        self.tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

fn progress(s: &Summary) {
    with_queue(|q| q.set_progress(s.fraction, s.label()));
}

fn profile_sink(lines: &mut Lines) -> impl FnMut(Event) + Send + '_ {
    move |e| match e {
        Event::Line(_, line) => lines.push(&line),
        Event::Progress(s) => progress(&s),
    }
}

async fn run_batch(nix: &Nix, batch: &queue::Batch<Job>, cancel: &Cancel) -> BatchResult {
    let mut lines = Lines::default();
    let res = match batch.action {
        Action::Install => {
            let installables: Result<Vec<String>, String> =
                batch.keys().iter().map(|k| super::sourceinfo::installable(k)).collect();
            match installables {
                Ok(installables) => ops::add_many(nix, &installables, cancel, profile_sink(&mut lines)).await,
                Err(e) => Err(anyhow::anyhow!(e)),
            }
        }
        Action::Remove => match element_names(&batch.keys()).await {
            Ok(names) => ops::remove(nix, &names, cancel, profile_sink(&mut lines)).await,
            Err(e) => Err(e),
        },
        Action::Exclusive => {
            let mut sink = |line: String| lines.push(&line);
            jobs::run(&batch.items[0].payload, cancel, &mut sink).await
        }
    };
    match res {
        Ok(true) => BatchResult::Done,
        _ if cancel.is_cancelled() => BatchResult::Cancelled,
        Ok(false) => {
            let text = lines.text();
            BatchResult::Failed(if text.is_empty() { "nix exited with an error".into() } else { text })
        }
        Err(e) => {
            warn!("Operation failed: {}", e);
            lines.push(&e.to_string());
            BatchResult::Failed(e.to_string())
        }
    }
}

/// Upgrades everything, after removing the entries behind `remove_first`.
pub async fn upgrade_all(cancel: &Cancel, remove_first: &[String], sink: jobs::Sink<'_>) -> Result<bool> {
    let nix = nix().await;
    let on_event = |e: Event| match e {
        Event::Line(_, line) => sink(line),
        Event::Progress(s) => progress(&s),
    };
    if remove_first.is_empty() {
        return ops::upgrade_all(nix, cancel, on_event).await;
    }
    let names = element_names(remove_first).await?;
    ops::remove_then_upgrade_all(nix, &names, cancel, on_event).await
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
    let installed = installed_in_sources(&profile::list(nix().await).await?);
    // Third-party apps are not "missing" just because their catalogue is not fetched yet.
    let mut attrs: Vec<String> = installed
        .into_iter()
        .filter_map(|p| p.nixpkgs_attr)
        .filter(|a| !a.contains(':'))
        .collect();
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
