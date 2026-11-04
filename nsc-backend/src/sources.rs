//! Package sources: flakes that publish packages plus a catalogue (`docs/integration.md`).
//! Holds the source list, the deployer settings and flake-reference validation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const NIXPKGS_ID: &str = "nixpkgs";
pub const UNFREE_ID: &str = "nixpkgs-unfree";
/// Default flake of the unfree built-in: own wrapper, follows the registry's nixpkgs.
pub const DEFAULT_UNFREE_FLAKE: &str = "github:lhns/nix-software-center?dir=nix/unfree";
const MAX_REF: usize = 512;

/// Deployer settings, read once at startup.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// `NSC_SYSTEM_PACKAGES=0` turns every NixOS/system feature off.
    pub system_packages: bool,
    pub unfree_flake: String,
    /// Whether "nixpkgs unfree" is listed unless the user toggled it.
    pub unfree_listed_default: bool,
}

impl Settings {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let unfree_flake = get("NSC_UNFREE_FLAKE")
            .filter(|f| validate_flake_ref(f).is_ok())
            .unwrap_or_else(|| DEFAULT_UNFREE_FLAKE.to_string());
        Settings {
            system_packages: get("NSC_SYSTEM_PACKAGES").as_deref() != Some("0"),
            unfree_flake,
            unfree_listed_default: get("NSC_UNFREE_LISTED_DEFAULT").as_deref() != Some("0"),
        }
    }
}

static SETTINGS: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();

/// The deployer settings, read once.
pub fn settings() -> &'static Settings {
    SETTINGS.get_or_init(Settings::from_env)
}

/// A NixOS system whose configuration the app may manage.
pub fn is_nixos() -> bool {
    // NSC_FAKE_NIXOS is a test seam: CI is not NixOS but must show the NixOS-only screens.
    settings().system_packages
        && (Path::new("/etc/NIXOS").exists() || std::env::var_os("NSC_FAKE_NIXOS").is_some())
}

/// Why a flake reference was refused.
pub fn validate_flake_ref(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    if s.len() > MAX_REF {
        return Err(format!("longer than {MAX_REF} characters"));
    }
    if s.starts_with('-') {
        return Err("starts with '-'".into());
    }
    if let Some(c) = s
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "._~:/@?=%+,-".contains(*c)))
    {
        return Err(format!("contains {:?}", c));
    }
    const SCHEMES: [&str; 6] = ["github:", "gitlab:", "sourcehut:", "git+https:", "https:", "flake:"];
    if SCHEMES.iter().any(|p| s.starts_with(p) && s.len() > p.len()) {
        return Ok(());
    }
    // A bare registry name; a leading '.' would be a relative path.
    if s.starts_with(|c: char| c.is_ascii_alphanumeric()) && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Ok(());
    }
    Err("not a github:, gitlab:, sourcehut:, git+https:, https: or flake: reference or registry name".into())
}

/// The form `nix profile list` reports in `originalUrl`: bare registry names become `flake:<name>`.
pub fn normalise_ref(s: &str) -> String {
    if s.contains(':') {
        s.to_string()
    } else {
        format!("flake:{s}")
    }
}

/// Attribute and pname allowlist; both reach shell strings.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "_+.-".contains(c))
}

