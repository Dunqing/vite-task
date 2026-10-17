pub mod channel;
mod ipc_path;
use std::iter;

use bumpalo::Bump;
pub use ipc_path::IpcPath;
pub use vt_ipc_str::IpcStr;
use wincode::{SchemaRead, SchemaWrite};

/// The kind of a single-path access, for building a record before its path
/// is resolved.
///
/// The clients choose it from an intercepted call's arguments, resolve the
/// path, then build the record with [`AccessMode::with_path`]. It is never
/// sent: [`PathAccess`] is the record, and accesses that touch more than one
/// path build it directly.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AccessMode {
    Read,
    Write,
    ReadWrite,
    ReadDir,
}

impl AccessMode {
    /// Builds the record for `path`. `created` is whether the path did not
    /// exist before the call; only the write modes keep it, since only a
    /// write can create a path.
    #[must_use]
    pub const fn with_path(self, path: &IpcPath, created: bool) -> PathAccess<'_> {
        match self {
            Self::Read => PathAccess::Read(path),
            Self::Write => PathAccess::Write { path, created },
            Self::ReadWrite => PathAccess::ReadWrite { path, created },
            Self::ReadDir => PathAccess::ReadDir(path),
        }
    }
}

/// A file system access made by a traced process, with the paths it touched.
///
/// A write carries whether the path did not exist before the call
/// (`created`). On Unix the clients check right before a call that can
/// create its path: an `O_CREAT` open, `mkdir`, or the destination of
/// `rename`, `link` or `symlink`. On Windows `NtCreateFile` reports it, and
/// rename and link destinations are checked before the call. Failed calls
/// are recorded like successful ones, so a created path can still be
/// missing afterwards.
// TODO: add follow_symlinks (O_NOFOLLOW)
#[derive(SchemaWrite, SchemaRead, Debug, Clone, Copy, PartialEq, Eq)]
#[wincode(tag_encoding = "u8")]
pub enum PathAccess<'a> {
    Read(&'a IpcPath),
    Write { path: &'a IpcPath, created: bool },
    ReadWrite { path: &'a IpcPath, created: bool },
    ReadDir(&'a IpcPath),
}

impl<'a> PathAccess<'a> {
    /// The paths this access touched.
    pub fn paths(self) -> impl Iterator<Item = &'a IpcPath> {
        match self {
            PathAccess::Read(path)
            | PathAccess::Write { path, .. }
            | PathAccess::ReadWrite { path, .. }
            | PathAccess::ReadDir(path) => iter::once(path),
        }
    }

    #[must_use]
    pub fn clone_in<'bump>(&self, bump: &'bump Bump) -> PathAccess<'bump> {
        match *self {
            PathAccess::Read(path) => PathAccess::Read(path.clone_in(bump)),
            PathAccess::Write { path, created } => {
                PathAccess::Write { path: path.clone_in(bump), created }
            }
            PathAccess::ReadWrite { path, created } => {
                PathAccess::ReadWrite { path: path.clone_in(bump), created }
            }
            PathAccess::ReadDir(path) => PathAccess::ReadDir(path.clone_in(bump)),
        }
    }
}
