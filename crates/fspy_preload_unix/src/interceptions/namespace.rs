//! Calls that create or replace a directory entry without opening it: the
//! entry is recorded as a write, created when it did not exist before the
//! call. The source of a rename needs no record.

use libc::{c_char, c_int, mode_t};

use crate::{
    client::{
        convert::{Access, PathAt},
        handle_open,
    },
    macros::intercept,
};

intercept!(mkdir: unsafe extern "C" fn(path: *const c_char, mode: mode_t) -> c_int);
unsafe extern "C" fn mkdir(path: *const c_char, mode: mode_t) -> c_int {
    // SAFETY: path is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_open(fspy_nostd::CStr::from_ptr(path.cast()), Access::CREATE) };
    // SAFETY: calling the original libc mkdir() with the same arguments forwarded from the interposed function
    unsafe { mkdir::original()(path, mode) }
}

intercept!(mkdirat: unsafe extern "C" fn(dirfd: c_int, path: *const c_char, mode: mode_t) -> c_int);
unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: mode_t) -> c_int {
    // SAFETY: dirfd and path are valid arguments provided by the caller of the interposed function
    unsafe { handle_open(PathAt::borrow_raw(dirfd, path), Access::CREATE) };
    // SAFETY: calling the original libc mkdirat() with the same arguments forwarded from the interposed function
    unsafe { mkdirat::original()(dirfd, path, mode) }
}

intercept!(rename: unsafe extern "C" fn(old: *const c_char, new: *const c_char) -> c_int);
unsafe extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: new is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_open(fspy_nostd::CStr::from_ptr(new.cast()), Access::CREATE) };
    // SAFETY: calling the original libc rename() with the same arguments forwarded from the interposed function
    unsafe { rename::original()(old, new) }
}

intercept!(renameat: unsafe extern "C" fn(olddirfd: c_int, old: *const c_char, newdirfd: c_int, new: *const c_char) -> c_int);
unsafe extern "C" fn renameat(
    olddirfd: c_int,
    old: *const c_char,
    newdirfd: c_int,
    new: *const c_char,
) -> c_int {
    // SAFETY: newdirfd and new are valid arguments provided by the caller of the interposed function
    unsafe { handle_open(PathAt::borrow_raw(newdirfd, new), Access::CREATE) };
    // SAFETY: calling the original libc renameat() with the same arguments forwarded from the interposed function
    unsafe { renameat::original()(olddirfd, old, newdirfd, new) }
}

#[cfg(target_os = "linux")]
intercept!(renameat2: unsafe extern "C" fn(olddirfd: c_int, old: *const c_char, newdirfd: c_int, new: *const c_char, flags: libc::c_uint) -> c_int);
#[cfg(target_os = "linux")]
unsafe extern "C" fn renameat2(
    olddirfd: c_int,
    old: *const c_char,
    newdirfd: c_int,
    new: *const c_char,
    flags: libc::c_uint,
) -> c_int {
    // With `RENAME_EXCHANGE` both paths are written; both exist, so neither
    // is created, and the destination's record covers the one that changes
    // name the way a plain rename does.
    // SAFETY: newdirfd and new are valid arguments provided by the caller of the interposed function
    unsafe { handle_open(PathAt::borrow_raw(newdirfd, new), Access::CREATE) };
    // SAFETY: calling the original libc renameat2() with the same arguments forwarded from the interposed function
    unsafe { renameat2::original()(olddirfd, old, newdirfd, new, flags) }
}

intercept!(link: unsafe extern "C" fn(old: *const c_char, new: *const c_char) -> c_int);
unsafe extern "C" fn link(old: *const c_char, new: *const c_char) -> c_int {
    // SAFETY: new is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_open(fspy_nostd::CStr::from_ptr(new.cast()), Access::CREATE) };
    // SAFETY: calling the original libc link() with the same arguments forwarded from the interposed function
    unsafe { link::original()(old, new) }
}

intercept!(linkat: unsafe extern "C" fn(olddirfd: c_int, old: *const c_char, newdirfd: c_int, new: *const c_char, flags: c_int) -> c_int);
unsafe extern "C" fn linkat(
    olddirfd: c_int,
    old: *const c_char,
    newdirfd: c_int,
    new: *const c_char,
    flags: c_int,
) -> c_int {
    // SAFETY: newdirfd and new are valid arguments provided by the caller of the interposed function
    unsafe { handle_open(PathAt::borrow_raw(newdirfd, new), Access::CREATE) };
    // SAFETY: calling the original libc linkat() with the same arguments forwarded from the interposed function
    unsafe { linkat::original()(olddirfd, old, newdirfd, new, flags) }
}

intercept!(symlink: unsafe extern "C" fn(target: *const c_char, linkpath: *const c_char) -> c_int);
unsafe extern "C" fn symlink(target: *const c_char, linkpath: *const c_char) -> c_int {
    // The target is stored as text, not accessed.
    // SAFETY: linkpath is a valid C string pointer provided by the caller of the interposed function
    unsafe { handle_open(fspy_nostd::CStr::from_ptr(linkpath.cast()), Access::CREATE) };
    // SAFETY: calling the original libc symlink() with the same arguments forwarded from the interposed function
    unsafe { symlink::original()(target, linkpath) }
}

intercept!(symlinkat: unsafe extern "C" fn(target: *const c_char, newdirfd: c_int, linkpath: *const c_char) -> c_int);
unsafe extern "C" fn symlinkat(
    target: *const c_char,
    newdirfd: c_int,
    linkpath: *const c_char,
) -> c_int {
    // SAFETY: newdirfd and linkpath are valid arguments provided by the caller of the interposed function
    unsafe { handle_open(PathAt::borrow_raw(newdirfd, linkpath), Access::CREATE) };
    // SAFETY: calling the original libc symlinkat() with the same arguments forwarded from the interposed function
    unsafe { symlinkat::original()(target, newdirfd, linkpath) }
}
