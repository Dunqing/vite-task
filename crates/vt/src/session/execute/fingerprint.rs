//! Post-run fingerprinting for execution caching.
//!
//! A run's inputs are fingerprinted after it, as the task left them: a file
//! by its content, a folder by being one or, when the task listed it, by its
//! entries. What a hit can rebuild may be missing (see
//! [`super::rebuildable`]), and so may an entry of a listed folder that a hit
//! can rebuild. What a hit can't rebuild in a folder the task created, such as
//! the contents of a folder renamed into place, must be as the task left it,
//! whether or not the task read it. A hit doesn't delete, so a folder the task
//! didn't list may hold files it didn't leave there.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::File,
    io::{self, BufRead},
    sync::Arc,
};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use vt_casefold::EnvName;
use vt_path::{AbsolutePath, RelativePathBuf};
use vt_plan::cache_metadata::EnvValueHash;
use vt_str::Str;
use wincode::{SchemaRead, SchemaWrite};

use super::{
    rebuildable::{Rebuildable, entry_path},
    redact::pnpm_store_bin_dir,
};
use crate::{
    collections::HashMap,
    session::cache::{EnvMismatch, InputChangeKind},
};

#[derive(
    SchemaWrite, SchemaRead, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum TrackedEnvQuery {
    Glob(Str),
    Prefix(Str),
}

/// Path read access info
#[derive(Debug, Clone, Copy)]
pub struct PathRead {
    pub read_dir_entries: bool,
}

/// Path write access info
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathWrite {
    /// A write found the path missing: the task created it.
    pub created: bool,
}

/// Post-run fingerprint capturing file state after execution.
/// Used to validate whether cached outputs are still valid.
#[derive(SchemaWrite, SchemaRead, Debug, Default, Serialize)]
pub struct PostRunFingerprint {
    /// Paths inferred from fspy during execution with their fingerprints:
    /// the paths the task read when `input_config.includes_auto` is true,
    /// and the output folders (see the module docs).
    pub inferred_inputs: HashMap<RelativePathBuf, InputFingerprint>,

    /// Env vars observed via runner-aware IPC `getEnv` with `tracked: true`.
    /// Key is the env name; value is the env value hash at execution time, or
    /// `None` if unset. Validated at cache lookup against the same plan env
    /// context that served the original request.
    pub tracked_envs: BTreeMap<Str, Option<EnvValueHash>>,

    /// Bulk env queries (`getEnvs`) made with `tracked: true`.
    /// Outer key is the query, inner map is the match-set at execution time
    /// (name -> value hash). Validated at cache lookup by re-matching against
    /// the current env context and comparing the resulting set.
    ///
    /// Non-UTF-8 env names are never matched, saved, or treated as errors:
    /// they are not returned to the client, so their existence cannot affect
    /// task behavior. Values are stricter. A matched env must have a UTF-8
    /// value; the JS client errors when querying a matched non-UTF-8 value,
    /// and cache-hit validation treats a currently matched non-UTF-8 value as
    /// a changed mismatch so stale cached output is not replayed.
    pub tracked_env_queries: BTreeMap<TrackedEnvQuery, BTreeMap<Str, EnvValueHash>>,
}

/// A mismatch between the stored post-run fingerprint and the current state.
#[derive(Debug, Clone)]
pub enum PostRunMismatch {
    /// An inferred input file or directory changed.
    Input { kind: InputChangeKind, path: RelativePathBuf },
    /// A tool-tracked env var changed value, appeared, or disappeared.
    TrackedEnv(EnvMismatch),
    /// A tool-tracked bulk env query's match-set changed between runs. Carries
    /// the first differing entry in env-name order.
    TrackedEnvQuery { query: TrackedEnvQuery, mismatch: EnvMismatch },
}

/// An inferred input: a path as the task left it.
#[derive(SchemaWrite, SchemaRead, PartialEq, Eq, Debug, Clone, Serialize, Deserialize)]
pub struct InputFingerprint {
    pub state: PathFingerprint,
    /// A hit can rebuild the path, so it may also be missing.
    pub rebuildable: bool,
    /// The entries of a listed folder that a hit can rebuild, so they may
    /// be missing.
    pub rebuildable_entries: BTreeSet<Str>,
}

impl InputFingerprint {
    /// The first difference between `current` and this fingerprint, as the
    /// kind of change and the folder entry it names, or `None` if `current`
    /// matches.
    fn mismatch<'a>(
        &'a self,
        current: &'a PathFingerprint,
    ) -> Option<(InputChangeKind, Option<&'a Str>)> {
        match (&self.state, current) {
            (_, PathFingerprint::NotFound) if self.rebuildable => None,
            (PathFingerprint::Folder(Some(left)), PathFingerprint::Folder(Some(now))) => {
                folder_mismatch(left, now, &self.rebuildable_entries)
            }
            (left, now) if left == now => None,
            (left, now) => Some((determine_change_kind(left, now), None)),
        }
    }
}

