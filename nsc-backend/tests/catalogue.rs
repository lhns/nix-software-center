use nsc_backend::catalogue::{merge, merged_path, Builtin, Third};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::path::{Path, PathBuf};

const PKGS: &str = "CREATE TABLE pkgs (attribute TEXT NOT NULL PRIMARY KEY, pname TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '')";
const META: &str = "CREATE TABLE meta (attribute TEXT NOT NULL PRIMARY KEY, description TEXT NOT NULL DEFAULT '', longdescription TEXT NOT NULL DEFAULT '', homepage TEXT NOT NULL DEFAULT '', license TEXT NOT NULL DEFAULT '', platforms TEXT NOT NULL DEFAULT '', maintainers TEXT NOT NULL DEFAULT '', position TEXT NOT NULL DEFAULT '', broken INTEGER NOT NULL DEFAULT 0, insecure INTEGER NOT NULL DEFAULT 0, unfree INTEGER NOT NULL DEFAULT 0)";
const INFO: &str = "CREATE TABLE info (key TEXT PRIMARY KEY, value TEXT)";

/// `rows`: (attribute, pname, unfree).
async fn make(path: &Path, version: Option<&str>, rows: &[(&str, &str, i64)], extra: &[&str]) {
    let mut c = SqliteConnectOptions::new().filename(path).create_if_missing(true).connect().await.unwrap();
    for q in [PKGS, META, INFO].into_iter().chain(extra.iter().copied()) {
        sqlx::query(q).execute(&mut c).await.unwrap();
    }
    if let Some(v) = version {
        sqlx::query("INSERT INTO info VALUES ('nsc-catalogue-version', $1)").bind(v).execute(&mut c).await.unwrap();
    }
    for (a, p, u) in rows {
        sqlx::query("INSERT INTO pkgs (attribute, pname, version, system) VALUES ($1, $2, '1.0', 'x86_64-linux')")
            .bind(a)
            .bind(p)
            .execute(&mut c)
            .await
            .unwrap();
        sqlx::query("INSERT INTO meta (attribute, description, unfree) VALUES ($1, 'd', $2)")
            .bind(a)
            .bind(u)
            .execute(&mut c)
            .await
            .unwrap();
    }
    c.close().await.unwrap();
}

async fn rows(db: &Path, sql: &str) -> Vec<(String, String)> {
    let pool = nsc_backend::pkgdb::connect(db).await.unwrap();
    let r: Vec<(String, String)> = sqlx::query_as(sql).fetch_all(&pool).await.unwrap();
    pool.close().await;
    r
}

fn third(id: &str, path: PathBuf, listed: bool) -> Third {
    Third { id: id.into(), path, listed }
}

fn builtin(db: PathBuf, free: bool, unfree: bool) -> Builtin {
    Builtin { db, free_listed: free, unfree_listed: unfree }
}

fn s(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[tokio::test]
async fn merges_namespaces_and_splits_unfree() {
    let d = tempfile::tempdir().unwrap();
    let b = d.path().join("b.db");
    make(&b, None, &[("hello", "hello", 0), ("slack", "slack", 1)], &[]).await;
    let t1 = d.path().join("t1.db");
    let t2 = d.path().join("t2.db");
    make(&t1, Some("1"), &[("hello", "hello", 0), ("tool", "tool", 1)], &[]).await;
    make(&t2, Some("1"), &[("hello", "hello", 0)], &[]).await;
    let m = merge(d.path(), Some(&builtin(b, true, false)), &[third("one", t1, true), third("two", t2, false)])
        .await
        .unwrap();
    assert_eq!(m.dropped, 0);
    assert!(m.rejected.is_empty());
    // Built-in first, then sources in the given order; duplicates all survive, namespaced.
    let all = rows(&m.path, "SELECT attribute, source FROM pkgs ORDER BY rowid").await;
    assert_eq!(
        all,
        [s("hello", "nixpkgs"), s("slack", "nixpkgs-unfree"), s("one:hello", "one"), s("one:tool", "one"), s("two:hello", "two")]
    );
    // Listing follows each source's enabled state; lookups still see hidden ones.
    let listed = rows(&m.path, "SELECT l.attribute, l.source FROM listed_pkgs l JOIN pkgs p ON p.attribute = l.attribute ORDER BY p.rowid").await;
    assert_eq!(listed, [s("hello", "nixpkgs"), s("one:hello", "one"), s("one:tool", "one")]);
    let unfree = rows(&m.path, "SELECT attribute, CAST(unfree AS TEXT) FROM meta WHERE unfree = 1 ORDER BY 1").await;
    assert_eq!(unfree, [s("one:tool", "1"), s("slack", "1")]);
    let joined = rows(
        &m.path,
        "SELECT p.attribute, m.description FROM pkgs p JOIN meta m USING (attribute) WHERE p.attribute = 'two:hello'",
    )
    .await;
    assert_eq!(joined, [s("two:hello", "d")]);
}

#[tokio::test]
async fn invalid_rows_are_dropped_and_counted() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("t.db");
    make(
        &t,
        Some("1"),
        &[("ok", "ok", 0), ("a b", "x", 0), ("semi;colon", "x", 0), ("fine", "bad name", 0), ("quote\"", "q", 0), ("", "e", 0)],
        &[],
    )
    .await;
    let m = merge(d.path(), None, &[third("src", t, true)]).await.unwrap();
    assert_eq!(m.dropped, 5);
    let all = rows(&m.path, "SELECT attribute, pname FROM pkgs").await;
    assert_eq!(all, [s("src:ok", "ok")]);
}