/// `<flake>#<attribute>`, refusing anything the shell could take apart.
pub fn installable(flake: &str, attr: &str) -> Result<String, String> {
    validate_flake_ref(flake)?;
    if !valid_name(attr) {
        return Err(format!("invalid attribute {attr:?}"));
    }
    Ok(format!("{flake}#{attr}"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserSource {
    pub id: String,
    pub name: String,
    pub flake: String,
}

/// `sources.json`: user sources and explicit toggles only, so an untouched built-in follows the
/// deployer default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SourcesFile {
    #[serde(default)]
    pub sources: Vec<UserSource>,
    #[serde(default)]
    pub enabled: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub flake: String,
    pub builtin: bool,
    pub enabled: bool,
}

/// The file plus whether it failed to parse, so the broken file is kept until the next save.
#[derive(Debug, Clone)]
pub struct Store {
    pub path: PathBuf,
    pub file: SourcesFile,
    pub settings: Settings,
    pub parse_error: Option<String>,
}

impl Store {
    pub fn load(path: PathBuf, settings: Settings) -> Store {
        let (file, parse_error) = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<SourcesFile>(&text) {
                Ok(f) => (sanitise(f), None),
                Err(e) => {
                    log::warn!("{}: {e}; using defaults", path.display());
                    (SourcesFile::default(), Some(e.to_string()))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (SourcesFile::default(), None),
            Err(e) => {
                log::warn!("{}: {e}; using defaults", path.display());
                (SourcesFile::default(), Some(e.to_string()))
            }
        };
        Store { path, file, settings, parse_error }
    }

    pub fn default_path() -> PathBuf {
        config_dir().join("sources.json")
    }

    /// Built-ins first, then user sources.
    pub fn sources(&self) -> Vec<Source> {
        let on = |id: &str, default: bool| self.file.enabled.get(id).copied().unwrap_or(default);
        let mut out = vec![
            Source {
                id: NIXPKGS_ID.into(),
                name: "nixpkgs".into(),
                flake: "nixpkgs".into(),
                builtin: true,
                enabled: on(NIXPKGS_ID, true),
            },
            Source {
                id: UNFREE_ID.into(),
                name: "nixpkgs unfree".into(),
                flake: self.settings.unfree_flake.clone(),
                builtin: true,
                enabled: on(UNFREE_ID, self.settings.unfree_listed_default),
            },
        ];
        out.extend(self.file.sources.iter().map(|s| Source {
            id: s.id.clone(),
            name: s.name.clone(),
            flake: s.flake.clone(),
            builtin: false,
            enabled: on(&s.id, true),
        }));
        out
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        self.file.enabled.insert(id.to_string(), enabled);
    }

    /// Adds a user source; the id is generated here.
    pub fn add(&mut self, name: &str, flake: &str) -> Result<String, String> {
        validate_flake_ref(flake)?;
        let name = name.trim();
        let name = if name.is_empty() { flake } else { name };
        let taken: Vec<String> = self.sources().into_iter().map(|s| s.id).collect();
        let id = new_id(name, &taken);
        self.file.sources.push(UserSource {
            id: id.clone(),
            name: name.chars().take(80).collect(),
            flake: flake.into(),
        });
        Ok(id)
    }

    pub fn remove(&mut self, id: &str) {
        self.file.sources.retain(|s| s.id != id);
        self.file.enabled.remove(id);
    }

    /// Writes the file; a file that failed to parse is moved to `.bad` first, not overwritten.
    pub fn save(&mut self) -> std::io::Result<()> {
        if self.parse_error.is_some() && self.path.exists() {
            std::fs::rename(&self.path, self.path.with_extension("json.bad"))?;
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.file)?)?;
        std::fs::rename(&tmp, &self.path)?;
        self.parse_error = None;
        Ok(())
    }
}

/// Drops entries that would break the invariants (hand-edited files).
fn sanitise(mut f: SourcesFile) -> SourcesFile {
    let mut seen = vec![NIXPKGS_ID.to_string(), UNFREE_ID.to_string()];
    f.sources.retain(|s| {
        let ok = is_id(&s.id) && validate_flake_ref(&s.flake).is_ok() && !seen.contains(&s.id);
        if ok {
            seen.push(s.id.clone());
        } else {
            log::warn!("sources.json: ignoring source {:?}", s.id);
        }
        ok
    });
    f
}

pub fn is_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
}

/// `[a-z0-9-]+`, unique among `taken`.
pub fn new_id(name: &str, taken: &[String]) -> String {
    let mapped: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' })
        .collect();
    let mut base = mapped.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    base.truncate(32);
    let base = base.trim_matches('-').to_string();
    let base = if base.is_empty() { "source".to_string() } else { base };
    if !taken.contains(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|c| !taken.contains(c)).unwrap()
}

/// Which source an installed element came from, by its `originalUrl`.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin<'a> {
    /// `flake:nixpkgs` or the unfree flake: attributes stay bare.
    Builtin,
    Source(&'a Source),
    /// Neither: a removed source, or something installed by hand.
    Unknown,
}

pub fn origin<'a>(sources: &'a [Source], unfree_flake: &str, original_url: &str) -> Origin<'a> {
    let url = normalise_ref(original_url);
    if url == "flake:nixpkgs" || url == normalise_ref(unfree_flake) {
        return Origin::Builtin;
    }
    match sources.iter().find(|s| !s.builtin && normalise_ref(&s.flake) == url) {
        Some(s) => Origin::Source(s),
        None => Origin::Unknown,
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("nix-software-center")
}

pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("nix-software-center")
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(fallback),
    }
}

/// `nix build` out-link of a source's catalogue (a GC root).
pub fn catalogue_link(id: &str) -> PathBuf {
    cache_dir().join("catalogues").join(id)
}

pub fn error_file(id: &str) -> PathBuf {
    cache_dir().join("catalogues").join(format!("{id}.error"))
}

/// The system attribute of `nscCatalogue.<system>`.
pub fn nix_system() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64-linux",
        _ => "x86_64-linux",
    }
}

/// Last update of a third-party source, or the error that kept its old catalogue.
pub fn status(id: &str) -> Result<Option<std::time::SystemTime>, String> {
    if let Ok(e) = std::fs::read_to_string(error_file(id)) {
        return Err(e);
    }
    Ok(std::fs::symlink_metadata(catalogue_link(id)).and_then(|m| m.modified()).ok())
}

/// Builds the source's catalogue into its out-link. On failure the previous link stays and the
/// error is written next to it.
pub async fn fetch(id: &str, flake: &str, timeout: std::time::Duration) -> Result<(), String> {
    validate_flake_ref(flake)?;
    let link = catalogue_link(id);
    let err = error_file(id);
    let result = fetch_inner(flake, &link, timeout).await;
    match &result {
        Ok(()) => {
            let _ = std::fs::remove_file(&err);
        }
        Err(e) => {
            let _ = std::fs::create_dir_all(err.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(&err, e);
        }
    }
    result
}

async fn fetch_inner(flake: &str, link: &Path, timeout: std::time::Duration) -> Result<(), String> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut cmd = tokio::process::Command::new("nix");
    cmd.arg("build")
        .arg(format!("{flake}#nscCatalogue.{}", nix_system()))
        .arg("--out-link")
        .arg(link)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => return Err("timed out".into()),
        Ok(Err(e)) => return Err(format!("could not run nix: {e}")),
        Ok(Ok(o)) => o,
    };
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(6).collect();
    Err(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
}

/// Whether the source should be refreshed at startup.
pub fn stale(id: &str, max_age: std::time::Duration) -> bool {
    match status(id) {
        Ok(Some(t)) => t.elapsed().map_or(true, |a| a > max_age),
        _ => true,
    }
}
