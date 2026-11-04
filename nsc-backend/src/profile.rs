//! `nix profile list --json`.

use crate::ops::Nix;
use crate::sources::{origin, Origin, Source};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileElement {
    /// Element name (v3), or the list index for the older list form.
    #[serde(default)]
    pub name: String,
    pub attr_path: Option<String>,
    pub original_url: Option<String>,
    pub url: Option<String>,
    pub outputs: Option<Vec<String>>,
    /// Empty for elements that are only declared.
    #[serde(default)]
    pub store_paths: Vec<String>,
    pub priority: Option<i64>,
    pub active: Option<bool>,
    pub backend: Option<String>,
}

#[derive(Deserialize)]
struct Root {
    elements: Elements,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Elements {
    Map(BTreeMap<String, ProfileElement>),
    List(Vec<ProfileElement>),
}

pub fn parse_list_json(json: &str) -> Result<Vec<ProfileElement>> {
    let root: Root = serde_json::from_str(json).context("parsing `nix profile list --json`")?;
    Ok(match root.elements {
        Elements::Map(m) => m
            .into_iter()
            .map(|(name, mut e)| {
                e.name = name;
                e
            })
            .collect(),
        Elements::List(l) => l
            .into_iter()
            .enumerate()
            .map(|(i, mut e)| {
                e.name = i.to_string();
                e
            })
            .collect(),
    })
}

pub async fn list(nix: &Nix) -> Result<Vec<ProfileElement>> {
    let out = tokio::process::Command::new(&nix.program)
        .args(["profile", "list", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .with_context(|| format!("failed to start {}", nix.program.display()))?;
    if !out.status.success() {
        bail!(
            "nix profile list failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_list_json(&String::from_utf8_lossy(&out.stdout))
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPkg {
    /// nixpkgs attribute, `<sourceid>:<attribute>`, or `<originalUrl>#<attrPath>`.
    pub key: String,
    /// Element name, what remove/upgrade take.
    pub element: String,
    /// `Some(key)` when the key is an attribute of the package database.
    pub nixpkgs_attr: Option<String>,
    pub store_version: Option<String>,
    /// Installed from a flake that is neither built in nor a configured source any more.
    pub source_removed: bool,
}

/// Elements the app can show; ones without `attrPath`/`originalUrl` are skipped. Every
/// `legacyPackages` element counts as nixpkgs.
pub fn installed(elements: &[ProfileElement]) -> Vec<InstalledPkg> {
    installed_in(elements, None)
}

/// As `installed`, with elements of configured sources keyed `<sourceid>:<attribute>`, and
/// elements of unknown flakes kept apart (and flagged) instead of passing as nixpkgs.
pub fn installed_in(elements: &[ProfileElement], sources: Option<(&[Source], &str)>) -> Vec<InstalledPkg> {
    elements
        .iter()
        .filter_map(|e| {
            let attr_path = e.attr_path.as_deref()?;
            let original_url = e.original_url.as_deref()?;
            let attr = match attr_path.splitn(3, '.').collect::<Vec<_>>()[..] {
                ["legacyPackages" | "packages", _system, attr] => Some(attr),
                _ => None,
            };
            let is_legacy = attr_path.starts_with("legacyPackages.");
            let mut source_removed = false;
            let (key, nixpkgs_attr) = match (sources, attr) {
                (None, Some(attr)) if is_legacy => (attr.to_string(), Some(attr.to_string())),
                (None, _) => (format!("{original_url}#{attr_path}"), None),
                (Some((srcs, unfree)), attr) => match (origin(srcs, unfree, original_url), attr) {
                    (Origin::Builtin, Some(attr)) if is_legacy => (attr.to_string(), Some(attr.to_string())),
                    (Origin::Source(s), Some(attr)) => {
                        let k = format!("{}:{attr}", s.id);
                        (k.clone(), Some(k))
                    }
                    (Origin::Unknown, Some(attr)) if is_legacy && looks_like_nixpkgs(original_url) => {
                        (attr.to_string(), Some(attr.to_string()))
                    }
                    (o, _) => {
                        source_removed = o == Origin::Unknown && !looks_like_nixpkgs(original_url);
                        (format!("{original_url}#{attr_path}"), None)
                    }
                },
            };
            Some(InstalledPkg {
                nixpkgs_attr,
                key,
                element: e.name.clone(),
                store_version: e.store_paths.first().and_then(|p| store_version(p)),
                source_removed,
            })
        })
        .collect()
}

/// A hand-installed `github:NixOS/nixpkgs/...` is not a removed source.
fn looks_like_nixpkgs(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("github:nixos/nixpkgs") || u.starts_with("flake:nixpkgs") || u.starts_with("nixpkgs")
}

/// Version of a store path, with `builtins.parseDrvName` rules.
pub fn store_version(path: &str) -> Option<String> {
    let base = path.rsplit('/').next()?;
    // `<32-char hash>-<name>`
    let name = base.get(33..).filter(|_| base.as_bytes().get(32) == Some(&b'-'))?;
    let bytes = name.as_bytes();
    let i = (0..bytes.len().saturating_sub(1))
        .find(|&i| bytes[i] == b'-' && !bytes[i + 1].is_ascii_alphabetic())?;
    Some(name[i + 1..].to_string())
}
