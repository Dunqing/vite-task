mod test_utils;

use std::{
    env::current_dir,
    fs::{self, File, OpenOptions},
    process::Stdio,
};

use fspy::PathAccess;
use test_log::test;
use test_utils::{assert_contains, created, written};

#[test(tokio::test)]
async fn open_read() -> anyhow::Result<()> {
    let accesses = track_fn!((), |(): ()| {
        let _ = File::open("hello");
    })
    .await?;
    assert_contains(&accesses, current_dir().unwrap().join("hello").as_path(), PathAccess::Read);

    Ok(())
}

#[test(tokio::test)]
async fn open_write() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let tmp_path = tmp_dir.path().join("hello");
    let tmp_path_str = tmp_path.to_str().unwrap().to_owned();
    let accesses = track_fn!(tmp_path_str, |tmp_path_str: String| {
        let _ = OpenOptions::new().write(true).open(tmp_path_str);
    })
    .await?;
    // Without `O_CREAT` the open cannot create the file, so it is not a
    // creating write even though the file is missing.
    assert_contains(&accesses, tmp_path.as_path(), written);

    Ok(())
}

#[test(tokio::test)]
async fn create_missing_file() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let tmp_path = tmp_dir.path().join("hello");
    let accesses = track_fn!(tmp_path.to_str().unwrap().to_owned(), |tmp_path: String| {
        File::create(tmp_path).unwrap();
    })
    .await?;
    assert_contains(&accesses, tmp_path.as_path(), created);

    Ok(())
}

#[test(tokio::test)]
async fn create_existing_file() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let tmp_path = tmp_dir.path().join("hello");
    File::create(&tmp_path)?;
    let accesses = track_fn!(tmp_path.to_str().unwrap().to_owned(), |tmp_path: String| {
        File::create(tmp_path).unwrap();
    })
    .await?;
    assert_contains(&accesses, tmp_path.as_path(), written);

    Ok(())
}

#[test(tokio::test)]
async fn create_dir() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let dir_path = tmp_dir.path().join("hello_dir");
    let accesses = track_fn!(dir_path.to_str().unwrap().to_owned(), |dir_path: String| {
        fs::create_dir(dir_path).unwrap();
    })
    .await?;
    assert_contains(&accesses, dir_path.as_path(), created);

    Ok(())
}

#[test(tokio::test)]
async fn rename_destination() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let source = tmp_dir.path().join("source");
    let destination = tmp_dir.path().join("destination");
    File::create(&source)?;
    let accesses = track_fn!(tmp_dir.path().to_str().unwrap().to_owned(), |dir: String| {
        let dir = std::path::Path::new(&dir);
        fs::rename(dir.join("source"), dir.join("destination")).unwrap();
    })
    .await?;
    assert_contains(&accesses, destination.as_path(), created);

    Ok(())
}

#[test(tokio::test)]
async fn rename_over_existing_destination() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let source = tmp_dir.path().join("source");
    let destination = tmp_dir.path().join("destination");
    File::create(&source)?;
    File::create(&destination)?;
    let accesses = track_fn!(tmp_dir.path().to_str().unwrap().to_owned(), |dir: String| {
        let dir = std::path::Path::new(&dir);
        fs::rename(dir.join("source"), dir.join("destination")).unwrap();
    })
    .await?;
    assert_contains(&accesses, destination.as_path(), written);

    Ok(())
}

#[test(tokio::test)]
async fn hard_link_destination() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let original = tmp_dir.path().join("original");
    let link = tmp_dir.path().join("link");
    File::create(&original)?;
    let accesses = track_fn!(tmp_dir.path().to_str().unwrap().to_owned(), |dir: String| {
        let dir = std::path::Path::new(&dir);
        fs::hard_link(dir.join("original"), dir.join("link")).unwrap();
    })
    .await?;
    assert_contains(&accesses, link.as_path(), created);

    Ok(())
}

/// Creating a symlink on Windows needs a privilege that test runners lack.
#[cfg(unix)]
#[test(tokio::test)]
async fn symlink_destination() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let link = tmp_dir.path().join("link");
    let accesses = track_fn!(link.to_str().unwrap().to_owned(), |link: String| {
        std::os::unix::fs::symlink("target", link).unwrap();
    })
    .await?;
    assert_contains(&accesses, link.as_path(), created);
    // The target is stored as text, not accessed.
    assert!(
        !accesses.iter().any(|access| {
            access.paths().any(|path| {
                path.strip_path_prefix(tmp_dir.path().join("target"), |stripped| {
                    stripped.is_ok_and(|rest| rest.as_os_str().is_empty())
                })
            })
        }),
        "the symlink target is not an access: {:?}",
        accesses.iter().collect::<Vec<_>>()
    );

    Ok(())
}

