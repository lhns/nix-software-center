use nsc_backend::pkgdb::{self, Unavailable};
use sqlx::sqlite::SqlitePoolOptions;

const SCHEMA: &[&str] = &[
    "CREATE TABLE pkgs (attribute TEXT NOT NULL PRIMARY KEY, pname TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '', system TEXT NOT NULL DEFAULT '')",
    "CREATE TABLE meta (attribute TEXT NOT NULL PRIMARY KEY, description TEXT NOT NULL DEFAULT '', longdescription TEXT NOT NULL DEFAULT '', homepage TEXT NOT NULL DEFAULT '', license TEXT NOT NULL DEFAULT '', platforms TEXT NOT NULL DEFAULT '', maintainers TEXT NOT NULL DEFAULT '', position TEXT NOT NULL DEFAULT '', broken INTEGER NOT NULL DEFAULT 0, insecure INTEGER NOT NULL DEFAULT 0)",
    "INSERT INTO pkgs (attribute, pname, version, system) VALUES ('hello','hello','2.12.1','x86_64-linux'), ('old','old','','x86_64-linux')",
    "INSERT INTO meta (attribute, broken, insecure) VALUES ('hello',0,0), ('old',0,0), ('brk',1,0), ('vuln',0,1), ('both',1,1)",
];

async fn fill(pool: &sqlx::SqlitePool) {
    for q in SCHEMA {
        sqlx::query(q).execute(pool).await.unwrap();
    }
}

#[tokio::test]
async fn queries_in_memory_db() {
    // One connection: every in-memory connection is its own database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    fill(&pool).await;

    assert_eq!(pkgdb::version(&pool, "hello").await.as_deref(), Some("2.12.1"));
    assert_eq!(pkgdb::version(&pool, "old").await, None);
    assert_eq!(pkgdb::version(&pool, "nope").await, None);

    let attrs: Vec<String> = ["hello", "gone", "brk", "vuln", "both"].map(String::from).into();
    assert_eq!(
        pkgdb::unavailable(&pool, &attrs).await,
        [
            ("gone".to_string(), Unavailable::Missing),
            ("brk".to_string(), Unavailable::Broken),
            ("vuln".to_string(), Unavailable::Insecure),
            ("both".to_string(), Unavailable::Broken),
        ]
    );
}

#[tokio::test]
async fn opens_a_db_file_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nixpkgs.db");
    let rw = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(rw).await.unwrap();
    fill(&pool).await;
    pool.close().await;

    let pool = pkgdb::connect(&path).await.unwrap();
    assert_eq!(pkgdb::version(&pool, "hello").await.as_deref(), Some("2.12.1"));
    assert!(sqlx::query("DELETE FROM pkgs").execute(&pool).await.is_err());
}