/// Fingerprint for a single path (file or directory)
#[derive(SchemaWrite, SchemaRead, PartialEq, Eq, Debug, Clone, Serialize, Deserialize)]
pub enum PathFingerprint {
    /// Path was not found when fingerprinting
    NotFound,
    /// File content hash using `xxHash3_64`
    FileContentHash(u64),
    /// Directory with optional entry listing.
    /// `Folder(None)` means the directory was opened but entries were not read
    /// (e.g., for `openat` calls).
    /// `Folder(Some(_))` contains the directory entries sorted by name.
    Folder(Option<BTreeMap<Str, DirEntryKind>>),
}

/// Kind of directory entry
#[derive(SchemaWrite, SchemaRead, PartialEq, Eq, Debug, Clone, Serialize, Deserialize)]
pub enum DirEntryKind {
    File,
    Dir,
    Symlink,
}

impl PostRunFingerprint {
    /// Creates a new fingerprint from path accesses after task execution.
    ///
    /// Negative glob filtering is done upstream (see
    /// [`super::tracked_accesses::TrackedPathAccesses::from_raw`]).
    /// Paths already present in `globbed_inputs` are skipped — they are
    /// already tracked by the prerun glob fingerprint, so a task that
    /// rewrites one misses on every run instead of hitting a stale entry.
    ///
    /// # Arguments
    /// * `inferred_path_reads` - Map of paths that were read during execution (from fspy)
    /// * `rebuildable` - What a hit can rebuild, from the paths the task wrote
    /// * `base_dir` - Workspace root for resolving relative paths
    /// * `globbed_inputs` - Prerun glob fingerprint; paths here are skipped
    /// * `tracked_envs` - Tool-requested env vars (name -> value hash), validated on lookup
    /// * `tracked_env_queries` - Tool-requested bulk env queries (query -> match-set hashes)
    #[tracing::instrument(level = "debug", skip_all, name = "create_post_run_fingerprint")]
    pub fn create(
        inferred_path_reads: &HashMap<RelativePathBuf, PathRead>,
        rebuildable: &Rebuildable,
        base_dir: &AbsolutePath,
        globbed_inputs: &BTreeMap<RelativePathBuf, u64>,
        tracked_envs: BTreeMap<Str, Option<EnvValueHash>>,
        tracked_env_queries: BTreeMap<TrackedEnvQuery, BTreeMap<Str, EnvValueHash>>,
    ) -> anyhow::Result<Self> {
        let mut inferred_inputs = unrebuildable_inputs(rebuildable, base_dir, globbed_inputs)?;
        let read_inputs = inferred_path_reads
            .par_iter()
            .filter(|(path, _)| {
                !globbed_inputs.contains_key(*path) && !inferred_inputs.contains_key(*path)
            })
            .map(|(relative_path, path_read)| {
                let full_path = Arc::<AbsolutePath>::from(base_dir.join(relative_path));
                let state = fingerprint_path(&full_path, *path_read)?;
                let is_dir = matches!(state, PathFingerprint::Folder(_));
                let rebuildable_entries = rebuildable
                    .folder(relative_path)
                    .filter(|_| path_read.read_dir_entries)
                    .map(|folder| folder.rebuildable_entries.clone())
                    .unwrap_or_default();
                let fingerprint = InputFingerprint {
                    state,
                    rebuildable: rebuildable.contains(relative_path, is_dir),
                    rebuildable_entries,
                };
                Ok((relative_path.clone(), fingerprint))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        inferred_inputs.extend(read_inputs);

        Ok(Self { inferred_inputs, tracked_envs, tracked_env_queries })
    }

    /// Validates the fingerprint against current filesystem state and the
    /// unfiltered env context used by runner-aware IPC. `unfiltered_envs` must
    /// be the same plan env context that served the original `getEnv` request,
    /// not the filtered env passed to the spawned process.
    ///
    /// Returns `Some(mismatch)` if anything changed, `None` if all valid.
    /// Returns an error if a tracked env is currently present but cannot be
    /// represented as UTF-8; treating that value as unset would make cache
    /// validation unsound.
    #[tracing::instrument(level = "debug", skip_all, name = "validate_post_run_fingerprint")]
    pub fn validate(
        &self,
        base_dir: &AbsolutePath,
        unfiltered_envs: &FxHashMap<EnvName<Arc<OsStr>>, Arc<OsStr>>,
    ) -> anyhow::Result<Option<PostRunMismatch>> {
        let input_mismatch = self.inferred_inputs.par_iter().find_map_any(
            |(input_relative_path, input_fingerprint)| {
                let input_full_path = Arc::<AbsolutePath>::from(base_dir.join(input_relative_path));
                let path_read = PathRead {
                    read_dir_entries: matches!(
                        input_fingerprint.state,
                        PathFingerprint::Folder(Some(_))
                    ),
                };
                let current_path_fingerprint = match fingerprint_path(&input_full_path, path_read) {
                    Ok(ok) => ok,
                    Err(err) => return Some(Err(err)),
                };
                let (kind, entry_name) = input_fingerprint.mismatch(&current_path_fingerprint)?;
                let path = if let Some(name) = entry_name {
                    // For folder changes, build `dir/entry` path
                    let entry = match RelativePathBuf::new(name.as_str()) {
                        Ok(p) => p,
                        Err(e) => return Some(Err(e.into())),
                    };
                    input_relative_path.as_relative_path().join(entry)
                } else {
                    input_relative_path.clone()
                };
                Some(Ok(PostRunMismatch::Input { kind, path }))
            },
        );
        if let Some(result) = input_mismatch {
            return result.map(Some);
        }

        for (name, stored_value) in &self.tracked_envs {
            let current_value = unfiltered_envs
                .get(EnvName::from_ref(OsStr::new(name.as_str())))
                .map(|value| {
                    let value_str = value.to_str().ok_or_else(|| {
                        anyhow::anyhow!("tracked env value for {name} is not valid UTF-8")
                    })?;
                    Ok::<_, anyhow::Error>(EnvValueHash::new(value_str))
                })
                .transpose()?;
            if let Some(mismatch) =
                EnvMismatch::compare(name, stored_value.as_ref(), current_value.as_ref())
            {
                return Ok(Some(PostRunMismatch::TrackedEnv(mismatch)));
            }
        }

        for (query, stored_matches) in &self.tracked_env_queries {
            let current_matches = match match_env_query(query, unfiltered_envs)? {
                EnvQueryValidation::Matches(matches) => matches,
                EnvQueryValidation::NonUtf8Value(mismatch) => {
                    return Ok(Some(PostRunMismatch::TrackedEnvQuery {
                        query: query.clone(),
                        mismatch,
                    }));
                }
            };
            if let Some(mismatch) = first_env_glob_mismatch(stored_matches, &current_matches) {
                return Ok(Some(PostRunMismatch::TrackedEnvQuery {
                    query: query.clone(),
                    mismatch,
                }));
            }
        }

        Ok(None)
    }
}

/// What a hit can't rebuild in a folder the task created, as the task left
/// it, read or not: the hit won't put it back, so it must already be there.
/// Symlinks are fingerprinted by what they point to but not descended into.
fn unrebuildable_inputs(
    rebuildable: &Rebuildable,
    base_dir: &AbsolutePath,
    globbed_inputs: &BTreeMap<RelativePathBuf, u64>,
) -> anyhow::Result<HashMap<RelativePathBuf, InputFingerprint>> {
    let mut inputs = HashMap::default();
    let mut unrebuildable: Vec<(RelativePathBuf, DirEntryKind)> = Vec::new();
    for (path, folder) in rebuildable.created_folders() {
        unrebuildable.extend(unrebuildable_entries(
            path,
            &folder.entries,
            &folder.rebuildable_entries,
        ));
    }
    while let Some((path, kind)) = unrebuildable.pop() {
        if inputs.contains_key(&path) || globbed_inputs.contains_key(&path) {
            continue;
        }
        let full_path = Arc::<AbsolutePath>::from(base_dir.join(&path));
        let is_dir = kind == DirEntryKind::Dir;
        let state = fingerprint_path(&full_path, PathRead { read_dir_entries: is_dir })?;
        let rebuildable_entries = rebuildable
            .folder(&path)
            .map(|folder| folder.rebuildable_entries.clone())
            .unwrap_or_default();
        if let PathFingerprint::Folder(Some(entries)) = &state {
            unrebuildable.extend(unrebuildable_entries(&path, entries, &rebuildable_entries));
        }
        inputs.insert(path, InputFingerprint { state, rebuildable: false, rebuildable_entries });
    }
    Ok(inputs)
}

/// The entries of the folder at `path` that a hit can't rebuild.
fn unrebuildable_entries<'a>(
    path: &'a RelativePathBuf,
    entries: &'a BTreeMap<Str, DirEntryKind>,
    rebuildable_entries: &'a BTreeSet<Str>,
) -> impl Iterator<Item = (RelativePathBuf, DirEntryKind)> + 'a {
    entries
        .iter()
        .filter(|(name, _)| !rebuildable_entries.contains(*name))
        .filter_map(|(name, kind)| Some((entry_path(path, name)?, kind.clone())))
}

