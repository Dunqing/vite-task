//! Paths whose content carries noise — timestamps, machine-specific paths,
//! the state of pnpm's store — that would otherwise cause cache misses
//! without any meaningful change. Files are fingerprinted only by whether
//! they exist; the pnpm store directory counts as missing.
//!
//! To add another file, add a module here with a `matches` condition, and
//! check it in [`hash_file_content`](super::hash::hash_file_content). A
//! directory rule goes in [`fingerprint_path`](super::fingerprint::fingerprint_path).

pub(super) mod node_modules_bin;
pub(super) mod pnpm_modules_manifest;
pub(super) mod pnpm_store_bin_dir;
