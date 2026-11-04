//! What the app-wide queue runs besides profile batches: updates and the
//! system and nix-env package operations. These are `Action::Exclusive`.

use super::{
    pkgpage::{InstallType, PkgAction, WorkPkg},
    updatepage::UpdateType,
    userops,
    window::{SystemPkgs, UserPkgs},
};
use anyhow::{anyhow, Result};
use log::*;
use nix_data::config::configfile::NixDataConfig;
use nsc_backend::run::Cancel;
use std::{fs, path::Path, process::Stdio};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// Where a job's output lines go.
pub type Sink<'a> = &'a mut (dyn FnMut(String) + Send);

/// The queue's payload: one unit of work with the settings it needs, as they were when it was queued.
#[derive(Debug, Clone)]
pub struct Job {
    pub kind: JobKind,
    pub config: NixDataConfig,
    pub syspkgs: SystemPkgs,
    pub userpkgs: UserPkgs,
}

#[derive(Debug, Clone)]
pub enum JobKind {
    Pkg(WorkPkg),
    Update(UpdateJob),
}

#[derive(Debug, Clone)]
pub struct UpdateJob {
    pub scope: UpdateType,
    /// Unavailable packages to remove first; `None` removes nothing.
    pub rm_user: Option<Vec<String>>,
    pub rm_sys: Option<Vec<String>>,
}

impl UpdateJob {
    pub fn title(&self) -> &'static str {
        match self.scope {
            UpdateType::System => "Update system",
            UpdateType::User => "Update user packages",
            UpdateType::All => "Update everything",
        }
    }
}

#[allow(dead_code)]
enum NscCmd {
    Rebuild,
    Channel,
    All,
}

impl Job {
    pub fn is_remove(&self) -> bool {
        matches!(&self.kind, JobKind::Pkg(w) if w.action == PkgAction::Remove)
    }

    /// Name of the package, for titles.
    fn name(w: &WorkPkg) -> &str {
        if w.pname.is_empty() {
            &w.pkg
        } else {
            &w.pname
        }
    }

    pub fn title(&self) -> String {
        match &self.kind {
            JobKind::Pkg(w) => {
                let what = if w.action == PkgAction::Install { "Install" } else { "Remove" };
                format!("{what} {}", Self::name(w))
            }
            JobKind::Update(u) => u.title().to_string(),
        }
    }

    /// Where the work happens.
    pub fn target(&self) -> &'static str {
        match &self.kind {
            JobKind::Pkg(w) => match (&w.pkgtype, &self.userpkgs) {
                (InstallType::System, _) => "System",
                (InstallType::User, UserPkgs::Env) => "nix-env",
                (InstallType::User, UserPkgs::Profile) => "Profile",
            },
            JobKind::Update(_) => "Update",
        }
    }

    /// Label of the footer button that cancels it.
    pub fn cancel_label(&self) -> &'static str {
        match &self.kind {
            JobKind::Pkg(w) if w.action == PkgAction::Install => "Cancel Install",
            JobKind::Pkg(_) => "Cancel Removal",
            JobKind::Update(_) => "Cancel Update",
        }
    }

    /// Whether a running job can be stopped: only Nix profile operations; a
    /// pkexec step or nix-env cannot be interrupted from here.
    pub fn cancellable(&self) -> bool {
        match &self.kind {
            JobKind::Pkg(w) => w.pkgtype == InstallType::User && self.userpkgs == UserPkgs::Profile,
            JobKind::Update(u) => match u.scope {
                UpdateType::User => self.userpkgs == UserPkgs::Profile,
                _ => false,
            },
        }
    }
}

/// Runs an exclusive job; `Ok(false)` is a failure with the output as explanation.
pub async fn run(job: &Job, cancel: &Cancel, sink: Sink<'_>) -> Result<bool> {
    let config = job.config.clone();
    match &job.kind {
        JobKind::Pkg(work) => match work.pkgtype {
            InstallType::User => envop(work, sink).await,
            InstallType::System => {
                if config.systemconfig.is_none() {
                    return Err(anyhow!("No system configuration is set up"));
                }
                installsys(work.pkg.to_string(), work.action.clone(), config, sink).await
            }
        },
        JobKind::Update(u) => match u.scope {
            UpdateType::System => runcmd(NscCmd::All, config, job.syspkgs.clone(), u.rm_sys.clone(), sink).await,
            UpdateType::User => updateuser(job, u.rm_user.clone(), cancel, sink).await,
            UpdateType::All => {
                if runcmd(NscCmd::All, config, job.syspkgs.clone(), u.rm_sys.clone(), &mut *sink).await? {
                    updateuser(job, u.rm_user.clone(), cancel, sink).await
                } else {
                    Ok(false)
                }
            }
        },
    }
}

async fn updateuser(job: &Job, rm: Option<Vec<String>>, cancel: &Cancel, sink: Sink<'_>) -> Result<bool> {
    match job.userpkgs {
        UserPkgs::Env => updateenv(sink).await,
        UserPkgs::Profile => userops::upgrade_all(cancel, &rm.unwrap_or_default(), sink).await,
    }
}

