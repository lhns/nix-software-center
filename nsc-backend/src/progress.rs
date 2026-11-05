//! Nix JSON log records (`json-log-path` / `--log-format internal-json`)
//! merged into one activity tree.

use serde_json::Value;
use std::collections::HashMap;

pub const ACT_FILE_TRANSFER: u64 = 101;
pub const ACT_COPY_PATHS: u64 = 103;
pub const ACT_BUILDS: u64 = 104;
pub const RES_PROGRESS: u64 = 105;

#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Start {
        id: u64,
        parent: u64,
        kind: u64,
        text: String,
        fields: Vec<Value>,
    },
    Stop {
        id: u64,
    },
    Result {
        id: u64,
        kind: u64,
        fields: Vec<Value>,
    },
    Msg {
        level: u64,
        msg: String,
    },
}

/// Parses one log line; the `@nix ` prefix is optional. `None` for anything else.
pub fn parse_line(line: &str) -> Option<Record> {
    let line = line.trim();
    let line = line.strip_prefix("@nix ").unwrap_or(line);
    if !line.starts_with('{') {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let num = |k: &str| v.get(k).and_then(Value::as_u64);
    let fields = || {
        v.get("fields")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    match v.get("action")?.as_str()? {
        "start" => Some(Record::Start {
            id: num("id")?,
            parent: num("parent").unwrap_or(0),
            kind: num("type").unwrap_or(0),
            text: v.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
            fields: fields(),
        }),
        "stop" => Some(Record::Stop { id: num("id")? }),
        "result" => Some(Record::Result {
            id: num("id")?,
            kind: num("type")?,
            fields: fields(),
        }),
        "msg" => Some(Record::Msg {
            level: num("level").unwrap_or(0),
            msg: v.get("msg").and_then(Value::as_str).unwrap_or("").to_string(),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    /// Overall completion in 0..=1, when Nix reported expected work.
    pub fraction: Option<f64>,
    /// Text of the most recently started running activity.
    pub status: Option<String>,
    /// (done, expected) top-level build count.
    pub builds: (u64, u64),
    /// (done, expected) top-level copy (substitution) count.
    pub downloads: (u64, u64),
    /// (done, expected) bytes over all file transfers.
    pub bytes: (u64, u64),
    /// Activities started / stopped so far.
    pub started: u64,
    pub stopped: u64,
}

impl Summary {
    /// One-line status. Until Nix reports expected work it is evaluating, which
    /// can take a while and has no numbers to show.
    pub fn label(&self) -> String {
        let mut parts = vec![];
        if self.fraction.is_none() {
            parts.push("Evaluating…".to_string());
        }
        if let Some(s) = &self.status {
            parts.push(s.clone());
        }
        if self.builds.1 > 0 {
            parts.push(format!("builds {}/{}", self.builds.0, self.builds.1));
        }
        if self.downloads.1 > 0 {
            parts.push(format!("downloads {}/{}", self.downloads.0, self.downloads.1));
        }
        parts.join(" — ")
    }
}

#[derive(Debug)]
struct Activity {
    parent: u64,
    kind: u64,
    text: String,
    running: bool,
    seq: u64,
    /// done, expected, running, failed
    progress: [u64; 4],
}

/// Activities of all connections; ids are unique across Nix processes
/// (pid in the upper 32 bits). Stopped activities are kept for their totals.
#[derive(Debug, Default)]
pub struct Tree {
    acts: HashMap<u64, Activity>,
    seq: u64,
}

impl Tree {
    pub fn apply(&mut self, rec: &Record) {
        match rec {
            Record::Start {
                id,
                parent,
                kind,
                text,
                ..
            } => {
                self.seq += 1;
                self.acts.insert(
                    *id,
                    Activity {
                        parent: *parent,
                        kind: *kind,
                        text: text.clone(),
                        running: true,
                        seq: self.seq,
                        progress: [0; 4],
                    },
                );
            }
            Record::Stop { id } => {
                if let Some(a) = self.acts.get_mut(id) {
                    a.running = false;
                }
            }
            Record::Result { id, kind, fields } if *kind == RES_PROGRESS => {
                if let Some(a) = self.acts.get_mut(id) {
                    for (slot, f) in a.progress.iter_mut().zip(fields) {
                        *slot = f.as_u64().unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn summary(&self) -> Summary {
        let mut s = Summary::default();
        let mut newest: Option<&Activity> = None;
        for a in self.acts.values() {
            s.started += 1;
            if !a.running {
                s.stopped += 1;
            } else if !a.text.is_empty() && newest.map_or(true, |n| a.seq > n.seq) {
                newest = Some(a);
            }
            let (done, expected) = (a.progress[0], a.progress[1]);
            match a.kind {
                ACT_BUILDS if a.parent == 0 => {
                    s.builds.0 += done;
                    s.builds.1 += expected;
                }
                ACT_COPY_PATHS if a.parent == 0 => {
                    s.downloads.0 += done;
                    s.downloads.1 += expected;
                }
                ACT_FILE_TRANSFER => {
                    s.bytes.0 += done;
                    s.bytes.1 += expected;
                }
                _ => {}
            }
        }
        s.status = newest.map(|a| a.text.clone());
        let expected = s.builds.1 + s.downloads.1;
        if expected > 0 {
            let done = s.builds.0 + s.downloads.0;
            s.fraction = Some((done as f64 / expected as f64).min(1.0));
        }
        s
    }
}
