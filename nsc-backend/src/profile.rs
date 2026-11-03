//! `nix profile list --json`.

use crate::ops::Nix;
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
    /// nixpkgs attribute, or `<originalUrl>#<attrPath>`.
    pub key: String,
    /// Element name, what remove/upgrade take.
    pub element: String,
    /// `Some(key)` when the key is a nixpkgs attribute.
    pub nixpkgs_attr: Option<String>,
    pub store_version: Option<String>,
}

/// Elements the app can show; ones without `attrPath`/`originalUrl` are skipped.
pub fn installed(elements: &[ProfileElement]) -> Vec<InstalledPkg> {
    elements
        .iter()
        .filter_map(|e| {
            let attr_path = e.attr_path.as_deref()?;
            let original_url = e.original_url.as_deref()?;
            let (key, nixpkgs_attr) = match attr_path.splitn(3, '.').collect::<Vec<_>>()[..] {
                ["legacyPackages", _system, attr] => (attr.to_string(), Some(attr.to_string())),
                _ => (format!("{original_url}#{attr_path}"), None),
            };
            Some(InstalledPkg {
                nixpkgs_attr: nixpkgs_attr.clone(),
                key,
                element: e.name.clone(),
                store_version: e.store_paths.first().and_then(|p| store_version(p)),
            })
        })
        .collect()
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
