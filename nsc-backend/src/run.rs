//! Runs a program, streams its output and optional Nix progress, supports a
//! polite cancel (SIGINT to the process group).

use crate::progress::{self, Record, Summary, Tree};
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Line(Stream, String),
    Progress(Summary),
}

/// Cloneable cancel handle. Cancelling is idempotent; a run that sees it sends
/// SIGINT to the child's process group once and then waits for it to exit.
#[derive(Clone)]
pub struct Cancel(Arc<watch::Sender<bool>>);

impl Default for Cancel {
    fn default() -> Self {
        Cancel(Arc::new(watch::channel(false).0))
    }
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    async fn cancelled(rx: &mut watch::Receiver<bool>) {
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Process-wide lock: the UI runs one operation at a time.
pub struct OpLock(#[allow(dead_code)] tokio::sync::MutexGuard<'static, ()>);

static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

pub async fn lock() -> OpLock {
    OpLock(LOCK.get_or_init(Default::default).lock().await)
}

/// Runs `program` with the lines of both streams forwarded to `on_event`.
pub async fn run(
    program: &OsStr,
    args: &[String],
    cancel: &Cancel,
    on_event: impl FnMut(Event) + Send,
) -> Result<ExitStatus> {
    run_inner(program, args, cancel, false, on_event).await
}

/// Like [`run`], and also streams `Event::Progress` from Nix's `json-log-path`.
/// Only useful for a Nix that has the setting; other programs just ignore it.
pub async fn run_with_progress(
    program: &OsStr,
    args: &[String],
    cancel: &Cancel,
    on_event: impl FnMut(Event) + Send,
) -> Result<ExitStatus> {
    run_inner(program, args, cancel, cfg!(unix), on_event).await
}

/// How long output may still arrive after the child exited.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

enum Msg {
    Line(Stream, String),
    Json(String),
}

async fn pump<R: AsyncRead + Unpin>(r: R, stream: Option<Stream>, tx: mpsc::UnboundedSender<Msg>) {
    // Bytes, not `lines()`: tools may print invalid UTF-8.
    let mut r = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match r.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n' | b'\r')) {
                    buf.pop();
                }
                let line = String::from_utf8_lossy(&buf).into_owned();
                let msg = match stream {
                    Some(s) => Msg::Line(s, line),
                    None => Msg::Json(line),
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
        }
    }
}

async fn run_inner(
    program: &OsStr,
    args: &[String],
    cancel: &Cancel,
    progress: bool,
    mut on_event: impl FnMut(Event) + Send,
) -> Result<ExitStatus> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let listener = if progress {
        let sock = sock::bind()?;
        let existing = std::env::var("NIX_CONFIG").unwrap_or_default();
        cmd.env(
            "NIX_CONFIG",
            format!("{existing}\njson-log-path = {}", sock.path().display()),
        );
        Some(sock)
    } else {
        None
    };

    let mut child = Command::from(cmd)
        .spawn()
        .with_context(|| format!("failed to start {}", program.to_string_lossy()))?;
    let pid = child.id();

    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(pump(child.stdout.take().unwrap(), Some(Stream::Stdout), tx.clone()));
    tokio::spawn(pump(child.stderr.take().unwrap(), Some(Stream::Stderr), tx.clone()));
    // Dropped once the child exited, so `rx` closes when every reader is done.
    let mut tx = Some(tx);

    let mut tree = Tree::default();
    let mut last = Summary::default();
    let mut cancel_rx = cancel.0.subscribe();
    let mut interrupted = false;
    let mut status: Option<std::io::Result<ExitStatus>> = None;
    // Set once the child exited: a grandchild may keep the pipes open.
    let mut drain_deadline: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Msg::Line(s, l)) => on_event(Event::Line(s, l)),
                Some(Msg::Json(l)) => {
                    if let Some(rec) = progress::parse_line(&l) {
                        tree.apply(&rec);
                        if !matches!(rec, Record::Msg { .. }) {
                            let s = tree.summary();
                            if s != last {
                                last = s.clone();
                                on_event(Event::Progress(s));
                            }
                        }
                    }
                }
                None => break,
            },
            st = child.wait(), if status.is_none() => {
                status = Some(st);
                // Connections made just before exit may still sit in the backlog.
                if let (Some(l), Some(tx)) = (&listener, &tx) {
                    while let Ok(Ok(conn)) =
                        tokio::time::timeout(std::time::Duration::ZERO, l.accept()).await
                    {
                        tokio::spawn(pump(conn, None, tx.clone()));
                    }
                }
                tx = None;
                drain_deadline = Some(tokio::time::Instant::now() + DRAIN_TIMEOUT);
            },
            _ = async { tokio::time::sleep_until(drain_deadline.unwrap()).await }, if drain_deadline.is_some() => break,
            _ = Cancel::cancelled(&mut cancel_rx), if !interrupted && status.is_none() => {
                interrupted = true;
                interrupt(pid);
            },
            conn = sock::accept(&listener), if listener.is_some() && status.is_none() => {
                if let (Ok(conn), Some(tx)) = (conn, &tx) {
                    tokio::spawn(pump(conn, None, tx.clone()));
                }
            },
        }
    }

    Ok(status.context("child status missing")??)
}

#[cfg(unix)]
fn interrupt(pid: Option<u32>) {
    if let Some(pid) = pid {
        // The child leads its own group, so this reaches its helpers too.
        unsafe { libc::killpg(pid as i32, libc::SIGINT) };
    }
}

#[cfg(not(unix))]
fn interrupt(_pid: Option<u32>) {}

#[cfg(unix)]
mod sock {
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::net::{UnixListener, UnixStream};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Listening socket, unlinked on drop.
    pub struct Sock {
        listener: UnixListener,
        path: PathBuf,
    }

    impl Sock {
        pub fn path(&self) -> &Path {
            &self.path
        }

        pub async fn accept(&self) -> std::io::Result<UnixStream> {
            self.listener.accept().await.map(|(s, _)| s)
        }
    }

    impl Drop for Sock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub async fn accept(l: &Option<Sock>) -> std::io::Result<UnixStream> {
        match l {
            Some(s) => s.accept().await,
            None => std::future::pending().await,
        }
    }

    pub fn bind() -> Result<Sock> {
        let name = format!(
            "nsc-{}-{}.sock",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        // sun_path holds ~108 bytes; stay well under.
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|d| d.is_dir() && d.join(&name).as_os_str().len() < 100)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let path = dir.join(name);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("binding progress socket {}", path.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(Sock { listener, path })
    }
}

/// Non-unix stub so the crate type-checks; progress is never requested there.
#[cfg(not(unix))]
mod sock {
    use anyhow::Result;
    use std::path::Path;

    pub struct Sock;

    impl Sock {
        pub fn path(&self) -> &Path {
            Path::new("")
        }

        pub async fn accept(&self) -> std::io::Result<tokio::io::Empty> {
            std::future::pending().await
        }
    }

    pub async fn accept(_: &Option<Sock>) -> std::io::Result<tokio::io::Empty> {
        std::future::pending().await
    }

    pub fn bind() -> Result<Sock> {
        anyhow::bail!("progress sockets need unix")
    }
}
