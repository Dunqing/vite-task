# fspy

Run a command and capture all the paths it tries to access.

## What is recorded

Each record (`fspy_shared::ipc::PathAccess`) is a path with how it was accessed: `Read` (opened for reading, stat'ed, checked, or executed), `Write`, `ReadWrite`, or `ReadDir` (its entries were listed). Calls are recorded whether or not they succeed.

A write also carries whether the path was **created**: it did not exist right before the call. On Unix, the clients check the path with one `lstat` before a call that can create it (`open` with `O_CREAT`, `mkdir`, and the destinations of `rename`, `link` and `symlink`). The seccomp supervisor does the same check, since its notification arrives before the syscall runs. On Windows, `NtCreateFile` reports it in its status block (`FILE_CREATED`), and rename and link destinations passed to `NtSetInformationFile` are checked before the call.

The destination of a rename, hard link or symlink is a write; the source needs no record, and a symlink's target is stored as text, not accessed. Deletions are not recorded.

## macOS/Linux (glibc) implementation

It uses `DYLD_INSERT_LIBRARIES` on macOS and `LD_PRELOAD` on Linux to inject a shared library that intercepts file system calls.
The injection process is almost identical on both platforms other than the environment variable name. The implementation is in `src/unix`.

## Linux-specific implementation for fully static binaries

For fully static binaries (such as `esbuild`), `LD_PRELOAD` does not work. In this case, `seccomp_unotify` is used to intercept direct system calls. The handler is implemented in `src/unix/syscall_handler`. It records accesses into the same shared-memory channel as the preload library (`fspy_shared::ipc::channel`).

## Linux musl implementation

On musl targets, only `seccomp_unotify`-based tracking is used (no preload library).

## Windows implementation

It uses [Detours](https://github.com/microsoft/Detours) to intercept file system calls. The implementation is in `src/windows`.

## Unified interface

The unified interface of `Command` is in `src/command.rs`.

## Preload Libraries

`DYLD_INSERT_LIBRARIES`, `LD_PRELOAD`, `Detours` all require a shared library to be injected. The shared libraries of macOS/Linux are in the `fspy_preload_unix` crate, and the shared library of Windows is in the `fspy_preload_windows` crate.
