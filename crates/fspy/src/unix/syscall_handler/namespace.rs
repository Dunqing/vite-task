//! Syscalls that create or replace a directory entry without opening it.
//! The entry is recorded as a write, created when it does not exist when
//! the call is about to run. The source of a rename needs no record.

use std::io;

use fspy_seccomp_unotify::supervisor::handler::arg::{CStrPtr, Caller, Fd, Ignored};

use super::SyscallHandler;

impl SyscallHandler {
    #[cfg(target_arch = "x86_64")]
    pub(super) fn mkdir(&mut self, caller: Caller, (path,): (CStrPtr,)) -> io::Result<()> {
        self.handle_create(caller, Fd::cwd(), path)
    }

    pub(super) fn mkdirat(
        &mut self,
        caller: Caller,
        (dir_fd, path): (Fd, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, dir_fd, path)
    }

    #[cfg(target_arch = "x86_64")]
    pub(super) fn rename(
        &mut self,
        caller: Caller,
        (_old, new): (Ignored, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, Fd::cwd(), new)
    }

    pub(super) fn renameat(
        &mut self,
        caller: Caller,
        (_old_dir_fd, _old, new_dir_fd, new): (Ignored, Ignored, Fd, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, new_dir_fd, new)
    }

    /// With `RENAME_EXCHANGE` both paths are written; both exist, so neither
    /// is created, and the destination's record covers the one that changes
    /// name the way a plain rename does.
    pub(super) fn renameat2(
        &mut self,
        caller: Caller,
        (_old_dir_fd, _old, new_dir_fd, new): (Ignored, Ignored, Fd, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, new_dir_fd, new)
    }

    #[cfg(target_arch = "x86_64")]
    pub(super) fn link(
        &mut self,
        caller: Caller,
        (_old, new): (Ignored, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, Fd::cwd(), new)
    }

    pub(super) fn linkat(
        &mut self,
        caller: Caller,
        (_old_dir_fd, _old, new_dir_fd, new): (Ignored, Ignored, Fd, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, new_dir_fd, new)
    }

    /// The target is stored as text, not accessed.
    #[cfg(target_arch = "x86_64")]
    pub(super) fn symlink(
        &mut self,
        caller: Caller,
        (_target, link_path): (Ignored, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, Fd::cwd(), link_path)
    }

    pub(super) fn symlinkat(
        &mut self,
        caller: Caller,
        (_target, new_dir_fd, link_path): (Ignored, Fd, CStrPtr),
    ) -> io::Result<()> {
        self.handle_create(caller, new_dir_fd, link_path)
    }
}