/// Build the current match-set for `query` by enumerating the given env
/// snapshot and keeping matching UTF-8 names. If a matching env has a non-UTF-8
/// value, return a changed mismatch so the stale cache entry is not replayed.
fn match_env_query(
    query: &TrackedEnvQuery,
    envs: &FxHashMap<EnvName<Arc<OsStr>>, Arc<OsStr>>,
) -> anyhow::Result<EnvQueryValidation> {
    Ok(match query {
        TrackedEnvQuery::Glob(pattern) => {
            let glob = vt_glob::env::EnvGlob::new(pattern.as_str())?;
            collect_matching_envs(envs, |name| glob.is_match(name))
        }
        TrackedEnvQuery::Prefix(prefix) => {
            collect_matching_envs(envs, |name| EnvName::from_ref(name).starts_with(prefix.as_str()))
        }
    })
}

fn collect_matching_envs(
    envs: &FxHashMap<EnvName<Arc<OsStr>>, Arc<OsStr>>,
    is_match: impl Fn(&str) -> bool,
) -> EnvQueryValidation {
    let mut matches = BTreeMap::new();
    for (name, value) in envs {
        let Some(name_str) = name.inner().to_str() else {
            continue;
        };
        if !is_match(name_str) {
            continue;
        }
        let Some(value_str) = value.to_str() else {
            return EnvQueryValidation::NonUtf8Value(EnvMismatch::Changed {
                name: Str::from(name_str),
            });
        };
        matches.insert(Str::from(name_str), EnvValueHash::new(value_str));
    }
    EnvQueryValidation::Matches(matches)
}

