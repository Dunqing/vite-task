//! Normalize raw fspy path accesses into workspace-relative, filtered form.
//!
//! User-configured negative globs are NOT applied here. They are applied later,
//! separately for reads (input config) and writes (output config), since those
//! two configs are independent.
#![cfg(fspy)]

use fspy::{IpcPath, PathAccess, PathAccessIterable};
use vt_path::{AbsolutePath, RelativePathBuf};

use super::fingerprint::{PathRead, PathWrite};
use crate::collections::HashMap;

/// Tracked file accesses from fspy, normalized to workspace-relative paths.
#[derive(Default, Debug)]
pub struct TrackedPathAccesses {
    /// Tracked path reads, with whether the path's entries were listed.
    pub path_reads: HashMap<RelativePathBuf, PathRead>,

    /// Tracked path writes, with whether the task created the path: one of
    /// its writes found the path missing.
    pub path_writes: HashMap<RelativePathBuf, PathWrite>,
}

impl TrackedPathAccesses {
    /// Build from fspy's raw iterable by stripping the workspace prefix and
    /// normalizing `..` components. `.git/*` paths and pnpm's workspace state
    /// files are skipped. User-configured negatives are applied by the caller
    /// (see module docs).
    pub fn from_raw(raw: &PathAccessIterable, workspace_root: &AbsolutePath) -> Self {
        Self::from_accesses(raw.iter(), workspace_root)
    }

    fn from_accesses<'a>(
        raw: impl Iterator<Item = PathAccess<'a>>,
        workspace_root: &AbsolutePath,
    ) -> Self {
        // Strip workspace root and clean `..` components in one pass.
        // fspy may report paths like `packages/sub-pkg/../shared/dist/output.js`.
        let relative_path = |path: &IpcPath| {
            path.strip_path_prefix(workspace_root, |strip_result| {
                normalize_tracked_workspace_path(strip_result.ok()?)
            })
        };

        let mut accesses = Self::default();
        for access in raw {
            match access {
                PathAccess::Read(path) => {
                    if let Some(path) = relative_path(path) {
                        accesses.add_read(path);
                    }
                }
                PathAccess::Write { path, created } => {
                    if let Some(path) = relative_path(path) {
                        accesses.add_write(path, created);
                    }
                }
                PathAccess::ReadWrite { path, created } => {
                    if let Some(path) = relative_path(path) {
                        accesses.add_read(path.clone());
                        accesses.add_write(path, created);
                    }
                }
                PathAccess::ReadDir(path) => {
                    if let Some(path) = relative_path(path) {
                        accesses.add_read(path).read_dir_entries = true;
                    }
                }
            }
        }
        accesses
    }

    fn add_read(&mut self, path: RelativePathBuf) -> &mut PathRead {
        self.path_reads.entry(path).or_insert(PathRead { read_dir_entries: false })
    }

    /// A path is created when any of its writes found it missing: a task
    /// that creates a file and then writes it again still created it, and
    /// one that removes a file and writes it anew did too.
    fn add_write(&mut self, path: RelativePathBuf, created: bool) {
        self.path_writes.entry(path).or_default().created |= created;
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "fspy strip_path_prefix exposes std::path::Path; convert to RelativePathBuf immediately"
)]
fn normalize_tracked_workspace_path(stripped_path: &std::path::Path) -> Option<RelativePathBuf> {
    // On Windows, paths are possible to be still absolute after stripping the workspace root.
    // For example: c:\workspace\subdir\c:\workspace\subdir
    // Just ignore those accesses.
    let relative = RelativePathBuf::new(stripped_path).ok()?;

    // Clean `..` components — fspy may report paths like
    // `packages/sub-pkg/../shared/dist/output.js`. Normalize them for
    // consistent behavior across platforms and clean user-facing messages.
    let relative = relative.clean().ok()?;

    // Skip .git directory accesses (workaround for tools like oxlint)
    if relative.as_path().strip_prefix(".git").is_ok() {
        return None;
    }

    if is_pnpm_workspace_state(&relative) {
        return None;
    }

    Some(relative)
}

