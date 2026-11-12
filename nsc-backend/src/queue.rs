//! The app-wide work queue as a state machine, free of UI and processes.
//!
//! Up to `slots` jobs run at once, one package per job (a run-without-installing job
//! counts like an install or removal: same slots and memory gate, never batched). A job never starts
//! while one with the same key is running or ahead of it, so a package is
//! never reordered against itself. A failure never stops the queue.
//! `Exclusive` items (updates, system and nix-env operations) run alone and
//! are a barrier: nothing starts while one runs, and nothing starts past one.
//!
//! `start` is told whether memory allows another job (`admit`); when it does
//! not, the job waits in the queue, except that an idle queue always starts one.
//!
//! Finished items stay in a bounded history, with their log, until removed.

use std::collections::{HashMap, VecDeque};

/// Jobs that may run at once.
pub const MAX_SLOTS: usize = 4;
/// Most finished entries kept; at the cap the oldest non-failed one goes first.
pub const MAX_FINISHED: usize = 50;
/// A log keeps at most this many lines and bytes ...
pub const MAX_LOG_LINES: usize = 5000;
pub const MAX_LOG_BYTES: usize = 1 << 20;
/// ... each cut to this many characters ...
pub const MAX_LINE_CHARS: usize = 1000;
/// ... and shrinks to this tail once its entry has finished.
pub const FINISHED_LOG_BYTES: usize = 256 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Install,
    Remove,
    /// Builds a package into the store and starts it; the profile is untouched.
    Run,
    /// Runs alone, never batched.
    Exclusive,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item<T> {
    /// Stable for the item's life, retries included. Assigned by `enqueue`.
    pub id: u64,
    pub key: String,
    pub action: Action,
    /// Whatever the caller needs back with the outcome.
    pub payload: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch<T> {
    pub action: Action,
    pub item: Item<T>,
}

impl<T> Batch<T> {
    /// What `complete`, `set_progress` and `log_push` take.
    pub fn id(&self) -> u64 {
        self.item.id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Start<T> {
    Run(Batch<T>),
    /// Items wait, but none may start now.
    Wait,
    /// Nothing waits; the runner claim is released.
    Idle,
}

impl<T> Start<T> {
    pub fn batch(self) -> Option<Batch<T>> {
        match self {
            Start::Run(b) => Some(b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BatchResult {
    Done,
    /// Nix's error text.
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome<T> {
    pub item: Item<T>,
    /// The error text on failure.
    pub result: Result<(), String>,
    /// Stopped on request; `result` is then an error too.
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Progress {
    /// `None` when the total is unknown.
    pub fraction: Option<f64>,
    pub label: String,
}

/// An item as the activity list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<T> {
    pub id: u64,
    pub key: String,
    pub action: Action,
    pub payload: T,
    pub state: EntryState,
    /// Queued only: entries that run before this one (a rough count, batching can merge them).
    pub ahead: usize,
    /// Running only.
    pub progress: Option<Progress>,
}

/// Bounded text log. Lines are numbered for ever, so a reader can ask for what
/// it has not seen even after old lines were dropped.
#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    bytes: usize,
    /// Lines ever pushed.
    total: u64,
}

impl LogBuffer {
    pub fn push(&mut self, line: &str) {
        let line: String = line.chars().take(MAX_LINE_CHARS).collect();
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        self.total += 1;
        self.trim(MAX_LOG_BYTES);
    }

    /// Drops the oldest lines until the log fits `max_bytes` and the line limit.
    pub fn trim(&mut self, max_bytes: usize) {
        while self.lines.len() > MAX_LOG_LINES || (self.bytes > max_bytes && self.lines.len() > 1) {
            let Some(old) = self.lines.pop_front() else { break };
            self.bytes -= old.len() + 1;
        }
    }

    /// Lines after the first `seen`, and the count to pass next time.
    pub fn since(&self, seen: u64) -> (Vec<String>, u64) {
        let first = self.total - self.lines.len() as u64;
        let skip = seen.saturating_sub(first).min(self.lines.len() as u64) as usize;
        (self.lines.iter().skip(skip).cloned().collect(), self.total)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

#[derive(Debug)]
struct Finished<T> {
    item: Item<T>,
    state: EntryState,
    /// Not a failure, or one the user has already looked at.
    seen: bool,
}

#[derive(Debug)]
pub struct Queue<T> {
    pending: VecDeque<Item<T>>,
    running: Vec<Batch<T>>,
    progress: HashMap<u64, Progress>,
    /// Oldest first.
    finished: VecDeque<Finished<T>>,
    logs: HashMap<u64, LogBuffer>,
    next_id: u64,
    runner: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue {
            pending: VecDeque::new(),
            running: vec![],
            progress: HashMap::new(),
            finished: VecDeque::new(),
            logs: HashMap::new(),
            next_id: 1,
            runner: false,
        }
    }
}

fn same<T>(i: &Item<T>, key: &str, action: Action) -> bool {
    i.key == key && i.action == action
}

impl<T: Clone> Queue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// False, and nothing added, when the same key and action is already queued
    /// or running. The item's `id` is replaced by a fresh one.
    pub fn enqueue(&mut self, mut item: Item<T>) -> bool {
        if self.status(&item.key, item.action).is_some() {
            return false;
        }
        item.id = self.next_id;
        self.next_id += 1;
        self.logs.insert(item.id, LogBuffer::default());
        self.pending.push_back(item);
        true
    }

    pub fn status(&self, key: &str, action: Action) -> Option<Status> {
        if self.running.iter().any(|b| same(&b.item, key, action)) {
            Some(Status::Running)
        } else if self.pending.iter().any(|i| same(i, key, action)) {
            Some(Status::Queued)
        } else {
            None
        }
    }

    /// What is happening to `key`, for its page: the running action first, else the newest queued one.
    pub fn key_status(&self, key: &str) -> Option<(Action, Status)> {
        self.key_item(key).map(|(i, s)| (i.action, s))
    }

    /// The item behind `key_status`.
    pub fn key_item(&self, key: &str) -> Option<(&Item<T>, Status)> {
        if let Some(b) = self.running.iter().find(|b| b.item.key == key) {
            return Some((&b.item, Status::Running));
        }
        self.pending
            .iter()
            .rev()
            .find(|i| i.key == key)
            .map(|i| (i, Status::Queued))
    }

    /// Drops the queued, not yet started items for `key`; they become Cancelled in the history.
    pub fn cancel_queued(&mut self, key: &str) -> Vec<Item<T>> {
        let (gone, keep): (VecDeque<_>, VecDeque<_>) = self.pending.drain(..).partition(|i| i.key == key);
        self.pending = keep;
        for item in &gone {
            self.record(item.clone(), EntryState::Cancelled);
        }
        gone.into_iter().collect()
    }

    /// Like `cancel_queued`, for the one queued item `id`.
    pub fn cancel_queued_id(&mut self, id: u64) -> Option<Item<T>> {
        let at = self.pending.iter().position(|i| i.id == id)?;
        let item = self.pending.remove(at)?;
        self.record(item.clone(), EntryState::Cancelled);
        Some(item)
    }

    /// Drops every queued item (running jobs are not touched).
    pub fn cancel_all_queued(&mut self) -> Vec<Item<T>> {
        let items: Vec<_> = self.pending.drain(..).collect();
        for item in &items {
            self.record(item.clone(), EntryState::Cancelled);
        }
        items
    }

    /// Items waiting, not counting the running jobs.
    pub fn waiting(&self) -> usize {
        self.pending.len()
    }

    /// Running plus waiting items.
    pub fn active(&self) -> usize {
        self.waiting() + self.running.len()
    }

    pub fn running(&self) -> &[Batch<T>] {
        &self.running
    }

    /// True for the caller that must start the runner loop; `start` returning
    /// `None` releases it.
    pub fn claim_runner(&mut self) -> bool {
        !std::mem::replace(&mut self.runner, true)
    }

    /// Starts the oldest item that may run. `slots` caps the running jobs;
    /// `admit` is false while memory is short.
    pub fn start(&mut self, slots: usize, admit: bool) -> Start<T> {
        if self.pending.is_empty() {
            self.runner = false;
            return Start::Idle;
        }
        let idle = self.running.is_empty();
        let exclusive = self.running.iter().any(|b| b.action == Action::Exclusive);
        if exclusive || self.running.len() >= slots.max(1) || !(admit || idle) {
            return Start::Wait;
        }
        let mut at = None;
        for (n, i) in self.pending.iter().enumerate() {
            if i.action == Action::Exclusive {
                at = idle.then_some(n);
                break;
            }
            let busy = self.running.iter().any(|b| b.item.key == i.key)
                || self.pending.iter().take(n).any(|p| p.key == i.key);
            if !busy {
                at = Some(n);
                break;
            }
        }
        let Some(item) = at.and_then(|n| self.pending.remove(n)) else {
            return Start::Wait;
        };
        let batch = Batch { action: item.action, item };
        self.progress.insert(batch.id(), Progress::default());
        self.running.push(batch.clone());
        Start::Run(batch)
    }

    /// Ends the running job `id`; `None` when no such job runs.
    pub fn complete(&mut self, id: u64, result: BatchResult) -> Option<Outcome<T>> {
        let at = self.running.iter().position(|b| b.id() == id)?;
        let item = self.running.remove(at).item;
        self.progress.remove(&id);
        let (err, cancelled) = match result {
            BatchResult::Done => (None, false),
            BatchResult::Cancelled => (Some("Cancelled".to_string()), true),
            BatchResult::Failed(text) => (Some(text), false),
        };
        let state = match (&err, cancelled) {
            (None, _) => EntryState::Done,
            (Some(_), true) => EntryState::Cancelled,
            (Some(text), false) => EntryState::Failed(text.clone()),
        };
        self.record(item.clone(), state);
        Some(Outcome { item, result: err.map_or(Ok(()), Err), cancelled })
    }

    fn record(&mut self, item: Item<T>, state: EntryState) {
        if let Some(log) = self.logs.get_mut(&item.id) {
            log.trim(FINISHED_LOG_BYTES);
        }
        if self.finished.len() >= MAX_FINISHED {
            let at = self.finished.iter().position(|f| !matches!(f.state, EntryState::Failed(_))).unwrap_or(0);
            if let Some(f) = self.finished.remove(at) {
                self.logs.remove(&f.item.id);
            }
        }
        let seen = !matches!(state, EntryState::Failed(_));
        self.finished.push_back(Finished { item, state, seen });
    }

    /// Puts a Failed or Cancelled entry back at the end of the queue under its
    /// id, keeping its log. `prepare` adjusts the payload first. False, leaving
    /// the entry, when there is no such entry or its key and action is queued
    /// or running.
    pub fn retry(&mut self, id: u64, prepare: impl FnOnce(&mut T)) -> bool {
        let Some(at) = self.finished.iter().position(|f| f.item.id == id) else {
            return false;
        };
        let f = &self.finished[at];
        if !matches!(f.state, EntryState::Failed(_) | EntryState::Cancelled)
            || self.status(&f.item.key, f.item.action).is_some()
        {
            return false;
        }
        let Some(Finished { mut item, .. }) = self.finished.remove(at) else {
            return false;
        };
        prepare(&mut item.payload);
        self.log_line(item.id, "--- Retry ---");
        self.pending.push_back(item);
        true
    }

    /// Drops a finished entry and its log.
    pub fn remove(&mut self, id: u64) -> bool {
        let Some(at) = self.finished.iter().position(|f| f.item.id == id) else {
            return false;
        };
        self.finished.remove(at);
        self.logs.remove(&id);
        true
    }

    /// Drops every finished entry; the count.
    pub fn clear_finished(&mut self) -> usize {
        let n = self.finished.len();
        for f in self.finished.drain(..) {
            self.logs.remove(&f.item.id);
        }
        n
    }

    /// Failures the user has not seen yet.
    pub fn unseen_failures(&self) -> usize {
        self.finished.iter().filter(|f| !f.seen).count()
    }

    pub fn mark_failures_seen(&mut self) {
        for f in &mut self.finished {
            f.seen = true;
        }
    }

    /// Every entry in display order: running, up next, then finished, newest first.
    pub fn entries(&self) -> Vec<Entry<T>> {
        let running = self.running.iter().map(|b| self.entry(&b.item, EntryState::Running, 0));
        let ahead = self.running.len();
        let queued = self.pending.iter().enumerate().map(|(n, i)| self.entry(i, EntryState::Queued, ahead + n));
        let finished = self.finished.iter().rev().map(|f| self.entry(&f.item, f.state.clone(), 0));
        running.chain(queued).chain(finished).collect()
    }

    pub fn entry_by_id(&self, id: u64) -> Option<Entry<T>> {
        self.entries().into_iter().find(|e| e.id == id)
    }

    fn entry(&self, i: &Item<T>, state: EntryState, ahead: usize) -> Entry<T> {
        Entry {
            id: i.id,
            key: i.key.clone(),
            action: i.action,
            payload: i.payload.clone(),
            progress: self.progress.get(&i.id).filter(|_| state == EntryState::Running).cloned(),
            state,
            ahead,
        }
    }

    pub fn set_progress(&mut self, id: u64, fraction: Option<f64>, label: String) {
        if let Some(p) = self.progress.get_mut(&id) {
            *p = Progress { fraction, label };
        }
    }

    /// Appends a line to the log of the running job `id`.
    pub fn log_push(&mut self, id: u64, line: &str) {
        if self.running.iter().any(|b| b.id() == id) {
            self.log_line(id, line);
        }
    }

    fn log_line(&mut self, id: u64, line: &str) {
        self.logs.entry(id).or_default().push(line);
    }

    /// Lines of `id`'s log after the first `seen` ones, and the count to pass next time.
    pub fn log_since(&self, id: u64, seen: u64) -> (Vec<String>, u64) {
        self.logs.get(&id).map_or((vec![], 0), |l| l.since(seen))
    }

    /// The log of `id`, for inspection.
    pub fn log(&self, id: u64) -> Option<&LogBuffer> {
        self.logs.get(&id)
    }
}
