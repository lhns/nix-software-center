//! The app-wide install queue as a state machine, free of UI and processes.
//!
//! One batch runs at a time (Nix serialises profile commits). `start` takes
//! the next batch: all queued items with the action of the oldest one, minus
//! those behind an opposite action on the same key, so a package is never
//! reordered against itself. A failed batch of several is retried one item
//! at a time before anything newer runs; a failure never stops the queue.

use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Install,
    Remove,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item<T> {
    pub key: String,
    pub action: Action,
    /// Whatever the caller needs back with the outcome.
    pub payload: T,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch<T> {
    pub action: Action,
    pub items: Vec<Item<T>>,
    /// A single item re-run after its batch failed.
    pub retry: bool,
}

impl<T> Batch<T> {
    pub fn keys(&self) -> Vec<String> {
        self.items.iter().map(|i| i.key.clone()).collect()
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
}

#[derive(Debug)]
pub struct Queue<T> {
    pending: VecDeque<Item<T>>,
    retry: VecDeque<Item<T>>,
    running: Option<Batch<T>>,
    runner: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue {
            pending: VecDeque::new(),
            retry: VecDeque::new(),
            running: None,
            runner: false,
        }
    }
}

impl<T: Clone> Queue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// False, and nothing added, when the same key and action is already queued or running.
    pub fn enqueue(&mut self, item: Item<T>) -> bool {
        if self.status(&item.key, item.action).is_some() {
            return false;
        }
        self.pending.push_back(item);
        true
    }

    pub fn status(&self, key: &str, action: Action) -> Option<Status> {
        if self.running.as_ref().map_or(false, |b| b.items.iter().any(|i| i.key == key && i.action == action)) {
            Some(Status::Running)
        } else if self.retry.iter().chain(&self.pending).any(|i| i.key == key && i.action == action) {
            Some(Status::Queued)
        } else {
            None
        }
    }

    /// What is happening to `key`, for its page: the running action first, else the newest queued one.
    pub fn key_status(&self, key: &str) -> Option<(Action, Status)> {
        if let Some(i) = self.running.as_ref().and_then(|b| b.items.iter().find(|i| i.key == key)) {
            return Some((i.action, Status::Running));
        }
        self.retry
            .iter()
            .chain(&self.pending)
            .rev()
            .find(|i| i.key == key)
            .map(|i| (i.action, Status::Queued))
    }

    /// Drops the queued, not yet started items for `key`.
    pub fn cancel_queued(&mut self, key: &str) -> Vec<Item<T>> {
        let mut gone = vec![];
        for q in [&mut self.retry, &mut self.pending] {
            let (hit, keep): (VecDeque<_>, VecDeque<_>) = q.drain(..).partition(|i| i.key == key);
            *q = keep;
            gone.extend(hit);
        }
        gone
    }

    /// Items waiting, not counting the running batch.
    pub fn waiting(&self) -> usize {
        self.retry.len() + self.pending.len()
    }

    pub fn running(&self) -> Option<&Batch<T>> {
        self.running.as_ref()
    }

    /// True for the caller that must start the runner loop; `start` returning
    /// `None` releases it.
    pub fn claim_runner(&mut self) -> bool {
        !std::mem::replace(&mut self.runner, true)
    }

    pub fn start(&mut self) -> Option<Batch<T>> {
        let batch = if let Some(item) = self.retry.pop_front() {
            Batch { action: item.action, items: vec![item], retry: true }
        } else if let Some(action) = self.pending.front().map(|i| i.action) {
            let mut blocked = HashSet::new();
            let mut items = vec![];
            for i in std::mem::take(&mut self.pending) {
                if i.action == action && !blocked.contains(&i.key) {
                    items.push(i);
                } else {
                    if i.action != action {
                        blocked.insert(i.key.clone());
                    }
                    self.pending.push_back(i);
                }
            }
            Batch { action, items, retry: false }
        } else {
            self.runner = false;
            return None;
        };
        self.running = Some(batch.clone());
        Some(batch)
    }

    /// Ends the running batch. A failed batch of several yields no outcomes:
    /// its items are retried one by one first.
    pub fn complete(&mut self, result: BatchResult) -> Vec<Outcome<T>> {
        let Some(batch) = self.running.take() else {
            return vec![];
        };
        let err = match result {
            BatchResult::Done => None,
            BatchResult::Cancelled => Some("Cancelled".to_string()),
            BatchResult::Failed(text) => {
                if batch.items.len() > 1 {
                    for item in batch.items.into_iter().rev() {
                        self.retry.push_front(item);
                    }
                    return vec![];
                }
                Some(text)
            }
        };
        batch
            .items
            .into_iter()
            .map(|item| Outcome { item, result: err.clone().map_or(Ok(()), Err) })
            .collect()
    }
}