enum EnvQueryValidation {
    Matches(BTreeMap<Str, EnvValueHash>),
    NonUtf8Value(EnvMismatch),
}

/// Find the first deterministic difference between stored and current env
/// glob match-sets.
fn first_env_glob_mismatch(
    stored: &BTreeMap<Str, EnvValueHash>,
    current: &BTreeMap<Str, EnvValueHash>,
) -> Option<EnvMismatch> {
    let mut stored_iter = stored.iter();
    let mut current_iter = current.iter();
    let mut s = stored_iter.next();
    let mut c = current_iter.next();

    loop {
        match (s, c) {
            (None, None) => return None,
            (Some((name, _)), None) => return Some(EnvMismatch::Removed { name: name.clone() }),
            (None, Some((name, _))) => return Some(EnvMismatch::Added { name: name.clone() }),
            (Some((sn, sv)), Some((cn, cv))) => match sn.cmp(cn) {
                std::cmp::Ordering::Equal => {
                    if sv != cv {
                        return Some(EnvMismatch::Changed { name: sn.clone() });
                    }
                    s = stored_iter.next();
                    c = current_iter.next();
                }
                std::cmp::Ordering::Less => return Some(EnvMismatch::Removed { name: sn.clone() }),
                std::cmp::Ordering::Greater => {
                    return Some(EnvMismatch::Added { name: cn.clone() });
                }
            },
        }
    }
}

/// Determine the kind of change between two differing path fingerprints
/// that aren't both folder listings. Caller guarantees `stored != current`.
const fn determine_change_kind(
    stored: &PathFingerprint,
    current: &PathFingerprint,
) -> InputChangeKind {
    match (stored, current) {
        (PathFingerprint::NotFound, _) => InputChangeKind::Added,
        (_, PathFingerprint::NotFound) => InputChangeKind::Removed,
        (PathFingerprint::FileContentHash(_), PathFingerprint::FileContentHash(_)) => {
            InputChangeKind::ContentModified
        }
        // Type changed (file ↔ folder)
        _ => InputChangeKind::Added,
    }
}

/// The first difference between a folder's entries as the task `left` them
/// and as they are `now`: an entry the task didn't leave there, which a hit
/// wouldn't delete, or else a missing entry that a hit can't rebuild.
fn folder_mismatch<'a>(
    left: &'a BTreeMap<Str, DirEntryKind>,
    now: &'a BTreeMap<Str, DirEntryKind>,
    rebuildable_entries: &BTreeSet<Str>,
) -> Option<(InputChangeKind, Option<&'a Str>)> {
    if let Some((name, _)) = now.iter().find(|(name, kind)| left.get(*name) != Some(*kind)) {
        return Some((InputChangeKind::Added, Some(name)));
    }
    left.keys()
        .find(|name| !now.contains_key(*name) && !rebuildable_entries.contains(*name))
        .map(|name| (InputChangeKind::Removed, Some(name)))
}

/// Check if a directory entry should be ignored in fingerprinting
const fn should_ignore_entry(name: &[u8]) -> bool {
    matches!(name, b"." | b".." | b".DS_Store")
}

