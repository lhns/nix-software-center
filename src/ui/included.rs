//! Apps built into the system image (`NSC_SYSTEM_MANIFEST`), resolved once at startup against
//! the loaded catalogue. An image update replaces the manifest, so it is never reloaded.

use nsc_backend::system_manifest;
use sqlx::SqlitePool;
use std::{collections::HashSet, sync::OnceLock};

static INCLUDED: OnceLock<HashSet<String>> = OnceLock::new();

pub async fn load(pool: &SqlitePool) {
    let Some(entries) = system_manifest::from_env() else { return };
    let attrs = system_manifest::resolve(pool, &entries).await;
    log::info!("system manifest: {} entries, {} catalogue apps included with the desktop", entries.len(), attrs.len());
    let _ = INCLUDED.set(attrs);
}

/// `user_installed`: the user's own install of it (profile or packages.json), which wins.
pub fn shown(attr: &str, user_installed: bool) -> bool {
    INCLUDED.get().map_or(false, |s| system_manifest::is_included(s, attr, user_installed))
}
