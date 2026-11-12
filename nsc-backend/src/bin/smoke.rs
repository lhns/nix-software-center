//! End-to-end check of the backend against a real `nix`, using the same
//! functions the UI calls. Run as the desktop user.
//!
//! Usage: nsc-backend-smoke <installable>... [--pkgdb <file>] [--require-progress]
//! `--run <installable> [--desktop-id <id>]` instead builds and starts it without installing, as
//! "Run without installing" does, and checks the profile is unchanged.
//! `--pkgdb` defaults to $NSC_PKGDB.
//! `--system-manifest <file>` instead prints how each entry of a system manifest matches the
//! package database (no installables, no nix).

use anyhow::{bail, Context, Result};
use nsc_backend::run::{Cancel, Event, Stream};
use nsc_backend::{launch, ops, pkgdb, profile};
use std::collections::HashSet;
use std::path::PathBuf;

fn show(e: Event) {
    match e {
        Event::Line(Stream::Stdout, l) => println!("  out| {l}"),
        Event::Line(Stream::Stderr, l) => println!("  err| {l}"),
        Event::Progress(p) => println!(
            "  progress| {:?} {:?} builds={:?} downloads={:?} bytes={:?}",
            p.fraction, p.status, p.builds, p.downloads, p.bytes
        ),
    }
}

async fn names(nix: &ops::Nix) -> Result<HashSet<String>> {
    Ok(profile::list(nix).await?.into_iter().map(|e| e.name).collect())
}

async fn run_without_installing(installable: &str, desktop_id: Option<&str>) -> Result<()> {
    let nix = ops::Nix::detect().await;
    println!("1. nix version: {:?}", nix.version);
    let before = names(&nix).await?;
    println!("2. list: {} elements", before.len());

    println!("3. build {installable}");
    let cancel = Cancel::new();
    let Some(outs) = ops::build(&nix, installable, &cancel, show).await? else {
        bail!("build failed");
    };
    for o in &outs {
        println!("run: out {}", o.display());
    }

    let target = launch::resolve(&nix, installable, installable, &outs, desktop_id, &cancel, show).await?;
    let env = launch::current_env(&outs);
    match target.kind {
        launch::Kind::Desktop => println!("run: target desktop {}", target.path.display()),
        launch::Kind::Program => println!("run: target program {}", target.path.display()),
    }
    if target.kind == launch::Kind::Desktop && !target.exec.terminal {
        launch::spawn_detached(&target.exec, &env)?;
    } else {
        let status = launch::run_foreground(&target.exec, &env)?;
        if !status.success() {
            bail!("{:?} exited with {status}", target.exec.argv);
        }
    }

    let after = names(&nix).await?;
    if after != before {
        bail!("profile changed: {before:?} -> {after:?}");
    }
    println!("4. ok: profile unchanged");
    Ok(())
}

async fn match_report(manifest: &std::path::Path, db: PathBuf) -> Result<()> {
    let entries = nsc_backend::system_manifest::read(manifest).map_err(anyhow::Error::msg)?;
    let pool = pkgdb::connect(&db).await?;
    let mut matched = 0;
    for e in &entries {
        let mut attrs: Vec<_> = nsc_backend::system_manifest::resolve(&pool, std::slice::from_ref(e)).await.into_iter().collect();
        attrs.sort();
        matched += !attrs.is_empty() as usize;
        println!("match: {} {} -> [{}]", e.pname, e.version, attrs.join(" "));
    }
    println!("matched {matched} of {} entries", entries.len());
    Ok(())
}

async fn real_main() -> Result<()> {
    let mut installables: Vec<String> = vec![];
    let mut pkgdb_path = None;
    let mut require_progress = false;
    let mut manifest = None;
    let mut run = None;
    let mut desktop_id = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pkgdb" => pkgdb_path = Some(PathBuf::from(it.next().context("--pkgdb needs a file")?)),
            "--require-progress" => require_progress = true,
            "--system-manifest" => manifest = Some(PathBuf::from(it.next().context("--system-manifest needs a file")?)),
            "--run" => run = Some(it.next().context("--run needs an installable")?),
            "--desktop-id" => desktop_id = Some(it.next().context("--desktop-id needs an id")?),
            _ if a.starts_with("--") => bail!("unexpected argument {a}"),
            _ => installables.push(a),
        }
    }
    let pkgdb_path = pkgdb_path.or_else(|| std::env::var_os("NSC_PKGDB").map(PathBuf::from));
    if let Some(m) = manifest {
        return match_report(&m, pkgdb_path.context("--system-manifest needs --pkgdb or $NSC_PKGDB")?).await;
    }
    if let Some(installable) = run {
        return run_without_installing(&installable, desktop_id.as_deref()).await;
    }
    if installables.is_empty() {
        bail!("usage: nsc-backend-smoke <installable>... [--pkgdb <file>] [--require-progress] | --run <installable> [--desktop-id <id>] | --system-manifest <file>");
    }

    let nix = ops::Nix::detect().await;
    println!("1. nix version: {:?}", nix.version);

    let before = names(&nix).await?;
    println!("2. list: {} elements", before.len());

    println!("3. add {}", installables.join(" "));
    let cancel = Cancel::new();
    let (mut started, mut stopped) = (0, 0);
    let ok = ops::add_many(&nix, &installables, &cancel, |e| {
        if let Event::Progress(p) = &e {
            started = started.max(p.started);
            stopped = stopped.max(p.stopped);
        }
        show(e);
    })
    .await?;
    if !ok {
        bail!("add failed");
    }
    println!("   progress: {started} started, {stopped} stopped");
    if require_progress && (started == 0 || stopped == 0) {
        bail!("no progress start/stop pair seen during add");
    }

    let elements = profile::list(&nix).await?;
    let added: Vec<_> = elements.iter().filter(|e| !before.contains(&e.name)).collect();
    if added.len() != installables.len() {
        bail!("expected {} new elements, got {}", installables.len(), added.len());
    }
    let added_names: Vec<String> = added.iter().map(|e| e.name.clone()).collect();
    println!("4. new elements: {}", added_names.join(" "));
    for added in &added {
        let pkgs = profile::installed(std::slice::from_ref(*added));
        let Some(pkg) = pkgs.first() else {
            bail!("new element {} has no key (no attrPath/originalUrl)", added.name);
        };
        println!("   key: {} (store version {:?})", pkg.key, pkg.store_version);
        if let (Some(db), Some(attr)) = (&pkgdb_path, &pkg.nixpkgs_attr) {
            let pool = pkgdb::connect(db).await?;
            println!("   pkgdb version: {:?}", pkgdb::version(&pool, attr).await);
        }
    }

    println!("5. upgrade --all");
    if !ops::upgrade_all(&nix, &cancel, show).await? {
        bail!("upgrade --all failed");
    }

    println!("6. remove {}", added_names.join(" "));
    if !ops::remove(&nix, &added_names, &cancel, show).await? {
        bail!("remove failed");
    }

    let after = names(&nix).await?;
    if let Some(n) = added_names.iter().find(|n| after.contains(*n)) {
        bail!("{n} still listed after remove");
    }
    println!("7. ok: {} no longer listed", added_names.join(" "));
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("FAILED: {e:#}");
        std::process::exit(1);
    }
}
