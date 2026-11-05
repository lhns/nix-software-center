//! What the UI knows about package sources: the merged database, per-app labels and the
//! installable for an attribute. Catalogue data is untrusted; names reach the UI only escaped
//! or through widgets with markup off.

use log::*;
use nsc_backend::{
    catalogue::{self, Builtin, Third},
    pkgdb,
    sources::{self, Source, Store},
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Mutex,
};

/// attribute -> (unfree, source id), for unfree apps and third-party apps only.
static INFO: Mutex<Option<HashMap<String, (bool, String)>>> = Mutex::new(None);
/// Profile keys installed from a flake that is no longer a source.
static REMOVED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// Rows dropped and sources refused by the last merge.
static REPORT: Mutex<Option<(u64, Vec<(String, String)>)>> = Mutex::new(None);

pub fn store() -> Store {
    Store::load(Store::default_path(), sources::settings().clone())
}

/// Sources and unfree flake, for mapping profile elements to keys.
pub fn profile_sources() -> (Vec<Source>, String) {
    (store().sources(), sources::settings().unfree_flake.clone())
}

pub fn set_removed(keys: HashSet<String>) {
    *REMOVED.lock().unwrap() = Some(keys);
}

pub fn source_removed(key: &str) -> bool {
    REMOVED.lock().unwrap().as_ref().map_or(false, |s| s.contains(key))
}

pub fn last_report() -> (u64, Vec<(String, String)>) {
    REPORT.lock().unwrap().clone().unwrap_or_default()
}

/// Merges the built-in database with the sources' catalogues; returns the file the app queries.
pub async fn merged(builtin: &str) -> Result<String, String> {
    let store = store();
    if let Some(e) = &store.parse_error {
        warn!("{}: {e}; using the default sources", store.path.display());
    }
    let srcs = store.sources();
    let third: Vec<Third> = srcs
        .iter()
        .filter(|s| !s.builtin)
        .filter_map(|s| {
            let link = sources::catalogue_link(&s.id);
            link.exists().then(|| Third { id: s.id.clone(), path: link, listed: s.enabled })
        })
        .collect();
    let b = Builtin {
        db: PathBuf::from(builtin),
        free_listed: srcs[0].enabled,
        unfree_listed: srcs[1].enabled,
    };
    let m = catalogue::merge(&sources::cache_dir(), Some(&b), &third)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if m.dropped > 0 || !m.rejected.is_empty() {
        warn!("catalogue merge: {} rows dropped, rejected {:?}", m.dropped, m.rejected);
    }
    let rows = load_info(&m.path).await;
    *INFO.lock().unwrap() = Some(rows);
    *REPORT.lock().unwrap() = Some((m.dropped, m.rejected));
    Ok(m.path.to_string_lossy().into_owned())
}

async fn load_info(path: &std::path::Path) -> HashMap<String, (bool, String)> {
    let Ok(pool) = pkgdb::connect(path).await else { return HashMap::new() };
    let rows: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT p.attribute, m.unfree, p.source FROM pkgs p JOIN meta m ON m.attribute = p.attribute
         WHERE m.unfree = 1 OR p.source NOT IN ('nixpkgs', 'nixpkgs-unfree')",
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_else(|e| {
        warn!("source labels: {e}");
        vec![]
    });
    pool.close().await;
    rows.into_iter().map(|(a, u, s)| (a, (u == 1, s))).collect()
}

fn lookup(pkg: &str) -> Option<(bool, String)> {
    INFO.lock().unwrap().as_ref()?.get(pkg).cloned()
}

pub fn is_unfree(pkg: &str) -> bool {
    lookup(pkg).map_or(false, |r| r.0)
}

/// Display name of a third-party app's source.
pub fn source_name(pkg: &str) -> Option<String> {
    let (_, id) = lookup(pkg)?;
    if id == sources::NIXPKGS_ID || id == sources::UNFREE_ID {
        return None;
    }
    store().sources().into_iter().find(|s| s.id == id).map(|s| s.name)
}

/// The one label text for an app: "unfree", the source of a third-party app, or both.
/// Plain text: put it in a label with markup off.
pub fn badges(pkg: &str) -> Option<String> {
    let mut parts = vec![];
    if is_unfree(pkg) {
        parts.push("unfree".to_string());
    }
    parts.extend(source_name(pkg));
    if source_removed(pkg) {
        parts.push("source removed".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The attribute with its badges, for the caption line of a list row.
pub fn caption(pkg: &str) -> String {
    match badges(pkg) {
        Some(b) => format!("{pkg} · {b}"),
        None => pkg.to_string(),
    }
}

/// `<flake>#<attribute>` for a package database attribute: third-party apps from their source's
/// flake, built-in ones from nixpkgs or the unfree flake. Safe to put into a shell string once
/// quoted with `shell_quote`.
pub fn installable(pkg: &str) -> Result<String, String> {
    match pkg.split_once(':') {
        Some((id, attr)) => {
            let src = store()
                .sources()
                .into_iter()
                .find(|s| !s.builtin && s.id == id)
                .ok_or_else(|| format!("unknown source {id}"))?;
            sources::installable(&src.flake, attr)
        }
        None => {
            let unfree = sources::settings().unfree_flake.as_str();
            sources::installable(if is_unfree(pkg) { unfree } else { "nixpkgs" }, pkg)
        }
    }
}

/// Single-quoted for `bash -c`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Escapes text for the few widgets that always parse markup.
pub fn escape(s: &str) -> String {
    gtk::glib::markup_escape_text(s).to_string()
}
