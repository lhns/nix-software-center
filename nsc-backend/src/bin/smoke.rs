//! End-to-end check of the backend against a real `nix`, using the same
//! functions the UI calls. Run as the desktop user.
//!
//! Usage: nsc-backend-smoke <installable> [--pkgdb <file>] [--require-progress]
//! `--pkgdb` defaults to $NSC_PKGDB.

use anyhow::{bail, Context, Result};
use nsc_backend::run::{Cancel, Event, Stream};
use nsc_backend::{ops, pkgdb, profile};
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

async fn real_main() -> Result<()> {
    let mut installable = None;
    let mut pkgdb_path = None;
    let mut require_progress = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pkgdb" => pkgdb_path = Some(PathBuf::from(it.next().context("--pkgdb needs a file")?)),
            "--require-progress" => require_progress = true,
            _ if installable.is_none() => installable = Some(a),
            _ => bail!("unexpected argument {a}"),
        }
    }
    let pkgdb_path = pkgdb_path.or_else(|| std::env::var_os("NSC_PKGDB").map(PathBuf::from));
    let installable = installable.context("usage: nsc-backend-smoke <installable> [--pkgdb <file>] [--require-progress]")?;

    let nix = ops::Nix::detect().await;
    println!("1. nix version: {:?}", nix.version);

    let before = names(&nix).await?;
    println!("2. list: {} elements", before.len());

    println!("3. add {installable}");
    let cancel = Cancel::new();
    let (mut started, mut stopped) = (0, 0);
    let ok = ops::add(&nix, &installable, &cancel, |e| {
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
    let [added] = added[..] else {
        bail!("expected exactly one new element, got {}", added.len());
    };
    println!("4. new element: {}", added.name);
    let pkgs = profile::installed(std::slice::from_ref(added));
    let Some(pkg) = pkgs.first() else {
        bail!("new element has no key (no attrPath/originalUrl)");
    };
    println!("   key: {} (store version {:?})", pkg.key, pkg.store_version);
    if let (Some(db), Some(attr)) = (&pkgdb_path, &pkg.nixpkgs_attr) {
        let pool = pkgdb::connect(db).await?;
        println!("   pkgdb version: {:?}", pkgdb::version(&pool, attr).await);
    }

    println!("5. upgrade --all");
    if !ops::upgrade_all(&nix, &cancel, show).await? {
        bail!("upgrade --all failed");
    }

    println!("6. remove {}", added.name);
    if !ops::remove(&nix, &[added.name.clone()], &cancel, show).await? {
        bail!("remove failed");
    }

    let after = names(&nix).await?;
    if after.contains(&added.name) {
        bail!("{} still listed after remove", added.name);
    }
    println!("7. ok: {} no longer listed", added.name);
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("FAILED: {e:#}");
        std::process::exit(1);
    }
}
