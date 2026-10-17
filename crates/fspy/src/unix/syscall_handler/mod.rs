mod execve;
mod getdents;
mod namespace;
mod open;
mod stat;

use std::{
    borrow::Cow,
    ffi::{OsStr, c_int},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use fspy_seccomp_unotify::{
    impl_handler,
    supervisor::handler::arg::{CStrPtr, Caller, Fd},
};
use fspy_shared::ipc::{AccessMode, PathAccess, channel::Sender};

const PATH_MAX: usize = libc::PATH_MAX as usize;

/// Records the accesses of intercepted syscalls into the IPC channel.
///
/// The supervisor lets a syscall continue only after its handler returns, so
/// every record is published before the access it describes, as the channel
/// requires of its senders. For the same reason, whether a call creates its
/// path is known here: the path is checked before the call runs.
pub struct SyscallHandler {
    ipc_sender: Arc<Sender>,
    path_read_buf: [u8; PATH_MAX],
}

impl SyscallHandler {
    pub const fn new(ipc_sender: Arc<Sender>) -> Self {
        Self { ipc_sender, path_read_buf: [0; PATH_MAX] }
    }

    fn handle_open(
        &mut self,
        caller: Caller,
        dir_fd: Fd,
        path_ptr: CStrPtr,
        flags: c_int,
    ) -> io::Result<()> {
        let Self { ipc_sender, path_read_buf } = self;
        let Some(path) = resolve_path(path_read_buf, caller, dir_fd, path_ptr)? else {
            return Ok(());
        };
        let mode = match flags & libc::O_ACCMODE {
            libc::O_RDWR => AccessMode::ReadWrite,
            libc::O_WRONLY => AccessMode::Write,
            _ => AccessMode::Read,
        };
        let created = flags & libc::O_CREAT != 0 && !exists(&path);
        ipc_sender.send(&mode.with_path(path.as_os_str().into(), created));
        Ok(())
    }

    /// Records a call that creates or replaces the entry at `path` without
    /// opening it: `mkdir`, and the destinations of `rename`, `link` and
    /// `symlink`.
    fn handle_create(&mut self, caller: Caller, dir_fd: Fd, path_ptr: CStrPtr) -> io::Result<()> {
        let Self { ipc_sender, path_read_buf } = self;
        let Some(path) = resolve_path(path_read_buf, caller, dir_fd, path_ptr)? else {
            return Ok(());
        };
        let created = !exists(&path);
        ipc_sender.send(&PathAccess::Write { path: path.as_os_str().into(), created });
        Ok(())
    }

    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "same receiver as `handle_open`, which writes `path_read_buf`"
    )]
    fn handle_open_dir(&mut self, caller: Caller, fd: Fd) -> io::Result<()> {
        let path = fd.get_path(caller)?;
        self.ipc_sender.send(&PathAccess::ReadDir(OsStr::from_bytes(path.as_bytes()).into()));
        Ok(())
    }
}

/// Reads the caller's path argument into `buf` and makes it absolute against
/// `dir_fd`. `None` when the path does not fit in `PATH_MAX`.
fn resolve_path<'buf>(
    buf: &'buf mut [u8; PATH_MAX],
    caller: Caller,
    dir_fd: Fd,
    path_ptr: CStrPtr,
) -> io::Result<Option<Cow<'buf, Path>>> {
    let Some(path_len) = path_ptr.read(caller, buf)? else {
        // Ignore paths that are too long to fit in PATH_MAX
        return Ok(None);
    };
    let path = Path::new(OsStr::from_bytes(&buf[..path_len]));
    if path.is_absolute() {
        return Ok(Some(Cow::Borrowed(path)));
    }
    let mut resolved_path = PathBuf::from(dir_fd.get_path(caller)?);
    if !nix::NixPath::is_empty(path) {
        resolved_path.push(path);
    }
    Ok(Some(Cow::Owned(resolved_path)))
}

/// Whether `path` exists right now, without following a final symlink. A
/// check that fails for a reason other than a missing entry counts as
/// existing: a path wrongly recorded as pre-existing is held to its exact
/// state later, which is the stricter mistake.
fn exists(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(err) => !matches!(err.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory),
    }
}

impl_handler!(
    SyscallHandler:

    #[cfg(target_arch = "x86_64")] open,
    openat,
    openat2,
    #[cfg(target_arch = "x86_64")] creat,

    #[cfg(target_arch = "x86_64")] getdents,
    getdents64,

    #[cfg(target_arch = "x86_64")] stat,
    #[cfg(target_arch = "x86_64")] lstat,
    #[cfg(target_arch = "x86_64")] newfstatat,
    #[cfg(target_arch = "aarch64")] fstatat,
    statx,

    #[cfg(target_arch = "x86_64")] access,
    faccessat,
    faccessat2,

    #[cfg(target_arch = "x86_64")] mkdir,
    mkdirat,
    #[cfg(target_arch = "x86_64")] rename,
    renameat,
    renameat2,
    #[cfg(target_arch = "x86_64")] link,
    linkat,
    #[cfg(target_arch = "x86_64")] symlink,
    symlinkat,

    execve,
    execveat,
);
