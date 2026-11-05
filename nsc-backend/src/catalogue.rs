//! Merges the built-in package database and the sources' catalogues into the one file the app
//! queries. Catalogues are untrusted: they are validated, copied row by row, never queried live.
//!
//! Built-in attributes stay bare; third-party ones become `<sourceid>:<attribute>`, so a source
//! can never shadow another. Rows carry `source` and `listed`; listing queries read `listed_pkgs`,
//! lookups by attribute read `pkgs`, so a hidden source's installed apps keep their metadata.

use crate::sources::{NIXPKGS_ID, UNFREE_ID};
use anyhow::{bail, Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{ConnectOptions, Connection, SqliteConnection};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const CATALOGUE_VERSION: &str = "1";
/// The catalogue file inside a source's `nscCatalogue.<system>` directory.
pub const CATALOGUE_DB: &str = "catalogue.db";

/// The package database shipped with the app (or nix-data's download), split by `meta.unfree`.
pub struct Builtin {
    pub db: PathBuf,
    pub free_listed: bool,
    pub unfree_listed: bool,
}

pub struct Third {
    pub id: String,
    /// The catalogue directory, or the db file itself.
    pub path: PathBuf,
    pub listed: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Merged {
    pub path: PathBuf,
    /// Rows left out because their attribute or pname failed the allowlist or collided.
    pub dropped: u64,
    /// Sources refused whole, with the reason.
    pub rejected: Vec<(String, String)>,
}

const PKGS_COLS: [&str; 4] = ["attribute", "pname", "version", "system"];
const META_COLS: [&str; 10] = [
    "attribute", "description", "longdescription", "homepage", "license", "platforms",
    "maintainers", "position", "broken", "insecure",
];

/// Name of the merged file for these inputs; same inputs, same name.
pub fn merged_path(dir: &Path, builtin: Option<&Builtin>, third: &[Third]) -> PathBuf {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut key = String::from("v1");
    if let Some(b) = builtin {
        key += &format!("|b|{}|{}|{}", canon(&b.db).display(), b.free_listed, b.unfree_listed);
    }
    for t in third {
        key += &format!("|t|{}|{}|{}", t.id, canon(&t.path).display(), t.listed);
    }
    dir.join(format!("merged-{:016x}.db", fnv1a(key.as_bytes())))
}

/// `merged-<16 hex digits>.db` only: never another instance's `merged-tmp-<pid>.db`.
fn is_merged_name(n: &str) -> bool {
    n.strip_prefix("merged-")
        .and_then(|r| r.strip_suffix(".db"))
        .map_or(false, |h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Leftovers of instances that died mid-merge. Age-gated so a running instance's file is safe.
pub const TMP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub fn remove_stale_tmp(dir: &Path, max_age: Duration) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let n = e.file_name();
        let n = n.to_string_lossy();
        if !(n.starts_with("merged-tmp-") && n.ends_with(".db")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(false, |age| age > max_age);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

pub async fn merge(dir: &Path, builtin: Option<&Builtin>, third: &[Third]) -> Result<Merged> {
    std::fs::create_dir_all(dir)?;
    remove_stale_tmp(dir, TMP_MAX_AGE);
    let path = merged_path(dir, builtin, third);
    if path.exists() {
        if let Ok(m) = read_report(&path).await {
            return Ok(m);
        }
        let _ = std::fs::remove_file(&path);
    }
    let tmp = dir.join(format!("merged-tmp-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let opts = SqliteConnectOptions::new()
        .filename(&tmp)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Off)
        .synchronous(SqliteSynchronous::Off)
        .pragma("trusted_schema", "OFF");
    let mut conn = opts.connect().await.context("creating the merged database")?;
    let mut report = Merged { path: path.clone(), ..Default::default() };
    let result = build(&mut conn, builtin, third, &mut report).await;
    conn.close().await.ok();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &path)?;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if is_merged_name(&n) && e.path() != path {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(report)
}

async fn read_report(path: &Path) -> Result<Merged> {
    let pool = crate::pkgdb::connect(path).await?;
    let get = |k: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_as::<_, (String,)>("SELECT value FROM info WHERE key = $1")
                .bind(k)
                .fetch_optional(&pool)
                .await
        }
    };
    let dropped = get("nsc-dropped").await?.and_then(|r| r.0.parse().ok()).unwrap_or(0);
    let rejected = get("nsc-rejected")
        .await?
        .and_then(|r| serde_json::from_str(&r.0).ok())
        .unwrap_or_default();
    pool.close().await;
    Ok(Merged { path: path.to_path_buf(), dropped, rejected })
}

async fn build(
    conn: &mut SqliteConnection,
    builtin: Option<&Builtin>,
    third: &[Third],
    report: &mut Merged,
) -> Result<()> {
    for ddl in [
        "CREATE TABLE pkgs (attribute TEXT NOT NULL PRIMARY KEY, pname TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT 'nixpkgs', listed INTEGER NOT NULL DEFAULT 1)",
        "CREATE TABLE meta (attribute TEXT NOT NULL PRIMARY KEY, description TEXT NOT NULL DEFAULT '', longdescription TEXT NOT NULL DEFAULT '', homepage TEXT NOT NULL DEFAULT '', license TEXT NOT NULL DEFAULT '', platforms TEXT NOT NULL DEFAULT '', maintainers TEXT NOT NULL DEFAULT '', position TEXT NOT NULL DEFAULT '', broken INTEGER NOT NULL DEFAULT 0, insecure INTEGER NOT NULL DEFAULT 0, unfree INTEGER NOT NULL DEFAULT 0)",
        "CREATE TABLE info (key TEXT PRIMARY KEY, value TEXT)",
        "CREATE INDEX pkgs_pname ON pkgs (pname)",
        "CREATE VIEW listed_pkgs AS SELECT * FROM pkgs WHERE listed = 1",
    ] {
        sqlx::query(ddl).execute(&mut *conn).await?;
    }
    if let Some(b) = builtin {
        match copy(conn, &b.db, Mode::Builtin { free: b.free_listed, unfree: b.unfree_listed }).await {
            Ok(dropped) => report.dropped += dropped,
            Err(e) => bail!("package database {}: {e:#}", b.db.display()),
        }
    }
    for t in third {
        let db = if t.path.is_dir() { t.path.join(CATALOGUE_DB) } else { t.path.clone() };
        match copy(conn, &db, Mode::Third { id: &t.id, listed: t.listed }).await {
            Ok(dropped) => report.dropped += dropped,
            Err(e) => report.rejected.push((t.id.clone(), format!("{e:#}"))),
        }
    }
    if let Some(b) = builtin {
        // Carries `nixpkgs-source` and `system` over.
        if attach(conn, &b.db).await.is_ok() {
            sqlx::query("INSERT OR IGNORE INTO main.info SELECT key, value FROM src.info").execute(&mut *conn).await.ok();
            detach(conn).await;
        }
    }
    for (k, v) in [
        ("nsc-dropped", report.dropped.to_string()),
        ("nsc-rejected", serde_json::to_string(&report.rejected)?),
    ] {
        sqlx::query("INSERT OR REPLACE INTO main.info VALUES ($1, $2)").bind(k).bind(v).execute(&mut *conn).await?;
    }
    Ok(())
}

enum Mode<'a> {
    Builtin { free: bool, unfree: bool },
    Third { id: &'a str, listed: bool },
}

async fn attach(conn: &mut SqliteConnection, db: &Path) -> Result<()> {
    if !db.is_file() {
        bail!("{} is not a file", db.display());
    }
    sqlx::query("ATTACH DATABASE $1 AS src")
        .bind(db.to_string_lossy().into_owned())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn detach(conn: &mut SqliteConnection) {
    let _ = sqlx::query("DETACH DATABASE src").execute(&mut *conn).await;
}

/// SQL: `x` is a text value matching the attribute allowlist.
fn valid(x: &str) -> String {
    format!("(typeof({x}) = 'text' AND length({x}) BETWEEN 1 AND 256 AND {x} NOT GLOB '*[^A-Za-z0-9_+.-]*')")
}

async fn copy(conn: &mut SqliteConnection, db: &Path, mode: Mode<'_>) -> Result<u64> {
    attach(conn, db).await?;
    let r = copy_attached(conn, &mode).await;
    detach(conn).await;
    r
}

async fn copy_attached(conn: &mut SqliteConnection, mode: &Mode<'_>) -> Result<u64> {
    // Anything but plain tables could run code or redirect a query: views, triggers, virtual tables.
    let objects: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT type, name, sql FROM src.sqlite_master").fetch_all(&mut *conn).await.context("not a database")?;
    for (ty, name, sql) in &objects {
        let virt = sql.as_deref().map_or(false, |s| s.trim_start().to_ascii_uppercase().starts_with("CREATE VIRTUAL"));
        if ty == "view" || ty == "trigger" || virt {
            bail!("contains a {ty} ({name}); only tables are accepted");
        }
    }
    let has = |t: &str| objects.iter().any(|(ty, n, _)| ty == "table" && n == t);
    if !has("pkgs") || !has("meta") {
        bail!("missing the pkgs or meta table");
    }
    let pkgs_cols = columns(conn, "pkgs").await?;
    let meta_cols = columns(conn, "meta").await?;
    for c in PKGS_COLS {
        if !pkgs_cols.iter().any(|x| x == c) {
            bail!("pkgs lacks the column {c}");
        }
    }
    for c in META_COLS {
        if !meta_cols.iter().any(|x| x == c) {
            bail!("meta lacks the column {c}");
        }
    }
    if let Mode::Third { .. } = mode {
        let v: Option<(String,)> = sqlx::query_as("SELECT CAST(value AS TEXT) FROM src.info WHERE key = 'nsc-catalogue-version'")
            .fetch_optional(&mut *conn)
            .await
            .unwrap_or(None);
        match v {
            Some((v,)) if v == CATALOGUE_VERSION => {}
            Some((v,)) => bail!("catalogue version {v}, expected {CATALOGUE_VERSION}"),
            None => bail!("no nsc-catalogue-version in info"),
        }
    }
    let unfree = if meta_cols.iter().any(|x| x == "unfree") { "(CASE WHEN m.unfree = 1 THEN 1 ELSE 0 END)" } else { "0" };
    let (attr, mattr, source, listed, pname_check) = match mode {
        Mode::Builtin { free, unfree: unfree_listed } => (
            "p.attribute".to_string(),
            "m.attribute".to_string(),
            format!("(CASE WHEN {unfree} = 1 THEN '{UNFREE_ID}' ELSE '{NIXPKGS_ID}' END)"),
            format!("(CASE WHEN {unfree} = 1 THEN {} ELSE {} END)", *unfree_listed as i64, *free as i64),
            "1".to_string(),
        ),
        Mode::Third { id, listed } => {
            if !crate::sources::is_id(id) {
                bail!("invalid source id");
            }
            (
                format!("('{id}:' || p.attribute)"),
                format!("('{id}:' || m.attribute)"),
                format!("'{id}'"),
                (*listed as i64).to_string(),
                valid("p.pname"),
            )
        }
    };
    let (total,): (i64,) = sqlx::query_as("SELECT count(*) FROM src.pkgs").fetch_one(&mut *conn).await?;
    let text = |c: &str| format!("substr(CAST(COALESCE({c}, '') AS TEXT), 1, 100000)");
    let sql = format!(
        "INSERT OR IGNORE INTO main.pkgs (attribute, pname, version, system, source, listed)
         SELECT {attr}, {pname}, {version}, {system}, {source}, {listed}
         FROM src.pkgs p LEFT JOIN src.meta m ON m.attribute = p.attribute
         WHERE {va} AND {pname_check}",
        pname = text("p.pname"),
        version = text("p.version"),
        system = text("p.system"),
        va = valid("p.attribute"),
    );
    let inserted = sqlx::query(&sql).execute(&mut *conn).await?.rows_affected();
    let sql = format!(
        "INSERT OR IGNORE INTO main.meta
         SELECT {mattr}, {d}, {ld}, {h}, {l}, {pl}, {mt}, {po},
                (CASE WHEN m.broken = 1 THEN 1 ELSE 0 END), (CASE WHEN m.insecure = 1 THEN 1 ELSE 0 END), {unfree}
         FROM src.meta m
         WHERE {vm} AND EXISTS (SELECT 1 FROM main.pkgs q WHERE q.attribute = {mattr})",
        d = text("m.description"),
        ld = text("m.longdescription"),
        h = text("m.homepage"),
        l = text("m.license"),
        pl = text("m.platforms"),
        mt = text("m.maintainers"),
        po = text("m.position"),
        vm = valid("m.attribute"),
    );
    sqlx::query(&sql).execute(&mut *conn).await?;
    Ok((total as u64).saturating_sub(inserted))
}

async fn columns(conn: &mut SqliteConnection, table: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info($1, 'src')")
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}