#[tokio::test]
async fn wrong_or_missing_version_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let (v2, none) = (d.path().join("v2.db"), d.path().join("none.db"));
    make(&v2, Some("2"), &[("a", "a", 0)], &[]).await;
    make(&none, None, &[("a", "a", 0)], &[]).await;
    let missing = d.path().join("nope.db");
    let m = merge(
        d.path(),
        None,
        &[third("v2", v2, true), third("none", none, true), third("missing", missing, true)],
    )
    .await
    .unwrap();
    let ids: Vec<_> = m.rejected.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(ids, ["v2", "none", "missing"]);
    assert!(m.rejected[0].1.contains("version"), "{:?}", m.rejected);
    assert!(rows(&m.path, "SELECT attribute, source FROM pkgs").await.is_empty());
}

#[tokio::test]
async fn hostile_databases_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    // A view in place of a table: a later query would run the view's SQL.
    let view = d.path().join("view.db");
    let mut c = SqliteConnectOptions::new().filename(&view).create_if_missing(true).connect().await.unwrap();
    for q in [
        "CREATE TABLE real_pkgs (attribute TEXT, pname TEXT, version TEXT, system TEXT)",
        "CREATE VIEW pkgs AS SELECT * FROM real_pkgs",
        META,
        INFO,
        "INSERT INTO info VALUES ('nsc-catalogue-version', '1')",
    ] {
        sqlx::query(q).execute(&mut c).await.unwrap();
    }
    c.close().await.unwrap();
    let trig = d.path().join("trigger.db");
    make(&trig, Some("1"), &[("a", "a", 0)], &["CREATE TRIGGER t AFTER INSERT ON pkgs BEGIN DELETE FROM meta; END"]).await;
    let garbage = d.path().join("garbage.db");
    std::fs::write(&garbage, b"not a database at all, just some text that is long enough to look like a header").unwrap();

    let m = merge(
        d.path(),
        None,
        &[third("view", view, true), third("trig", trig, true), third("junk", garbage, true)],
    )
    .await
    .unwrap();
    assert_eq!(m.rejected.len(), 3, "{:?}", m.rejected);
    assert!(m.rejected[0].1.contains("view"), "{:?}", m.rejected);
    assert!(m.rejected[1].1.contains("trigger"), "{:?}", m.rejected);
    assert!(rows(&m.path, "SELECT attribute, source FROM pkgs").await.is_empty());
}

#[tokio::test]
async fn path_is_stable_and_keyed_on_inputs() {
    let d = tempfile::tempdir().unwrap();
    let b = d.path().join("b.db");
    make(&b, None, &[("a", "a", 0)], &[]).await;
    let p1 = merged_path(d.path(), Some(&builtin(b.clone(), true, true)), &[]);
    assert_eq!(p1, merged_path(d.path(), Some(&builtin(b.clone(), true, true)), &[]));
    assert_ne!(p1, merged_path(d.path(), Some(&builtin(b.clone(), true, false)), &[]));
    assert_ne!(p1, merged_path(d.path(), Some(&builtin(b.clone(), true, true)), &[third("x", b.clone(), true)]));
    let m = merge(d.path(), Some(&builtin(b.clone(), true, true)), &[]).await.unwrap();
    assert_eq!(m.path, p1);
    // A second merge reuses the file and reports the same.
    let again = merge(d.path(), Some(&builtin(b, true, true)), &[]).await.unwrap();
    assert_eq!(again, m);
}
