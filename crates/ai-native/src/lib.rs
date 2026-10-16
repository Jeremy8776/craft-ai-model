//! Native-only pieces of the Craft AI layer. Desktop builds only: this crate does file and
//! operating system I/O and must never be a dependency of a wasm build. The data model it serves
//! lives in `craft-ai-core`, which stays I/O free.
//!
//! # Map
//!
//! | Module | What it holds |
//! |---|---|
//! | [`secrets`] | the [`SecretStore`](secrets::SecretStore) trait and its stores: OS keychain, passphrase-encrypted file, environment, in-memory, and a layered chain |
//! | [`shared`] | [`SharedStore`](shared::SharedStore): the launcher's versioned connections file, read by apps, updated under a lock with an atomic write |
//! | [`ConsentFile`] | one app's consent ledger on disk; damage makes the app ask again |
//! | [`HistoryFile`] | the shared request log (`jobs.jsonl`): locked appends, size-capped rotation, tolerant reads |
//! | [`paths`] | where those files live on each OS |
//!
//! # Rules this crate keeps
//!
//! - A secret value never reaches `Debug`, `Display`, an error message, a log line or a file in
//!   the clear. The shared store file has no field that can hold one.
//! - Nothing here panics on bad input. A truncated, mutated, oversized or newer-schema file is an
//!   error.
//! - Writes are atomic (temp file, flush, rename) and serialised by a lock file, so two processes
//!   never interleave and a crash never leaves half a file.

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]

mod catalog_file;
mod consent_file;
mod fsutil;
mod history_file;
pub mod paths;
pub mod secrets;
pub mod shared;

pub use catalog_file::{CatalogFile, CatalogLoad};
pub use consent_file::ConsentFile;
pub use history_file::{DEFAULT_MAX_BYTES, HistoryFile, HistoryRead};