/// Fingerprint a single path
pub fn fingerprint_path(
    path: &Arc<AbsolutePath>,
    path_read: PathRead,
) -> anyhow::Result<PathFingerprint> {
    let std_path = path.as_path();

    // A store package's own `node_modules` that holds only `.bin` is pnpm's
    // cold store showing through; see `redact::pnpm_store_bin_dir`.
    if pnpm_store_bin_dir::matches(path) && pnpm_store_bin_dir::holds_only_bin(path) {
        return Ok(PathFingerprint::NotFound);
    }

    let file = match File::open(std_path) {
        Ok(file) => file,
        Err(err) => {
            // On Windows, File::open fails specifically for directories with PermissionDenied
            #[cfg(windows)]
            {
                if err.kind() == io::ErrorKind::PermissionDenied {
                    // This might be a directory - try reading it as such
                    return process_directory(std_path, path_read);
                }
                // On Windows, paths with trailing backslash (from joining empty path)
                // fail with NotFound (error code 3). Try as directory in this case.
                if err.raw_os_error() == Some(3) && std_path.to_string_lossy().ends_with('\\') {
                    return process_directory(std_path, path_read);
                }
            }
            if err.kind() != io::ErrorKind::NotFound {
                tracing::trace!(
                    "Uncommon error when opening {} for fingerprinting: {}",
                    std_path.display(),
                    err
                );
            }
            // Treat all open errors as NotFound for fingerprinting purposes
            return Ok(PathFingerprint::NotFound);
        }
    };

    let mut reader = io::BufReader::new(file);
    if let Err(io_err) = reader.fill_buf() {
        if io_err.kind() != io::ErrorKind::IsADirectory {
            return Err(io_err.into());
        }
        // Is a directory on Unix - use the optimized nix implementation
        #[cfg(unix)]
        {
            return process_directory_unix(reader.get_ref(), path_read);
        }
        #[cfg(windows)]
        {
            return process_directory(std_path, path_read);
        }
    }
    Ok(PathFingerprint::FileContentHash(super::hash::hash_file_content(path, reader)?))
}

/// Process a directory on Windows using `std::fs::read_dir`
#[cfg(windows)]
#[expect(clippy::disallowed_types, reason = "Windows fallback uses std::path::Path directly")]
fn process_directory(
    path: &std::path::Path,
    path_read: PathRead,
) -> anyhow::Result<PathFingerprint> {
    if !path_read.read_dir_entries {
        return Ok(PathFingerprint::Folder(None));
    }

    let mut entries = BTreeMap::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_bytes = name.as_encoded_bytes();

        if should_ignore_entry(name_bytes) {
            continue;
        }

        let file_type = entry.file_type()?;
        let kind = if file_type.is_file() {
            DirEntryKind::File
        } else if file_type.is_dir() {
            DirEntryKind::Dir
        } else {
            DirEntryKind::Symlink
        };

        let name_str = name.to_string_lossy();
        entries.insert(Str::from(name_str.as_ref()), kind);
    }

    Ok(PathFingerprint::Folder(Some(entries)))
}