/// Whether `relative` is a `node_modules/.pnpm-workspace-state-*.json` file,
/// pnpm's record of its dependency check before `pnpm run` and `pnpm exec`.
/// pnpm rewrites it with a new timestamp whenever it rechecks dependencies,
/// and it holds absolute project paths. Whether the check passes doesn't
/// change what a cached task outputs.
fn is_pnpm_workspace_state(relative: &RelativePathBuf) -> bool {
    let path = relative.as_str();
    let (dir, file_name) = path.rsplit_once('/').unwrap_or(("", path));
    (dir == "node_modules" || dir.ends_with("/node_modules"))
        && file_name
            .strip_prefix(".pnpm-workspace-state-")
            .is_some_and(|rest| rest.strip_suffix(".json").is_some())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn pnpm_workspace_state_is_ignored() {
        #[expect(
            clippy::disallowed_types,
            reason = "normalize_tracked_workspace_path requires std::path::Path for fspy strip_path_prefix output"
        )]
        let normalize = |path: &str| normalize_tracked_workspace_path(std::path::Path::new(path));
        assert!(normalize("node_modules/.pnpm-workspace-state-v1.json").is_none());
        assert!(normalize("node_modules/.pnpm-workspace-state-v2.json").is_none());
        assert!(normalize("packages/a/node_modules/.pnpm-workspace-state-v1.json").is_none());
        assert!(normalize("node_modules/.modules.yaml").is_some());
        assert!(normalize(".pnpm-workspace-state-v1.json").is_some());
        assert!(normalize("node_modules/a/.pnpm-workspace-state-v1.json").is_some());
    }

    #[cfg(windows)]
    #[test]
    fn malformed_windows_drive_path_after_workspace_strip_is_ignored() {
        #[expect(
            clippy::disallowed_types,
            reason = "normalize_tracked_workspace_path requires std::path::Path for fspy strip_path_prefix output"
        )]
        let relative_path = normalize_tracked_workspace_path(std::path::Path::new(r"foo\C:\bar"));
        assert!(relative_path.is_none());
    }

    /// Builds a record for a path.
    type Record = fn(&IpcPath) -> PathAccess<'_>;

    /// The records of one run, as `(path, access)` with the path relative to
    /// the workspace root, turned into the accesses fspy would report.
    fn track(records: &[(&str, Record)]) -> TrackedPathAccesses {
        let workspace_root = vt_path::current_dir().unwrap();
        let paths: Vec<OsString> = records
            .iter()
            .map(|(path, _)| workspace_root.join(path).as_path().as_os_str().to_owned())
            .collect();
        #[cfg(windows)]
        let wide: Vec<Vec<u16>> = paths
            .iter()
            .map(|path| std::os::windows::ffi::OsStrExt::encode_wide(path.as_os_str()).collect())
            .collect();
        let accesses = records.iter().enumerate().map(|(index, (_, access))| {
            #[cfg(unix)]
            let path = <&IpcPath>::from(&paths[index]);
            #[cfg(windows)]
            let path = IpcPath::from_wide(&wide[index]);
            access(path)
        });
        TrackedPathAccesses::from_accesses(accesses, &workspace_root)
    }

    fn read(path: &IpcPath) -> PathAccess<'_> {
        PathAccess::Read(path)
    }

    fn listed(path: &IpcPath) -> PathAccess<'_> {
        PathAccess::ReadDir(path)
    }

    fn created(path: &IpcPath) -> PathAccess<'_> {
        PathAccess::Write { path, created: true }
    }

    fn written(path: &IpcPath) -> PathAccess<'_> {
        PathAccess::Write { path, created: false }
    }

    fn read_written(path: &IpcPath) -> PathAccess<'_> {
        PathAccess::ReadWrite { path, created: false }
    }

    fn rel(path: &str) -> RelativePathBuf {
        RelativePathBuf::new(path).unwrap()
    }

    #[test]
    fn a_path_is_created_when_any_write_found_it_missing() {
        let tracked = track(&[
            ("dist/a.js", created),
            ("dist/a.js", written),
            ("dist/b.js", written),
            ("dist/b.js", created),
            ("package.json", read_written),
        ]);
        assert_eq!(tracked.path_writes[&rel("dist/a.js")], PathWrite { created: true });
        assert_eq!(tracked.path_writes[&rel("dist/b.js")], PathWrite { created: true });
        assert_eq!(tracked.path_writes[&rel("package.json")], PathWrite { created: false });
        assert!(tracked.path_reads.contains_key(&rel("package.json")));
        assert!(!tracked.path_reads.contains_key(&rel("dist/a.js")));
    }

    #[test]
    fn a_read_and_written_path_is_both_an_input_and_an_output() {
        let tracked = track(&[("src/index.ts", read), ("src/index.ts", written), ("dist", listed)]);
        assert!(tracked.path_reads.contains_key(&rel("src/index.ts")));
        assert!(tracked.path_writes.contains_key(&rel("src/index.ts")));
        assert!(tracked.path_reads[&rel("dist")].read_dir_entries);
        assert!(!tracked.path_writes.contains_key(&rel("dist")));
    }
}
