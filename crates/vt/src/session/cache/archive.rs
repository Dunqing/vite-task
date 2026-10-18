//! Output archive creation and extraction using tar + zstd compression.

use std::{
    fs::File,
    io::{self, Read},
};

use vt_path::{AbsolutePath, RelativePathBuf};

/// Create a tar.zst archive from workspace-relative output paths.
///
/// Of `output_files`, regular files and symlinks are archived (a symlink as
/// a symlink, not its target); directories and paths that no longer exist
/// are skipped (the task may delete temporary files during execution).
/// `folders` are archived as directory entries, so a cache hit restores them
/// even when empty.
///
/// # Errors
///
/// Returns an error if creating the archive file or adding entries fails.
pub fn create_output_archive(
    workspace_root: &AbsolutePath,
    output_files: &[RelativePathBuf],
    folders: &[RelativePathBuf],
    archive_path: &AbsolutePath,
) -> anyhow::Result<()> {
    let file = File::create(archive_path.as_path())?;
    let encoder = zstd::Encoder::new(file, 0)?.auto_finish();
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);

    for rel_path in folders {
        let abs_path = workspace_root.join(rel_path);
        match std::fs::symlink_metadata(abs_path.as_path()) {
            Ok(metadata) if metadata.is_dir() => {
                builder.append_dir(rel_path.as_str(), abs_path.as_path())?;
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }

    for rel_path in output_files {
        let abs_path = workspace_root.join(rel_path);
        // Skip files that no longer exist (task may delete temp files between
        // glob walk and archiving). Any other error is propagated.
        let metadata = match std::fs::symlink_metadata(abs_path.as_path()) {
            Ok(m) => m,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        if metadata.is_file() {
            let mut file = File::open(abs_path.as_path())?;
            let mut header = tar::Header::new_gnu();
            header.set_metadata(&metadata);
            header.set_cksum();
            builder.append_data(&mut header, rel_path.as_str(), &mut file)?;
        } else if metadata.is_symlink() {
            builder.append_path_with_name(abs_path.as_path(), rel_path.as_str())?;
        }
    }

    builder.finish()?;
    Ok(())
}

/// Extract a tar.zst archive, restoring files relative to workspace root.
///
/// Parent directories are created automatically. Existing files are overwritten.
///
/// # Errors
///
/// Returns an error if opening the archive or extracting entries fails.
pub fn extract_output_archive(
    workspace_root: &AbsolutePath,
    archive_path: &AbsolutePath,
) -> anyhow::Result<()> {
    let file = File::open(archive_path.as_path())?;
    let decoder = zstd::Decoder::new(file)?;
    let mut archive = tar::Archive::new(decoder);

    archive.unpack(workspace_root.as_path())?;
    Ok(())
}

/// Read a tar.zst archive from `reader` to the end without writing any files,
/// to check that it decodes.
///
/// # Errors
///
/// Returns an error if reading fails or the archive doesn't decode.
pub fn check_output_archive(reader: impl Read) -> io::Result<()> {
    let mut archive = tar::Archive::new(zstd::Decoder::new(reader)?);
    for entry in archive.entries()? {
        io::copy(&mut entry?, &mut io::sink())?;
    }
    // Decode the rest of the stream too, so a truncated zstd frame after the
    // end of the tar data is caught.
    io::copy(&mut archive.into_inner(), &mut io::sink())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use vt_path::AbsolutePathBuf;

    use super::*;

    fn temp_workspace() -> (tempfile::TempDir, AbsolutePathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = AbsolutePathBuf::new(tmp.path().to_path_buf()).unwrap();
        (tmp, dir)
    }

    fn rel(path: &str) -> RelativePathBuf {
        RelativePathBuf::new(path).unwrap()
    }

    #[test]
    fn check_accepts_only_complete_archives() {
        let (_tmp, dir) = temp_workspace();
        let output = rel("output.txt");
        std::fs::write(dir.join(&output).as_path(), "built\n".repeat(1000)).unwrap();
        let archive_path = dir.join("output.tar.zst");
        create_output_archive(&dir, &[output], &[], &archive_path).unwrap();
        let archive = std::fs::read(archive_path.as_path()).unwrap();
        check_output_archive(archive.as_slice()).unwrap();

        for corrupt in [&archive[..archive.len() - 1], b"corrupt"] {
            assert!(check_output_archive(corrupt).is_err());
        }
    }

    /// Folders are restored even when empty, and a directory among the
    /// output files is not an entry.
    #[test]
    fn restores_folders_and_skips_directories_among_files() {
        let (_tmp, dir) = temp_workspace();
        std::fs::create_dir_all(dir.join("dist/empty")).unwrap();
        std::fs::create_dir_all(dir.join("existing")).unwrap();
        std::fs::write(dir.join("dist/main.js"), "built").unwrap();
        let archive_path = dir.join("output.tar.zst");
        create_output_archive(
            &dir,
            &[rel("dist/main.js"), rel("existing"), rel("gone.txt")],
            &[rel("dist/empty"), rel("dist")],
            &archive_path,
        )
        .unwrap();

        let (_restored_tmp, restored) = temp_workspace();
        extract_output_archive(&restored, &archive_path).unwrap();
        assert!(restored.join("dist/empty").as_path().is_dir());
        assert_eq!(std::fs::read_to_string(restored.join("dist/main.js")).unwrap(), "built");
        assert!(!restored.join("existing").as_path().exists());
        assert!(!restored.join("gone.txt").as_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn restores_symlinks_as_symlinks() {
        let (_tmp, dir) = temp_workspace();
        std::fs::write(dir.join("real.txt"), "content").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();
        let archive_path = dir.join("output.tar.zst");
        create_output_archive(&dir, &[rel("link.txt")], &[], &archive_path).unwrap();

        let (_restored_tmp, restored) = temp_workspace();
        extract_output_archive(&restored, &archive_path).unwrap();
        assert_eq!(
            std::fs::read_link(restored.join("link.txt")).unwrap().to_str(),
            Some("real.txt")
        );
        // The target itself was not an output.
        assert!(!restored.join("real.txt").as_path().exists());
    }
}
