//! GTK-free backend of the software center: profile listing, Nix process
//! running with progress, profile operations and the package database.

pub mod catalogue;
pub mod ops;
pub mod pkgdb;
pub mod profile;
pub mod progress;
pub mod run;
pub mod sources;

pub use anyhow::Result;
