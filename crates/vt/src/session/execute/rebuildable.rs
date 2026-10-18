//! What a cache hit can rebuild: the outputs it puts back exactly as the
//! task left them.
//!
//! A file can be rebuilt when the task created it and the hit restores it. A
//! folder can be rebuilt when the task created it or wrote into it, and
//! everything in it can be rebuilt. A path excluded from outputs that the
//! task created counts as rebuilt: the hit isn't asked to restore it.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use rustc_hash::FxHashSet;
use vt_path::{AbsolutePath, RelativePath, RelativePathBuf};
use vt_str::Str;

use super::fingerprint::{DirEntryKind, PathFingerprint, PathRead, PathWrite, fingerprint_path};
use crate::collections::HashMap;

/// A folder the task created or wrote into, as the run left it.
#[derive(Debug)]
pub struct WrittenFolder {
    pub created: bool,
    pub entries: BTreeMap<Str, DirEntryKind>,
    /// The entries a hit can rebuild.
    pub rebuildable_entries: BTreeSet<Str>,
}

impl WrittenFolder {
    /// Everything in the folder can be rebuilt.
    #[must_use]
    pub fn is_rebuildable(&self) -> bool {
        self.rebuildable_entries.len() == self.entries.len()
    }
}

/// What a hit can rebuild after a run, decided over the folders the task
/// created or wrote into, deepest first.
#[derive(Debug, Default)]
pub struct Rebuildable {
    /// Files and symlinks the task created that the hit restores or that
    /// are excluded from outputs.
    files: FxHashSet<RelativePathBuf>,
    /// Folders the task created or wrote into, other than excluded ones.
    folders: HashMap<RelativePathBuf, WrittenFolder>,
    /// Folders the task created or wrote into that are excluded from outputs.
    excluded_folders: FxHashSet<RelativePathBuf>,
}

impl Rebuildable {
    /// `writes` are every path the task wrote, `archived` the paths the
    /// output archive restores, and `is_excluded` whether a path is excluded
    /// from outputs.
    ///
    /// # Errors
    ///
    /// Returns an error if listing a folder fails.
    pub fn new(
        base_dir: &AbsolutePath,
        writes: &HashMap<RelativePathBuf, PathWrite>,
        archived: &FxHashSet<RelativePathBuf>,
        is_excluded: impl Fn(&RelativePathBuf) -> bool,
    ) -> anyhow::Result<Self> {
        let files = writes
            .iter()
            .filter(|(path, write)| {
                write.created && (archived.contains(*path) || is_excluded(path))
            })
            .map(|(path, _)| path.clone())
            .collect();
        let mut rebuildable = Self { files, ..Self::default() };

        // The folders the task created and every folder a write is in. A
        // path is longer than its ancestors, so the longest come first and
        // each folder is decided after the folders in it.
        let mut folders: FxHashSet<&RelativePath> = writes
            .iter()
            .filter(|(_, write)| write.created)
            .map(|(path, _)| path.as_relative_path())
            .collect();
        for path in writes.keys() {
            folders.extend(std::iter::successors(path.parent(), |folder| folder.parent()));
        }
        let mut folders: Vec<&RelativePath> = folders.into_iter().collect();
        folders.sort_unstable_by_key(|folder| Reverse(folder.as_str().len()));

        for folder in folders {
            let folder = folder.to_relative_path_buf();
            if is_excluded(&folder) {
                rebuildable.excluded_folders.insert(folder);
                continue;
            }
            let full_path = Arc::<AbsolutePath>::from(base_dir.join(&folder));
            // The folder itself, not a symlink to one.
            if !std::fs::symlink_metadata(full_path.as_path()).is_ok_and(|meta| meta.is_dir()) {
                continue;
            }
            let PathFingerprint::Folder(Some(entries)) =
                fingerprint_path(&full_path, PathRead { read_dir_entries: true })?
            else {
                continue;
            };
            let rebuildable_entries = entries
                .iter()
                .filter(|(name, kind)| {
                    entry_path(&folder, name).is_some_and(|path| rebuildable.entry(&path, kind))
                })
                .map(|(name, _)| name.clone())
                .collect();
            let created = writes.get(&folder).is_some_and(|write| write.created);
            rebuildable
                .folders
                .insert(folder, WrittenFolder { created, entries, rebuildable_entries });
        }
        Ok(rebuildable)
    }

    fn entry(&self, path: &RelativePathBuf, kind: &DirEntryKind) -> bool {
        match kind {
            DirEntryKind::Dir => {
                self.excluded_folders.contains(path)
                    || self.folders.get(path).is_some_and(WrittenFolder::is_rebuildable)
            }
            DirEntryKind::File | DirEntryKind::Symlink => self.files.contains(path),
        }
    }

    /// Whether a hit can rebuild `path`, a folder if `is_dir`.
    #[must_use]
    pub fn contains(&self, path: &RelativePathBuf, is_dir: bool) -> bool {
        self.entry(path, if is_dir { &DirEntryKind::Dir } else { &DirEntryKind::File })
    }

    /// The folder the task created or wrote into at `path`, if any.
    #[must_use]
    pub fn folder(&self, path: &RelativePathBuf) -> Option<&WrittenFolder> {
        self.folders.get(path)
    }

