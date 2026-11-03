//! GTK-free backend of the software center: profile listing, Nix process
//! running with progress, profile operations and the package database.

pub mod ops;
pub mod pkgdb;
pub mod profile;
pub mod progress;
pub mod run;

pub use anyhow::Result;