#[test(tokio::test)]
async fn metadata() -> anyhow::Result<()> {
    let tmp_dir = tempfile::tempdir()?;
    let tmp_path = tmp_dir.path().join("hello");
    File::create(&tmp_path)?;
    let tmp_path_str = tmp_path.to_str().unwrap().to_owned();

    let accesses = track_fn!(tmp_path_str, |tmp_path_str: String| {
        let _ = fs::metadata(tmp_path_str);
    })
    .await?;
    assert_contains(&accesses, tmp_path.as_path(), PathAccess::Read);

    Ok(())
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test(tokio::test)]
async fn syscall_during_dlopen() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let fifo_path = tmp_dir.path().join("fifo").to_str().unwrap().to_owned();
    nix::unistd::mkfifo(fifo_path.as_str(), nix::sys::stat::Mode::S_IRWXU).unwrap();

    track_fn!(fifo_path, |fifo_path: String| {
        // The interposer used to resolve libc's syscall with dlsym, which waits for dlopen,
        // so a second thread calling syscall waited for the first through futex, re-entering
        // the interposer until its stack overflowed.
        fn call_syscall_during_dlopen(fifo_path: &str) {
            // This waits for dlopen to open the FIFO, which it reads until writers close.
            let _writer = OpenOptions::new().write(true).open(fifo_path).unwrap();
            // SAFETY: getpid takes no arguments.
            unsafe { libc::syscall(libc::SYS_getpid) };
        }
        // Opening files and spawning threads can also wait for dlopen, so do both first.
        File::open("/").unwrap();
        let path = fifo_path.clone();
        std::thread::spawn(move || {
            let c = std::ffi::CString::new(path.as_str()).unwrap();
            // SAFETY: dlopen fails on the FIFO's empty contents.
            std::thread::spawn(move || unsafe {
                libc::dlopen(c.as_ptr(), libc::RTLD_NOW);
            });
            call_syscall_during_dlopen(&path);
        });
        call_syscall_during_dlopen(&fifo_path);
    })
    .await
    .unwrap();
}

#[test(tokio::test)]
async fn readdir() -> anyhow::Result<()> {
    let tmpdir = tempfile::tempdir()?;
    let tmpdir_path = std::fs::canonicalize(tmpdir.path())?;
    // Reading a non-existent directory results in different tracked accesses on different platforms:
    // - Windows: READ, because the NT APIs open the directory as handle just like files (NtCreateFile/NtOpenFile),
    //   and if that fails, not read dir call (NtQueryDirectoryFile/NtQueryDirectoryFileEx) is made.
    // - macOS/Linux:
    //   - opendir results in a read_dir access. This call is directly made without trying to open the directory as a fd first.
    //   - open + fopendir results in READ access, because open would fail with ENOENT, and fopendir is not called.
    //
    // This difference is acceptable because both will result in a "not found" fingerprint in vite-task.
    // To keep the test consistent across platforms, we create the directory first.
    std::fs::create_dir(tmpdir.path().join("hello_dir"))?;

    let accesses = track_fn!(tmpdir_path.to_str().unwrap().to_owned(), |tmpdir_path: String| {
        std::env::set_current_dir(tmpdir_path).unwrap();
        // Consume at least one entry so that getdents64 fires on seccomp targets.
        let _ = std::fs::read_dir("hello_dir").unwrap().next();
    })
    .await?;
    assert_contains(&accesses, tmpdir_path.join("hello_dir").as_path(), PathAccess::ReadDir);

    Ok(())
}

#[test(tokio::test)]
async fn read_in_subprocess() -> anyhow::Result<()> {
    let accesses = track_fn!((), |(): ()| {
        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.arg("/c").arg("type hello");
            command
        } else {
            let mut command = std::process::Command::new("/bin/sh");
            command.arg("-c").arg("cat hello");
            command
        };
        command
            .stdout(Stdio::null())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
            .wait()
            .unwrap();
    })
    .await?;
    assert_contains(&accesses, current_dir().unwrap().join("hello").as_path(), PathAccess::Read);

    Ok(())
}

#[test(tokio::test)]
async fn read_program() -> anyhow::Result<()> {
    let accesses = track_fn!((), |(): ()| {
        let _ = std::process::Command::new("./not_exist.exe").spawn();
    })
    .await?;
    assert_contains(
        &accesses,
        current_dir().unwrap().join("not_exist.exe").as_path(),
        PathAccess::Read,
    );

    Ok(())
}