async fn envop(work: &WorkPkg, sink: Sink<'_>) -> Result<bool> {
    let _lock = nsc_backend::run::lock().await;
    let mut cmd = tokio::process::Command::new("nix-env");
    match work.action {
        PkgAction::Install => cmd.arg("-iA").arg(format!("nixos.{}", work.pkg)),
        PkgAction::Remove => cmd.arg("-e").arg(&work.pname),
    };
    let mut p = cmd
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = p.stderr.take().unwrap();
    let reader = tokio::io::BufReader::new(stderr);

    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        trace!("CAUGHT LINE: {}", line);
        sink(line);
    }
    Ok(p.wait().await?.success())
}

async fn installsys(
    pkg: String,
    action: PkgAction,
    config: NixDataConfig,
    sink: Sink<'_>,
) -> Result<bool> {
    let _lock = nsc_backend::run::lock().await;
    let systemconfig = config.systemconfig.unwrap_or_default();
    let flakeargs = if let Some(flake) = config.flake {
        if let Some(flakearg) = config.flakearg {
            Some(format!("{}#{}", flake, flakearg))
        } else {
            Some(flake)
        }
    } else {
        None
    };
    let mut p = pkg;
    let f = fs::read_to_string(&systemconfig)?;
    if let Ok(s) = nix_editor::read::getwithvalue(&f, "environment.systemPackages") {
        if !s.contains(&"pkgs".to_string()) {
            p = format!("pkgs.{}", p);
        }
    } else {
        p = format!("pkgs.{}", p);
    }

    let out = match action {
        PkgAction::Install => {
            match nix_editor::write::addtoarr(&f, "environment.systemPackages", vec![p]) {
                Ok(x) => x,
                Err(_) => {
                    return Err(anyhow!("Failed to write configuration.nix"));
                }
            }
        }
        PkgAction::Remove => {
            match nix_editor::write::rmarr(&f, "environment.systemPackages", vec![p]) {
                Ok(x) => x,
                Err(_) => {
                    return Err(anyhow!("Failed to write configuration.nix"));
                }
            }
        }
    };

    let exe = match std::env::current_exe() {
        Ok(mut e) => {
            e.pop(); // root/bin
            e.pop(); // root/
            e.push("libexec"); // root/libexec
            e.push("nsc-helper");
            let x = e.to_string_lossy().to_string();
            info!("nsc-helper path: {}", x);
            if Path::new(&x).is_file() {
                x
            } else {
                String::from("nsc-helper")
            }
        }
        Err(_) => String::from("nsc-helper"),
    };

    let rebuildargs = if let Some(x) = flakeargs {
        let mut v = vec![String::from("--flake")];
        for arg in x.split(' ') {
            if !arg.is_empty() {
                v.push(String::from(arg));
            }
        }
        v
    } else {
        vec![]
    };

    let mut cmd = tokio::process::Command::new("pkexec")
        .arg(&exe)
        .arg("config")
        .arg("--generations")
        .arg(config.generations.unwrap_or(0).to_string())
        .arg("--output")
        .arg(&systemconfig)
        .arg("--")
        .arg("switch")
        .arg("--impure")
        .args(&rebuildargs)
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()?;

    cmd.stdin.take().unwrap().write_all(out.as_bytes()).await?;
    let stderr = cmd.stderr.take().unwrap();
    let reader = tokio::io::BufReader::new(stderr);

    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        trace!("CAUGHT LINE: {}", line);
        sink(line);
    }
    if cmd.wait().await?.success() {
        Ok(true)
    } else {
        Ok(false)
    }
}

