//! Starr domain core.
//!
//! This crate holds domain types and the contracts other modules implement
//! (adapters, jobs). It must stay free of IO: no database, no HTTP, no
//! filesystem. Infrastructure crates depend on `starr-core`, never the reverse.

pub mod accounts;
pub mod adapters;
pub mod auth;
pub mod catalog;
pub mod categories;
pub mod follows;
pub mod inventory;
pub mod jobs;
pub mod journey;
pub mod libraries;
pub mod library;
pub mod members;
pub mod notifications;
pub mod provisioning;
pub mod requests;
pub mod source;
pub mod system;

/// Human-readable application name.
pub const APP_NAME: &str = "Starr";

/// Application version, taken from the workspace version at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
