//! GTK-free backend of the software center: profile listing, Nix process
//! running with progress, profile operations and the package database.

pub mod catalogue;
pub mod helper;
pub mod launch;
pub mod memory;
pub mod ops;
pub mod pkgdb;
pub mod profile;
pub mod progress;
pub mod queue;
pub mod run;
pub mod shell;
pub mod sources;
pub mod system_manifest;

pub use anyhow::Result;