async fn runcmd(
    cmd: NscCmd,
    config: NixDataConfig,
    syspkgs: SystemPkgs,
    rmpkgs: Option<Vec<String>>,
    sink: Sink<'_>,
) -> Result<bool> {
    if syspkgs == SystemPkgs::None {
        return Ok(true);
    }
    let _lock = nsc_backend::run::lock().await;
    let systemconfig = config.systemconfig.unwrap_or_default();
    let flakeargs = if let Some(flake) = config.flake {
        if let Some(flakearg) = config.flakearg {
            Some(format!("{}#{}", flake, flakearg))
        } else {
            Some(flake)
        }
    } else {
        None
    };
    let f = fs::read_to_string(&systemconfig)?;
    let exe = match std::env::current_exe() {
        Ok(mut e) => {
            e.pop(); // root/bin
            e.pop(); // root/
            e.push("libexec"); // root/libexec
            e.push("nsc-helper");
            let x = e.to_string_lossy().to_string();
            info!("nsc-helper path: {}", x);
            if Path::new(&x).is_file() {
                x
            } else {
                String::from("nsc-helper")
            }
        }
        Err(_) => String::from("nsc-helper"),
    };

    let flakepathsplit = flakeargs.clone().unwrap_or_default().to_string();
    let flakepath = flakepathsplit
        .split('#')
        .collect::<Vec<&str>>()
        .first()
        .cloned()
        .unwrap_or_default();

    let rebuildargs = if let Some(x) = flakeargs {
        let mut v = vec![String::from("--flake")];
        for arg in x.split(' ') {
            if !arg.is_empty() {
                v.push(String::from(arg));
            }
        }
        v
    } else {
        vec![]
    };

    let mut cmd = match cmd {
        NscCmd::Rebuild => tokio::process::Command::new("pkexec")
            .arg(&exe)
            .arg("rebuild")
            .arg("--generations")
            .arg(config.generations.unwrap_or(0).to_string())
            .arg("--")
            .arg("switch")
            .args(&rebuildargs)
            .stderr(Stdio::piped())
            .spawn()?,
        NscCmd::Channel => tokio::process::Command::new("pkexec")
            .arg(&exe)
            .arg("channel")
            .arg("--output")
            .arg(&systemconfig)
            .stderr(Stdio::piped())
            .spawn()?,
        NscCmd::All => match syspkgs {
            SystemPkgs::Legacy => {
                if let Some(rmpkgs) = rmpkgs {
                    let newconfig =
                        match nix_editor::write::rmarr(&f, "environment.systemPackages", rmpkgs) {
                            Ok(x) => x,
                            Err(_) => {
                                return Err(anyhow!("Failed to write configuration.nix"));
                            }
                        };
                    let mut cmd = tokio::process::Command::new("pkexec")
                        .arg(&exe)
                        .arg("channel")
                        .arg("--rebuild")
                        .arg("--update")
                        .arg("--generations")
                        .arg(config.generations.unwrap_or(0).to_string())
                        .arg("--output")
                        .arg(&systemconfig)
                        .arg("--")
                        .arg("switch")
                        .args(&rebuildargs)
                        .stderr(Stdio::piped())
                        .stdin(Stdio::piped())
                        .spawn()?;
                    cmd.stdin
                        .take()
                        .unwrap()
                        .write_all(newconfig.as_bytes())
                        .await?;
                    cmd
                } else {
                    tokio::process::Command::new("pkexec")
                        .arg(&exe)
                        .arg("channel")
                        .arg("--rebuild")
                        .arg("--generations")
                        .arg(config.generations.unwrap_or(0).to_string())
                        .arg("--")
                        .arg("switch")
                        .args(&rebuildargs)
                        .stderr(Stdio::piped())
                        .spawn()?
                }
            }
            SystemPkgs::Flake => {
                if let Some(rmpkgs) = rmpkgs {
                    let newconfig =
                        match nix_editor::write::rmarr(&f, "environment.systemPackages", rmpkgs) {
                            Ok(x) => x,
                            Err(_) => {
                                return Err(anyhow!("Failed to write configuration.nix"));
                            }
                        };
                    let mut cmd = tokio::process::Command::new("pkexec")
                        .arg(&exe)
                        .arg("flake")
                        .arg("--rebuild")
                        .arg("--flakepath")
                        .arg(&flakepath)
                        .arg("--update")
                        .arg("--generations")
                        .arg(config.generations.unwrap_or(0).to_string())
                        .arg("--output")
                        .arg(&systemconfig)
                        .arg("--")
                        .arg("switch")
                        .arg("--impure")
                        .args(&rebuildargs)
                        .stderr(Stdio::piped())
                        .stdin(Stdio::piped())
                        .spawn()?;
                    cmd.stdin
                        .take()
                        .unwrap()
                        .write_all(newconfig.as_bytes())
                        .await?;
                    cmd
                } else {
                    tokio::process::Command::new("pkexec")
                        .arg(&exe)
                        .arg("flake")
                        .arg("--rebuild")
                        .arg("--flakepath")
                        .arg(&flakepath)
                        .arg("--generations")
                        .arg(config.generations.unwrap_or(0).to_string())
                        .arg("--output")
                        .arg(&systemconfig)
                        .arg("--")
                        .arg("switch")
                        .arg("--impure")
                        .args(&rebuildargs)
                        .stderr(Stdio::piped())
                        .spawn()?
                }
            }
            SystemPkgs::None => return Ok(true),
        },
    };

    let stderr = cmd.stderr.take().unwrap();
    let reader = tokio::io::BufReader::new(stderr);

    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        trace!("CAUGHT REBUILD LINE: {}", line);
        sink(line);
    }
    if cmd.wait().await?.success() {
        Ok(true)
    } else {
        Ok(false)
    }
}

async fn updateenv(sink: Sink<'_>) -> Result<bool> {
    let _lock = nsc_backend::run::lock().await;
    let mut cmd = tokio::process::Command::new("nix-env")
        .arg("-u")
        .stderr(Stdio::piped())
        .spawn()?;

    let stderr = cmd.stderr.take().unwrap();
    let reader = tokio::io::BufReader::new(stderr);

    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        trace!("CAUGHT NIXENV LINE: {}", line);
        sink(line);
    }
    if cmd.wait().await?.success() {
        Ok(true)
    } else {
        Ok(false)
    }
}