/// Process a directory on Unix using nix for efficiency
#[cfg(unix)]
fn process_directory_unix(file: &File, path_read: PathRead) -> anyhow::Result<PathFingerprint> {
    use std::os::fd::AsFd;

    if !path_read.read_dir_entries {
        return Ok(PathFingerprint::Folder(None));
    }

    let fd = file.as_fd();
    let mut dir = nix::dir::Dir::from_fd(fd.try_clone_to_owned()?)?;

    let mut entries = BTreeMap::new();
    for entry in dir.iter() {
        let entry = entry?;
        let name = entry.file_name().to_bytes();

        if should_ignore_entry(name) {
            continue;
        }

        let kind = match entry.file_type() {
            Some(nix::dir::Type::Directory) => DirEntryKind::Dir,
            Some(nix::dir::Type::Symlink) => DirEntryKind::Symlink,
            // Treat files and other types as files for fingerprinting
            _ => DirEntryKind::File,
        };

        #[expect(
            clippy::disallowed_types,
            reason = "from_utf8_lossy returns Cow referencing String"
        )]
        let name_str = String::from_utf8_lossy(name);
        entries.insert(Str::from(name_str.as_ref()), kind);
    }

    Ok(PathFingerprint::Folder(Some(entries)))
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};

    use super::*;

    #[cfg(unix)]
    fn non_utf8_os_string() -> OsString {
        use std::os::unix::ffi::OsStringExt;

        OsString::from_vec(vec![0xFF])
    }

    #[cfg(windows)]
    fn non_utf8_os_string() -> OsString {
        use std::os::windows::ffi::OsStringExt;

        OsString::from_wide(&[0xD800])
    }

    #[test]
    fn validate_errors_on_current_non_utf8_tracked_env_value() {
        let mut tracked_envs = BTreeMap::new();
        tracked_envs.insert(Str::from("PROBE_ENV"), None);
        let fingerprint = PostRunFingerprint { tracked_envs, ..PostRunFingerprint::default() };

        let mut unfiltered_envs = FxHashMap::default();
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(OsStr::new("PROBE_ENV"))),
            Arc::<OsStr>::from(non_utf8_os_string()),
        );

        let workspace_root = vt_path::current_dir().expect("cwd");
        let err = fingerprint
            .validate(&workspace_root, &unfiltered_envs)
            .expect_err("non-UTF-8 tracked env values must error");

        assert!(err.to_string().contains("tracked env value for PROBE_ENV is not valid UTF-8"));
    }

    #[test]
    fn validate_reports_current_non_utf8_tracked_env_glob_value_as_changed() {
        let mut tracked_env_queries = BTreeMap::new();
        tracked_env_queries.insert(TrackedEnvQuery::Glob(Str::from("PROBE_*")), BTreeMap::new());
        let fingerprint =
            PostRunFingerprint { tracked_env_queries, ..PostRunFingerprint::default() };

        let mut unfiltered_envs = FxHashMap::default();
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(OsStr::new("PROBE_BAD"))),
            Arc::<OsStr>::from(non_utf8_os_string()),
        );

        let workspace_root = vt_path::current_dir().expect("cwd");
        let mismatch =
            fingerprint.validate(&workspace_root, &unfiltered_envs).expect("validation succeeds");

        match mismatch {
            Some(PostRunMismatch::TrackedEnvQuery {
                query,
                mismatch: EnvMismatch::Changed { name },
            }) => {
                assert_eq!(query, TrackedEnvQuery::Glob(Str::from("PROBE_*")));
                assert_eq!(name.as_str(), "PROBE_BAD");
            }
            other => panic!("expected changed tracked env query mismatch, got {other:?}"),
        }
    }

    #[test]
    fn validate_tracked_env_prefix_treats_star_literally() {
        let mut tracked_env_queries = BTreeMap::new();
        let mut stored_matches = BTreeMap::new();
        stored_matches.insert(Str::from("PROBE_*A"), EnvValueHash::new("literal"));
        tracked_env_queries.insert(TrackedEnvQuery::Prefix(Str::from("PROBE_*")), stored_matches);
        let fingerprint =
            PostRunFingerprint { tracked_env_queries, ..PostRunFingerprint::default() };

        let mut unfiltered_envs = FxHashMap::default();
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(OsStr::new("PROBE_*A"))),
            Arc::<OsStr>::from(OsStr::new("literal")),
        );
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(OsStr::new("PROBE_XA"))),
            Arc::<OsStr>::from(OsStr::new("wildcard if interpreted as glob")),
        );

        let workspace_root = vt_path::current_dir().expect("cwd");
        let mismatch =
            fingerprint.validate(&workspace_root, &unfiltered_envs).expect("validation succeeds");

        assert!(mismatch.is_none());
    }

    #[test]
    fn validate_ignores_non_utf8_tracked_env_glob_names() {
        let mut tracked_env_queries = BTreeMap::new();
        tracked_env_queries.insert(TrackedEnvQuery::Glob(Str::from("PROBE_*")), BTreeMap::new());
        let fingerprint =
            PostRunFingerprint { tracked_env_queries, ..PostRunFingerprint::default() };

        let mut unfiltered_envs = FxHashMap::default();
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(non_utf8_os_string())),
            Arc::<OsStr>::from(OsStr::new("value")),
        );

        let workspace_root = vt_path::current_dir().expect("cwd");
        let mismatch =
            fingerprint.validate(&workspace_root, &unfiltered_envs).expect("validation succeeds");

        assert!(mismatch.is_none());
    }

    #[test]
    fn validate_looks_up_tracked_env_names_by_platform_rules() {
        let mut tracked_envs = BTreeMap::new();
        tracked_envs.insert(Str::from("PROBE_ENV"), Some(EnvValueHash::new("value")));
        let fingerprint = PostRunFingerprint { tracked_envs, ..PostRunFingerprint::default() };

        let mut unfiltered_envs = FxHashMap::default();
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(OsStr::new("probe_env"))),
            Arc::<OsStr>::from(OsStr::new("value")),
        );

        let workspace_root = vt_path::current_dir().expect("cwd");
        let mismatch =
            fingerprint.validate(&workspace_root, &unfiltered_envs).expect("validation succeeds");

        assert_eq!(mismatch.is_none(), cfg!(windows));
    }

    #[test]
    fn validate_matches_tracked_env_prefix_by_platform_rules() {
        let mut tracked_env_queries = BTreeMap::new();
        let mut stored_matches = BTreeMap::new();
        stored_matches.insert(Str::from("probe_a"), EnvValueHash::new("value"));
        tracked_env_queries.insert(TrackedEnvQuery::Prefix(Str::from("PROBE_")), stored_matches);
        let fingerprint =
            PostRunFingerprint { tracked_env_queries, ..PostRunFingerprint::default() };

        let mut unfiltered_envs = FxHashMap::default();
        unfiltered_envs.insert(
            EnvName::new(Arc::<OsStr>::from(OsStr::new("probe_a"))),
            Arc::<OsStr>::from(OsStr::new("value")),
        );

        let workspace_root = vt_path::current_dir().expect("cwd");
        let mismatch =
            fingerprint.validate(&workspace_root, &unfiltered_envs).expect("validation succeeds");

        assert_eq!(mismatch.is_none(), cfg!(windows));
    }

    // ── Path rules ──────────────────────────────────────────────────────────

    fn rel(path: &str) -> RelativePathBuf {
        RelativePathBuf::new(path).unwrap()
    }

    /// A workspace in a temporary directory holding `files`; a name ending in
    /// `/` is an empty folder.
    fn workspace(files: &[&str]) -> (tempfile::TempDir, vt_path::AbsolutePathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = vt_path::AbsolutePathBuf::new(tmp.path().to_path_buf()).unwrap();
        for name in files {
            let path = root.join(name.trim_end_matches('/'));
            if name.ends_with('/') {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, *name).unwrap();
            }
        }
        (tmp, root)
    }

    /// The fingerprint of a run that read `reads` (`(path, listed)`) and wrote
    /// `writes` (`(path, created)`), with every write archived except those
    /// under `excluded`.
    fn record(
        root: &AbsolutePath,
        reads: &[(&str, bool)],
        writes: &[(&str, bool)],
        excluded: &str,
    ) -> PostRunFingerprint {
        let reads: HashMap<RelativePathBuf, PathRead> = reads
            .iter()
            .map(|(path, listed)| (rel(path), PathRead { read_dir_entries: *listed }))
            .collect();
        let writes: HashMap<RelativePathBuf, PathWrite> = writes
            .iter()
            .map(|(path, created)| (rel(path), PathWrite { created: *created }))
            .collect();
        let is_excluded = |path: &RelativePathBuf| {
            !excluded.is_empty() && path.strip_prefix(rel(excluded)).is_some()
        };
        let archived = writes.keys().filter(|path| !is_excluded(path)).cloned().collect();
        let rebuildable = Rebuildable::new(root, &writes, &archived, is_excluded).unwrap();
        PostRunFingerprint::create(
            &reads,
            &rebuildable,
            root,
            &BTreeMap::new(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .unwrap()
    }

    /// The first mismatch against the workspace as it is now, as
    /// `"{kind:?} {path}"`.
    fn validate(fingerprint: &PostRunFingerprint, root: &AbsolutePath) -> Option<Str> {
        fingerprint.validate(root, &FxHashMap::default()).unwrap().map(|mismatch| match mismatch {
            PostRunMismatch::Input { kind, path } => vt_str::format!("{kind:?} {path}"),
            other => panic!("unexpected mismatch {other:?}"),
        })
    }

    #[test]
    fn created_file_may_be_missing_but_not_changed() {
        let (_tmp, root) = workspace(&["package.json", "dist/main.js"]);
        let fingerprint = record(
            &root,
            &[("dist/main.js", false)],
            &[("dist", true), ("dist/main.js", true)],
            "",
        );
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::write(root.join("dist/main.js"), "stale").unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("ContentModified dist/main.js"));

        std::fs::remove_dir_all(root.join("dist")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
    }

    #[test]
    fn modified_file_must_be_as_left() {
        // tsdown reads `package.json` and writes `exports` into it.
        let (_tmp, root) = workspace(&["package.json"]);
        let fingerprint = record(&root, &[("package.json", false)], &[("package.json", false)], "");
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::write(root.join("package.json"), "{}").unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("ContentModified package.json"));

        std::fs::remove_file(root.join("package.json")).unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Removed package.json"));
    }

    #[test]
    fn created_folder_the_task_did_not_list_may_lack_or_gain_entries() {
        // Vite checks `dist` before creating it; rolldown creates
        // `dist/assets` and writes into it without looking at it.
        let (_tmp, root) = workspace(&["package.json", "dist/index.html", "dist/assets/main.js"]);
        let fingerprint = record(
            &root,
            &[("dist", false)],
            &[
                ("dist", true),
                ("dist/index.html", true),
                ("dist/assets", true),
                ("dist/assets/main.js", true),
            ],
            "",
        );
        assert!(!fingerprint.inferred_inputs.contains_key(&rel("dist/assets")));
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::remove_file(root.join("dist/assets/main.js")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
        // A hit doesn't delete: a file another step or an older build left
        // in an unlisted output folder stays.
        std::fs::write(root.join("dist/assets/stale.js"), "").unwrap();
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::remove_dir_all(root.join("dist")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
    }

    #[test]
    fn folder_renamed_into_place_must_be_as_left() {
        // The task writes `dist.tmp/a.js` and renames `dist.tmp` to `dist`:
        // a hit can't rebuild `a.js`.
        let (_tmp, root) = workspace(&["package.json", "dist/a.js"]);
        let fingerprint =
            record(&root, &[], &[("dist.tmp", true), ("dist.tmp/a.js", true), ("dist", true)], "");
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::write(root.join("dist/a.js"), "stale").unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("ContentModified dist/a.js"));

        std::fs::remove_file(root.join("dist/a.js")).unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Removed dist/a.js"));
    }

    #[test]
    fn folder_holding_only_created_files_may_be_missing() {
        // tsdown lists an existing `dist`, empties it and fills it again.
        let (_tmp, root) = workspace(&["package.json", "dist/index.mjs"]);
        let fingerprint = record(&root, &[("dist", true)], &[("dist/index.mjs", true)], "");
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::write(root.join("dist/stale.mjs"), "").unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Added dist/stale.mjs"));

        std::fs::remove_dir_all(root.join("dist")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
    }

    #[test]
    fn folder_holding_more_than_outputs_must_exist() {
        // Codegen lists `src` and creates `src/gen.ts` next to the sources.
        let (_tmp, root) = workspace(&["package.json", "src/a.ts", "src/gen.ts"]);
        let fingerprint = record(&root, &[("src", true)], &[("src/gen.ts", true)], "");

        std::fs::remove_file(root.join("src/gen.ts")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
        std::fs::write(root.join("src/b.ts"), "").unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Added src/b.ts"));

        std::fs::remove_dir_all(root.join("src")).unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Removed src"));
    }

    #[test]
    fn listed_folder_may_lack_only_entries_the_task_created() {
        // `eslint --cache .` lists the project and creates `.eslintcache`.
        let (_tmp, root) = workspace(&["package.json", "src/a.js", ".eslintcache"]);
        let fingerprint = record(&root, &[("", true)], &[(".eslintcache", true)], "");

        std::fs::remove_file(root.join(".eslintcache")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
        std::fs::remove_dir_all(root.join("src")).unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Removed src"));
    }

    #[test]
    fn excluded_outputs_are_not_checked() {
        // Vite renames its dependency cache into place and excludes it from
        // outputs.
        let (_tmp, root) = workspace(&["package.json", "node_modules/.vite/deps/react.js"]);
        let fingerprint = record(
            &root,
            &[],
            &[
                ("node_modules/.vite", true),
                ("node_modules/.vite/deps_temp/react.js", true),
                ("node_modules/.vite/deps", true),
            ],
            "node_modules/.vite/deps",
        );
        assert!(!fingerprint.inferred_inputs.contains_key(&rel("node_modules/.vite/deps")));

        std::fs::write(root.join("node_modules/.vite/deps/stale.js"), "").unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
        std::fs::remove_dir_all(root.join("node_modules/.vite")).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
    }

    #[test]
    fn checked_folder_is_a_directory() {
        let (_tmp, root) = workspace(&["src/a.js"]);
        let fingerprint = record(&root, &[("src", false)], &[], "");
        assert_eq!(validate(&fingerprint, &root), None);
        std::fs::write(root.join("src/b.js"), "").unwrap();
        assert_eq!(validate(&fingerprint, &root), None);
        std::fs::remove_dir_all(root.join("src")).unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Removed src"));
    }

    #[test]
    fn listings_include_dist_and_leave_out_ds_store() {
        let (_tmp, root) = workspace(&["dist/a.js", ".DS_Store", "src/"]);
        let fingerprint = record(&root, &[("", true)], &[], "");
        let PathFingerprint::Folder(Some(entries)) = &fingerprint.inferred_inputs[&rel("")].state
        else {
            panic!("{:?}", fingerprint.inferred_inputs[&rel("")]);
        };
        assert_eq!(entries.keys().map(Str::as_str).collect::<Vec<_>>(), ["dist", "src"]);
    }

    #[test]
    fn created_path_removed_again_must_stay_missing() {
        let (_tmp, root) = workspace(&["package.json"]);
        let fingerprint = record(&root, &[("out.tmp", false)], &[("out.tmp", true)], "");
        assert_eq!(validate(&fingerprint, &root), None);
        std::fs::write(root.join("out.tmp"), "").unwrap();
        assert_eq!(validate(&fingerprint, &root).as_deref(), Some("Added out.tmp"));
    }

    /// pnpm's cold store leaves a `.bin` in a store package's `node_modules`
    /// that a warm store doesn't have; see `redact::pnpm_store_bin_dir`.
    #[test]
    fn store_package_node_modules_with_only_bin_is_missing() {
        const DIR: &str = "node_modules/.pnpm/esbuild@0.28.2/node_modules/esbuild/node_modules";
        let bin = vt_str::format!("{DIR}/.bin/");
        let (_tmp, root) = workspace(&[bin.as_str()]);
        let fingerprint = record(&root, &[(DIR, false)], &[], "");
        assert_eq!(fingerprint.inferred_inputs[&rel(DIR)].state, PathFingerprint::NotFound);

        std::fs::remove_dir_all(root.join(DIR)).unwrap();
        assert_eq!(validate(&fingerprint, &root), None);

        std::fs::create_dir_all(root.join(DIR).join("left-pad")).unwrap();
        assert_eq!(
            validate(&fingerprint, &root).as_deref(),
            Some(vt_str::format!("Added {DIR}").as_str())
        );
    }
}
