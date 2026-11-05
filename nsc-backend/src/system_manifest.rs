//! Apps built into the system image ("Included with desktop"). The deployer points
//! `NSC_SYSTEM_MANIFEST` at a read-only JSON file listing the image's packages:
//!
//! ```json
//! {"packages": [{"pname": "dolphin", "version": "24.02", "path": "/nix/store/…-dolphin-24.02"}]}
//! ```
//!
//! Matching is by `pname` and `version` against the built-in catalogue (see `pick`): the catalogue
//! has no store paths (computing them means instantiating every derivation), so `path` is
//! informational. Only built-in attributes match; a third-party source's `<id>:<attr>` never does.
//! The file is untrusted input: size-capped, parsed leniently, bound as SQL parameters only.

use serde::Deserialize;
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::path::Path;

pub const ENV: &str = "NSC_SYSTEM_MANIFEST";
const MAX_BYTES: u64 = 16 << 20;
const MAX_NAME: usize = 256;

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    packages: Vec<serde_json::Value>,
}

/// One manifest entry: the package name and, when the manifest has one, its version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Entry {
    pub pname: String,
    pub version: String,
}

fn text(p: &serde_json::Value, key: &str) -> Option<String> {
    let v = p.get(key)?.as_str()?;
    (v.len() <= MAX_NAME && !v.chars().any(char::is_control)).then(|| v.to_string())
}

/// Distinct entries of the manifest; those without a usable `pname` are skipped, a missing or
/// unusable `version` becomes empty.
pub fn parse(json: &str) -> Result<Vec<Entry>, String> {
    let file: File = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in file.packages {
        let Some(pname) = text(&p, "pname").filter(|n| !n.is_empty()) else { continue };
        let e = Entry { pname, version: text(&p, "version").unwrap_or_default() };
        if seen.insert(e.clone()) {
            out.push(e);
        }
    }
    Ok(out)
}

pub fn read(path: &Path) -> Result<Vec<Entry>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err(format!("{} bytes is over the {MAX_BYTES} limit", meta.len()));
    }
    parse(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
}

/// Entries from `$NSC_SYSTEM_MANIFEST`. Unset or empty: the feature is off, silently. Set but
/// unreadable or malformed: off, with a warning.
pub fn from_env() -> Option<Vec<Entry>> {
    let path = std::env::var_os(ENV).filter(|v| !v.is_empty())?;
    match read(Path::new(&path)) {
        Ok(entries) => Some(entries),
        Err(e) => {
            log::warn!("Ignoring {ENV}={}: {e}", Path::new(&path).display());
            None
        }
    }
}

/// Picks the catalogue attributes for one entry from the candidates sharing its `pname`, as
/// `(attribute, version)`. Candidates with the manifest's version win; with none, a lone
/// candidate still matches (the image patched the package) and several match nothing rather
/// than a guess. Then top-level attributes win over set members (`git` over `haskellPackages.git`),
/// then those whose last component is the `pname`.
pub fn pick(entry: &Entry, mut cands: Vec<(String, String)>) -> Vec<String> {
    let same: Vec<_> = cands.iter().filter(|c| !entry.version.is_empty() && c.1 == entry.version).cloned().collect();
    if !same.is_empty() {
        cands = same;
    } else if cands.len() != 1 {
        return Vec::new();
    }
    let mut attrs: Vec<String> = cands.into_iter().map(|c| c.0).collect();
    let top: Vec<_> = attrs.iter().filter(|a| !a.contains('.')).cloned().collect();
    if !top.is_empty() {
        attrs = top;
    }
    let named: Vec<_> = attrs.iter().filter(|a| a.rsplit('.').next() == Some(entry.pname.as_str())).cloned().collect();
    if !named.is_empty() {
        attrs = named;
    }
    attrs
}

/// Built-in catalogue attributes for the entries (see `pick`). Third-party attributes never match.
pub async fn resolve(pool: &SqlitePool, entries: &[Entry]) -> HashSet<String> {
    let mut out = HashSet::new();
    for e in entries {
        let rows: Result<Vec<(String, String)>, _> =
            sqlx::query_as("SELECT attribute, version FROM pkgs WHERE pname = $1 AND instr(attribute, ':') = 0")
                .bind(&e.pname)
                .fetch_all(pool)
                .await;
        match rows {
            Ok(r) => out.extend(pick(e, r)),
            Err(err) => log::warn!("system manifest lookup of {}: {err}", e.pname),
        }
    }
    out
}

/// Whether the app shows as "Included with desktop": in the image and not installed by the user,
/// whose own install wins. Third-party attributes (`<id>:<attr>`) are never in the set.
pub fn is_included(included: &HashSet<String>, attr: &str, user_installed: bool) -> bool {
    !user_installed && included.contains(attr)
}
