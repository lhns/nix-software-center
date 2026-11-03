//! Reads the nix-data schema SQLite package database.

use sqlx::SqlitePool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// No row in `meta`: not in this nixpkgs.
    Missing,
    Broken,
    Insecure,
}

pub async fn connect(path: &std::path::Path) -> sqlx::Result<SqlitePool> {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true);
    SqlitePool::connect_with(opts).await
}

pub async fn version(pool: &SqlitePool, attr: &str) -> Option<String> {
    let row: Result<Option<(Option<String>,)>, _> =
        sqlx::query_as("SELECT version FROM pkgs WHERE attribute = $1 LIMIT 1")
            .bind(attr)
            .fetch_optional(pool)
            .await;
    match row {
        Ok(r) => r.and_then(|r| r.0).filter(|v| !v.is_empty()),
        Err(e) => {
            log::warn!("pkgdb version({attr}): {e}");
            None
        }
    }
}

/// The attributes that are missing, broken or insecure; available ones are left out.
pub async fn unavailable(pool: &SqlitePool, attrs: &[String]) -> Vec<(String, Unavailable)> {
    let mut out = Vec::new();
    for attr in attrs {
        let row: Result<Option<(i64, i64)>, _> =
            sqlx::query_as("SELECT broken, insecure FROM meta WHERE attribute = $1")
                .bind(attr)
                .fetch_optional(pool)
                .await;
        match row {
            Ok(None) => out.push((attr.clone(), Unavailable::Missing)),
            Ok(Some((1, _))) => out.push((attr.clone(), Unavailable::Broken)),
            Ok(Some((_, 1))) => out.push((attr.clone(), Unavailable::Insecure)),
            Ok(Some(_)) => {}
            Err(e) => log::warn!("pkgdb unavailable({attr}): {e}"),
        }
    }
    out
}