    /// The folders a hit restores and must find nothing extra in: those the
    /// task created, and those it wrote into that can be rebuilt.
    pub fn output_folders(&self) -> impl Iterator<Item = (&RelativePathBuf, &WrittenFolder)> {
        self.folders.iter().filter(|(_, folder)| folder.created || folder.is_rebuildable())
    }
}

/// The path of entry `name` of `folder`, or `None` if the name isn't a
/// portable path component.
pub fn entry_path(folder: &RelativePath, name: &str) -> Option<RelativePathBuf> {
    Some(folder.join(RelativePathBuf::new(name).ok()?))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use vt_path::AbsolutePathBuf;

    use super::*;

    fn rel(path: &str) -> RelativePathBuf {
        RelativePathBuf::new(path).unwrap()
    }

    /// The verdicts after a run that left `files` in a temporary workspace
    /// (a name ending in `/` is an empty folder) and wrote `writes`
    /// (`(path, created)`); every write is archived except under `excluded`.
    fn rebuildable(files: &[&str], writes: &[(&str, bool)], excluded: &str) -> Rebuildable {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = AbsolutePathBuf::new(tmp.path().to_path_buf()).unwrap();
        for name in files {
            let path = root.join(name.trim_end_matches('/'));
            if name.ends_with('/') {
                fs::create_dir_all(&path).unwrap();
            } else {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, "").unwrap();
            }
        }
        let writes: HashMap<RelativePathBuf, PathWrite> = writes
            .iter()
            .map(|(path, created)| (rel(path), PathWrite { created: *created }))
            .collect();
        let archived = writes.keys().cloned().collect();
        Rebuildable::new(&root, &writes, &archived, |path| {
            !excluded.is_empty() && path.strip_prefix(rel(excluded)).is_some()
        })
        .unwrap()
    }

    fn output_folders(rebuildable: &Rebuildable) -> Vec<&str> {
        let mut folders: Vec<&str> =
            rebuildable.output_folders().map(|(path, _)| path.as_str()).collect();
        folders.sort_unstable();
        folders
    }

    #[test]
    fn created_folder_holding_only_created_files_can_be_rebuilt() {
        let r = rebuildable(
            &["pkg/package.json", "pkg/dist/assets/a.js", "pkg/dist/index.html"],
            &[
                ("pkg/dist", true),
                ("pkg/dist/assets", true),
                ("pkg/dist/assets/a.js", true),
                ("pkg/dist/index.html", true),
            ],
            "",
        );
        assert!(r.contains(&rel("pkg/dist"), true));
        assert!(r.contains(&rel("pkg/dist/index.html"), false));
        assert!(!r.contains(&rel("pkg"), true));
        assert_eq!(output_folders(&r), ["pkg/dist", "pkg/dist/assets"]);
        assert_eq!(r.folder(&rel("pkg")).unwrap().rebuildable_entries, [Str::from("dist")].into());
    }

    #[test]
    fn folder_written_into_holding_only_created_files_can_be_rebuilt() {
        // tsdown empties an existing `dist` and fills it again.
        let r = rebuildable(&["package.json", "dist/index.mjs"], &[("dist/index.mjs", true)], "");
        assert!(r.contains(&rel("dist"), true));
        assert_eq!(output_folders(&r), ["dist"]);
    }

    #[test]
    fn folder_holding_a_modified_file_cannot_be_rebuilt() {
        // tsc rewrites `dist/index.js` in place: it existed before the run.
        let r = rebuildable(&["package.json", "dist/index.js"], &[("dist/index.js", false)], "");
        assert!(!r.contains(&rel("dist"), true));
        assert!(!r.contains(&rel("dist/index.js"), false));
        assert!(output_folders(&r).is_empty());
    }

    #[test]
    fn created_folder_holding_files_renamed_into_it_cannot_be_rebuilt() {
        // `rename(dist.tmp, dist)`: the files were written as `dist.tmp/*`.
        let r = rebuildable(
            &["package.json", "dist/a.js"],
            &[("dist.tmp", true), ("dist.tmp/a.js", true), ("dist", true)],
            "",
        );
        assert!(!r.contains(&rel("dist"), true));
        assert!(r.folder(&rel("dist")).unwrap().rebuildable_entries.is_empty());
        // Still an output folder: a hit can't empty it.
        assert_eq!(output_folders(&r), ["dist"]);
    }

    #[test]
    fn excluded_outputs_count_as_rebuilt() {
        // Vite renames its dependency cache into place and excludes it.
        let r = rebuildable(
            &["node_modules/.vite/deps/react.js", "node_modules/react/index.js"],
            &[
                ("node_modules/.vite", true),
                ("node_modules/.vite/deps_temp/react.js", true),
                ("node_modules/.vite/deps", true),
            ],
            "node_modules/.vite/deps",
        );
        assert!(r.contains(&rel("node_modules/.vite"), true));
        assert!(r.contains(&rel("node_modules/.vite/deps"), true));
        assert!(!r.contains(&rel("node_modules"), true));
        assert_eq!(output_folders(&r), ["node_modules/.vite"]);
    }

    #[test]
    fn empty_created_folder_can_be_rebuilt() {
        let r =
            rebuildable(&["package.json", "out/empty/"], &[("out", true), ("out/empty", true)], "");
        assert!(r.contains(&rel("out/empty"), true));
        assert!(r.contains(&rel("out"), true));
    }
}
