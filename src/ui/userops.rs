//! The app-wide work queue on top of `nsc-backend`: profile installs and
//! removals run as batches here, everything else as exclusive jobs (`jobs.rs`).
//! The Activity tab and its log modal show what this module holds.

use super::{
    activitylog::LogMsg,
    activitypage::ActivityMsg,
    jobs::{self, Job, JobKind, RunJob, UpdateJob},
    pkgpage::{InstallType, PkgAction, PkgMsg, WorkPkg},
    updatepage::UpdateType,
    window::{SystemPkgs, UserPkgs, ACTIVITYLOG_BROKER, ACTIVITY_BROKER, PKGPAGE_BROKER},
};
use anyhow::{bail, Result};
use log::*;
use nix_data::config::configfile::NixDataConfig;
use nsc_backend::{
    memory,
    launch,
    ops::{self, Nix},
    pkgdb::{self, Unavailable},
    profile,
    progress::Summary,
    queue::{self, Action, BatchResult, Entry, Item, Queue, Start, Status},
    run::{Cancel, Event},
};
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::Notify;

/// A running job ended or an item arrived: the runner looks again.
static WAKE: Notify = Notify::const_new();
/// Memory use takes a moment to show after a job starts; the next one waits this long.
const SETTLE: Duration = Duration::from_secs(2);
/// How often a waiting runner rereads the memory gate.
const GATE_POLL: Duration = Duration::from_secs(3);

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
    #[cfg(feature = "uitest")]
    if std::env::var_os("NSC_UI_TEST").is_some() {
        // Screenshot fixtures: an unfree app, and one whose source is gone.
        let gone = "github:gone/src#packages.x86_64-linux.gone".to_string();
        out.insert("unfree-thing".to_string(), "1.0".to_string());
        out.insert(gone.clone(), "2.0".to_string());
        super::sourceinfo::set_removed([gone].into_iter().collect());
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

/// The queue and the cancel handles of its running jobs by id, under one lock so a
/// cancel can neither miss a job that just started nor hit a later one.
#[derive(Default)]
struct QueueState {
    queue: Queue<Job>,
    cancels: HashMap<u64, Cancel>,
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
/// the busy marks of the installed page always clear. A run installs nothing, so it sends nothing.
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
        WAKE.notify_one();
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

/// Queues running a package without installing it. False when that run is already queued
/// or running.
pub fn enqueue_run(run: RunJob, ctx: Ctx) -> bool {
    let key = run_key(&run.pkg);
    enqueue_job(key, Action::Run, ctx.job(JobKind::Run(run)))
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

/// Queue key of running the package without installing it; never an install's key.
fn run_key(pkg: &str) -> String {
    format!("run:{pkg}")
}

/// The package's queued or running install or removal, else its run (profile only).
fn page_item(q: &Queue<Job>, pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> Option<(Item<Job>, Status)> {
    let by_key = |key: &str| q.key_item(key).map(|(i, s)| (i.clone(), s));
    by_key(&pkg_key(pkg, pkgtype, userpkgs)).or_else(|| {
        (*pkgtype == InstallType::User && *userpkgs == UserPkgs::Profile)
            .then(|| by_key(&run_key(pkg)))
            .flatten()
    })
}

/// What the package's page shows while it is in the queue.
pub fn state_label(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> Option<&'static str> {
    let (item, status) = with_queue(|q| page_item(q, pkg, pkgtype, userpkgs))?;
    Some(match (&item.payload.kind, item.payload.is_remove(), status) {
        (JobKind::Run(_), _, Status::Queued) => "Run queued",
        (JobKind::Run(_), _, Status::Running) => "Preparing…",
        (_, false, Status::Queued) => "Queued",
        (_, false, Status::Running) => "Installing…",
        (_, true, Status::Queued) => "Removal queued",
        (_, true, Status::Running) => "Removing…",
    })
}

/// The queued or running entry for the package, to open its log.
pub fn active_id(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> Option<u64> {
    with_queue(|q| page_item(q, pkg, pkgtype, userpkgs)).map(|(i, _)| i.id)
}

/// A queued entry can always be dropped; a running one only if its job can be stopped.
pub fn can_cancel(entry: &Entry<Job>) -> bool {
    entry.state == queue::EntryState::Queued || entry.payload.cancellable()
}

/// `can_cancel` for the package's queued or running entry.
pub fn pkg_can_cancel(pkg: &str, pkgtype: &InstallType, userpkgs: &UserPkgs) -> bool {
    with_queue(|q| page_item(q, pkg, pkgtype, userpkgs))
        .map_or(false, |(i, s)| s == Status::Queued || i.payload.cancellable())
}

/// Drops the queued entry, or asks the running one to stop. Once Nix has started
/// its commit, it finishes or rolls back by itself.
pub fn cancel(id: u64) {
    let gone = with_state(|s| {
        let gone = s.queue.cancel_queued_id(id);
        if gone.is_none() {
            let running = s.queue.running().iter().find(|b| b.id() == id);
            if running.map_or(false, |b| b.item.payload.cancellable()) {
                if let Some(c) = s.cancels.get(&id) {
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
        WAKE.notify_one();
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
    with_queue(|q| !q.running().is_empty())
}

/// Drops everything waiting and asks the running jobs to stop, those that can be stopped;
/// flagging one that cannot would report its own failure as a cancel.
pub fn stop_all() {
    let gone = with_state(|s| {
        for b in s.queue.running().iter().filter(|b| b.item.payload.cancellable()) {
            if let Some(c) = s.cancels.get(&b.id()) {
                c.cancel();
            }
        }
        s.queue.cancel_all_queued()
    });
    for item in &gone {
        finished(&item.payload, false);
    }
    changed();
}

/// Starts jobs as slots and memory allow until nothing waits. Every failure is
/// reported and the loop goes on.
async fn run_queue() {
    let nix = nix().await;
    let slots = nix.slots();
    loop {
        let admit = memory::has_room();
        let started = with_state(|s| {
            let start = s.queue.start(slots, admit);
            let cancel = Cancel::new();
            if let Start::Run(b) = &start {
                s.cancels.insert(b.id(), cancel.clone());
            }
            (start, cancel)
        });
        match started {
            (Start::Run(batch), cancel) => {
                changed();
                relm4::spawn(run_job(nix, batch, cancel));
                if slots > 1 {
                    tokio::time::sleep(SETTLE).await;
                }
            }
            (Start::Wait, _) => {
                let _ = tokio::time::timeout(GATE_POLL, WAKE.notified()).await;
            }
            (Start::Idle, _) => break,
        }
    }
    changed();
}

async fn run_job(nix: &'static Nix, batch: queue::Batch<Job>, cancel: Cancel) {
    let result = run_batch(nix, &batch, &cancel).await;
    let outcome = with_state(|s| {
        s.cancels.remove(&batch.id());
        s.queue.complete(batch.id(), result)
    });
    if let Some(o) = outcome {
        finished(&o.item.payload, o.result.is_ok());
        // One refresh per completed job; a run changes nothing to refresh.
        if o.item.action != Action::Run {
            PKGPAGE_BROKER.send(PkgMsg::ProfileBatchDone);
        }
    }
    changed();
    WAKE.notify_one();
}

/// Output of one running job: every line goes to its log; the last few explain a failure.
struct Lines {
    id: u64,
    tail: VecDeque<String>,
}

impl Lines {
    fn new(id: u64) -> Self {
        Lines { id, tail: VecDeque::new() }
    }

    fn push(&mut self, line: &str) {
        if ops::is_noise(line) {
            return;
        }
        let line: String = line.chars().take(queue::MAX_LINE_CHARS).collect();
        with_queue(|q| q.log_push(self.id, &line));
        if self.tail.len() == 12 {
            self.tail.pop_front();
        }
        self.tail.push_back(line);
    }

    fn text(&self) -> String {
        self.tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

fn progress(id: u64, s: &Summary) {
    with_queue(|q| q.set_progress(id, s.fraction, s.label()));
}

fn profile_sink(lines: &mut Lines) -> impl FnMut(Event) + Send + '_ {
    move |e| match e {
        Event::Line(_, line) => lines.push(&line),
        Event::Progress(s) => progress(lines.id, &s),
    }
}

async fn run_batch(nix: &Nix, batch: &queue::Batch<Job>, cancel: &Cancel) -> BatchResult {
    let mut lines = Lines::new(batch.id());
    let key = batch.item.key.as_str();
    let res = match batch.action {
        Action::Install => match super::sourceinfo::installable(key) {
            Ok(installable) => ops::add(nix, &installable, cancel, profile_sink(&mut lines)).await,
            Err(e) => Err(anyhow::anyhow!(e)),
        },
        Action::Remove => match element_names(&[key.to_string()]).await {
            Ok(names) => ops::remove(nix, &names, cancel, profile_sink(&mut lines)).await,
            Err(e) => Err(e),
        },
        Action::Run => match &batch.item.payload.kind {
            JobKind::Run(run) => run_app(nix, run, cancel, &mut lines).await,
            _ => Err(anyhow::anyhow!("not a run job")),
        },
        Action::Exclusive => {
            let mut sink = |line: String| lines.push(&line);
            jobs::run(&batch.item.payload, cancel, &mut sink).await
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

/// Builds the package and starts its app or program. The outputs stay in the store without a GC
/// root, so a garbage collection may take them; the next run builds or fetches them again.
async fn run_app(nix: &Nix, run: &RunJob, cancel: &Cancel, lines: &mut Lines) -> Result<bool> {
    let installable = super::sourceinfo::installable(&run.pkg).map_err(anyhow::Error::msg)?;
    let Some(outs) = ops::build(nix, &installable, cancel, profile_sink(lines)).await? else {
        return Ok(false);
    };
    if cancel.is_cancelled() {
        return Ok(false);
    }
    let name = run.display_name();
    let target =
        launch::resolve(nix, &installable, name, &outs, run.desktop_id.as_deref(), cancel, profile_sink(lines)).await?;
    let env = launch::current_env(&outs);
    lines.push(&format!("Starting {}", target.path.display()));
    if target.exec.terminal {
        let cwd = target.exec.cwd.as_ref().map(|c| c.to_string_lossy().into_owned());
        let script = nsc_backend::shell::terminal_script(
            &env.path.to_string_lossy(),
            &env.xdg_data_dirs.to_string_lossy(),
            cwd.as_deref(),
            &target.exec.argv,
        );
        super::term::try_launchterm(&script).map_err(anyhow::Error::msg)?;
    } else {
        launch::spawn_detached(&target.exec, &env)?;
    }
    Ok(true)
}

/// Upgrades everything, after removing the entries behind `remove_first`.
pub async fn upgrade_all(cancel: &Cancel, remove_first: &[String], sink: jobs::Sink<'_>) -> Result<bool> {
    let nix = nix().await;
    // An exclusive job runs alone, so the one running job is this one.
    let id = with_queue(|q| q.running().first().map_or(0, |b| b.id()));
    let on_event = |e: Event| match e {
        Event::Line(_, line) => sink(line),
        Event::Progress(s) => progress(id, &s),
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
