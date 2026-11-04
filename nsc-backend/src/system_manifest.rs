//! Apps built into the system image ("Included with desktop"). The deployer points
//! `NSC_SYSTEM_MANIFEST` at a read-only JSON file listing the image's packages:
//!
//! ```json
//! {"packages": [{"pname": "dolphin", "version": "24.02", "path": "/nix/store/…-dolphin-24.02"}]}
//! ```
//!
//! Matching is by `pname` against the built-in catalogue: the catalogue has no store paths
//! (computing them means instantiating every derivation), so `path` is informational. Only
//! built-in attributes match; a third-party source's `<id>:<attr>` never does. The file is
//! untrusted input: size-capped, parsed leniently, bound as SQL parameters only.

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

/// Distinct package names of the manifest; entries without a usable `pname` are skipped.
pub fn parse(json: &str) -> Result<Vec<String>, String> {
    let file: File = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in file.packages {
        let Some(name) = p.get("pname").and_then(|n| n.as_str()) else { continue };
        if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
            continue;
        }
        if seen.insert(name.to_string()) {
            out.push(name.to_string());
        }
    }
    Ok(out)
}

pub fn read(path: &Path) -> Result<Vec<String>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err(format!("{} bytes is over the {MAX_BYTES} limit", meta.len()));
    }
    parse(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
}

/// Names from `$NSC_SYSTEM_MANIFEST`. Unset or empty: the feature is off, silently. Set but
/// unreadable or malformed: off, with a warning.
pub fn from_env() -> Option<Vec<String>> {
    let path = std::env::var_os(ENV).filter(|v| !v.is_empty())?;
    match read(Path::new(&path)) {
        Ok(names) => Some(names),
        Err(e) => {
            log::warn!("Ignoring {ENV}={}: {e}", Path::new(&path).display());
            None
        }
    }
}

/// Built-in catalogue attributes for the names. Several attributes can share a `pname`
/// (`git`, `gitMinimal`): those whose last path component is the name win, else all match.
pub async fn resolve(pool: &SqlitePool, names: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for name in names {
        let rows: Result<Vec<(String,)>, _> =
            sqlx::query_as("SELECT attribute FROM pkgs WHERE pname = $1 AND instr(attribute, ':') = 0")
                .bind(name)
                .fetch_all(pool)
                .await;
        let attrs: Vec<String> = match rows {
            Ok(r) => r.into_iter().map(|r| r.0).collect(),
            Err(e) => {
                log::warn!("system manifest lookup of {name}: {e}");
                continue;
            }
        };
        let exact: Vec<&String> = attrs.iter().filter(|a| a.rsplit('.').next() == Some(name.as_str())).collect();
        if exact.is_empty() {
            out.extend(attrs);
        } else {
            out.extend(exact.into_iter().cloned());
        }
    }
    out
}

/// Whether the app shows as "Included with desktop": in the image and not installed by the user,
/// whose own install wins. Third-party attributes (`<id>:<attr>`) are never in the set.
pub fn is_included(included: &HashSet<String>, attr: &str, user_installed: bool) -> bool {
    !user_installed && included.contains(attr)
}
