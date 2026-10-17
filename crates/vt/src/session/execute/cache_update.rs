//! Post-run cache update: decide whether a finished spawn may be cached and,
//! if so, store its fingerprint, captured output, and output archive.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use rustc_hash::FxHashSet;
use tokio_util::sync::CancellationToken;
use vt_path::{AbsolutePath, RelativePathBuf};
use vt_plan::cache_metadata::{CacheMetadata, EnvValueHash};
use vt_server::Reports;
use vt_str::Str;

use super::{
    CacheState,
    fingerprint::{PathRead, PathWrite, PostRunFingerprint, TrackedEnvQuery},
    glob,
    rebuildable::Rebuildable,
    spawn::ChildOutcome,
};
use crate::{
    collections::HashMap,
    session::{
        cache::{CacheEntryValue, ExecutionCache, archive, checkout::RecordedCheckout},
        event::{CacheErrorKind, CacheNotUpdatedReason, CacheUpdateStatus, ExecutionError},
    },
};

/// Post-execution summary of what fspy observed for a single task. Fields are
/// cfg-agnostic so the decision logic below doesn't need `cfg(fspy)` — the
/// value is only ever `Some` when tracking happened (see [`observe_fspy`]).
struct TrackingOutcome {
    /// Auto-input reads after input exclusions are applied. Empty when
    /// `input_config.includes_auto` is false.
    path_reads: HashMap<RelativePathBuf, PathRead>,
    /// Every write the task made, with whether it created the path, before
    /// output exclusions are applied: what the task wrote decides what a hit
    /// can rebuild.
    path_writes: HashMap<RelativePathBuf, PathWrite>,
    /// Tool-reported `ignoreOutput` paths, workspace-relative.
    ignored_outputs: FxHashSet<RelativePathBuf>,
}

type TrackedEnvValues = BTreeMap<Str, Option<EnvValueHash>>;
type TrackedEnvQueryValues = BTreeMap<TrackedEnvQuery, BTreeMap<Str, EnvValueHash>>;

/// Decide whether the finished run may be cached, and store it if so.
///
/// Every outcome returns a `(status, error)` pair for the caller's single
/// `finish()` call; this function never reports by itself. The guard clauses
/// run in priority order — each names the reason the run is *not* cached, and
/// only a run that passes them all is stored. A run cancelled by Ctrl-C or
/// fast-fail (`cancel_token`) isn't cached.
#[expect(
    clippy::too_many_arguments,
    reason = "the run's full context is genuinely needed to decide and store the cache entry"
)]
pub(super) async fn update_cache(
    cache: &ExecutionCache,
    workspace_root: &Arc<AbsolutePath>,
    cache_dir: &AbsolutePath,
    state: CacheState<'_>,
    outcome: &ChildOutcome,
    reports: Option<&Reports>,
    duration: Duration,
    cancel_token: &CancellationToken,
) -> (CacheUpdateStatus, Option<ExecutionError>) {
    let CacheState { metadata, globbed_inputs, std_outputs, tracking } = state;
    let fspy = tracking.fspy.as_ref();

    if let Some(reports) = reports
        && reports.cache_disabled
    {
        // A runner-aware tool short-circuited caching via `disableCache()`
        // (e.g. a dev server with no deterministic output).
        return (CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::ToolRequested), None);
    }

    // Tool-reported paths to exclude from auto input tracking. Absolute paths
    // are normalized to workspace-relative; anything outside is dropped.
    let ignored_input_rels: FxHashSet<RelativePathBuf> = reports
        .map(|r| normalize_ignored_paths(&r.ignored_inputs, workspace_root))
        .unwrap_or_default();
    let ignored_output_rels: FxHashSet<RelativePathBuf> = reports
        .map(|r| normalize_ignored_paths(&r.ignored_outputs, workspace_root))
        .unwrap_or_default();

    if cancel_token.is_cancelled() {
        // Cancelled (Ctrl-C or sibling failure) — result is untrustworthy.
        return (CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::Cancelled), None);
    }

    if !outcome.exit_status.success() {
        // Execution failed with non-zero exit status — don't update cache.
        return (CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::NonZeroExitStatus), None);
    }

    // The accesses, or `None` when the task was not tracked at all. An
    // `Err` means it made more file accesses than the tracking channel had
    // room for, so what arrived is a subset of what it touched, and an entry
    // built from a subset would replay with inputs and outputs missing.
    #[cfg(fspy)]
    let Ok(path_accesses) = outcome.path_accesses.as_ref().map(Result::as_ref).transpose() else {
        return (CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::TrackingIncomplete), None);
    };

    let fspy_outcome = observe_fspy(
        #[cfg(fspy)]
        path_accesses,
        metadata,
        fspy,
        &ignored_input_rels,
        ignored_output_rels,
        workspace_root,
    );

    if fspy_outcome.is_none() && fspy.is_some() {
        // Task requested fspy auto-inference but this binary was built without
        // `cfg(fspy)`. Task ran, but we can't compute a valid cache entry
        // without tracked path accesses.
        return (CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::FspyUnsupported), None);
    }

    // Collect tool-reported tracked envs for the post-run fingerprint. Env
    // names that the user already declared are skipped because their values
    // are already part of the spawn fingerprint.
    let (tracked_envs, tracked_env_queries) = match collect_tracked_reports(reports, metadata) {
        Ok(tracked_reports) => tracked_reports,
        Err(err) => {
            return (
                CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::CacheDisabled),
                Some(ExecutionError::PostRunFingerprint(err)),
            );
        }
    };

    let outputs = match Outputs::collect(metadata, fspy_outcome.as_ref(), fspy, workspace_root) {
        Ok(outputs) => outputs,
        Err(err) => {
            return (
                CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::CacheDisabled),
                Some(err),
            );
        }
    };

    let empty_path_reads = HashMap::default();
    let path_reads = fspy_outcome.as_ref().map_or(&empty_path_reads, |o| &o.path_reads);
    let post_run_fingerprint = match PostRunFingerprint::create(
        path_reads,
        &outputs.rebuildable,
        workspace_root,
        &globbed_inputs,
        tracked_envs,
        tracked_env_queries,
    ) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            return (
                CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::CacheDisabled),
                Some(ExecutionError::PostRunFingerprint(err)),
            );
        }
    };

    let archived = match outputs.archive(workspace_root, cache_dir) {
        Ok(archived) => archived,
        Err(err) => {
            return (
                CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::CacheDisabled),
                Some(ExecutionError::Cache { kind: CacheErrorKind::Update, source: err }),
            );
        }
    };

    let new_cache_value = CacheEntryValue {
        post_run_fingerprint,
        std_outputs: std_outputs.into(),
        duration,
        globbed_inputs,
        output_archive: archived.archive,
        checkout: RecordedCheckout {
            root: RecordedCheckout::root_string(workspace_root),
            outputs_contain_root: archived.contain_root,
        },
    };
    match cache.update(metadata, new_cache_value, cache_dir).await {
        Ok(upload_error) => (CacheUpdateStatus::Updated { upload_error }, None),
        Err(err) => (
            CacheUpdateStatus::NotUpdated(CacheNotUpdatedReason::CacheDisabled),
            Some(ExecutionError::Cache { kind: CacheErrorKind::Update, source: err }),
        ),
    }
}

/// Summarize the run's fspy observations. `Some` iff tracking was both
/// requested (`tracking.fspy.is_some()`) and compiled in (`cfg(fspy)`). On a
/// `cfg(not(fspy))` build this is always `None`, and [`update_cache`]
/// short-circuits to `FspyUnsupported` when tracking was needed.
///
/// `path_reads` is gated on `input_config.includes_auto`, filtered by
/// user-configured input negatives, and by tool-reported `ignoreInput` paths.
/// Output exclusions are applied by [`Outputs::collect`].
#[cfg_attr(
    not(fspy),
    expect(clippy::missing_const_for_fn, reason = "the body is a no-op without fspy")
)]
fn observe_fspy(
    #[cfg(fspy)] path_accesses: Option<&fspy::PathAccessIterable>,
    metadata: &CacheMetadata,
    fspy: Option<&super::FspyTracking<'_>>,
    ignored_input_rels: &FxHashSet<RelativePathBuf>,
    ignored_output_rels: FxHashSet<RelativePathBuf>,
    workspace_root: &AbsolutePath,
) -> Option<TrackingOutcome> {
    #[cfg(fspy)]
    {
        use super::tracked_accesses::TrackedPathAccesses;

        path_accesses.map(|raw| {
            let tracked = TrackedPathAccesses::from_raw(raw, workspace_root);
            let filtered_path_reads: HashMap<RelativePathBuf, PathRead> =
                // fspy can be attached for auto-output-only tasks. In that
                // mode reads must not become inferred inputs.
                if metadata.input_config.includes_auto
                    && let Some(fspy) = fspy
                {
                    tracked
                        .path_reads
                        .iter()
                        .filter(|(path, _)| {
                            !fspy.input_negative_globs.is_match(path.as_str())
                                && !is_ignored(path, ignored_input_rels)
                        })
                        .map(|(path, read)| (path.clone(), *read))
                        .collect()
                } else {
                    HashMap::default()
                };
            TrackingOutcome {
                path_reads: filtered_path_reads,
                path_writes: tracked.path_writes,
                ignored_outputs: ignored_output_rels,
            }
        })
    }
    #[cfg(not(fspy))]
    {
        let _ = (metadata, fspy, ignored_input_rels, ignored_output_rels, workspace_root);
        None
    }
}

fn collect_tracked_reports(
    reports: Option<&Reports>,
    metadata: &CacheMetadata,
) -> anyhow::Result<(TrackedEnvValues, TrackedEnvQueryValues)> {
    reports
        .map(|reports| {
            let tracked_envs = collect_tracked_envs(reports, metadata)?;
            let tracked_env_queries = collect_tracked_env_queries(reports)?;
            Ok::<_, anyhow::Error>((tracked_envs, tracked_env_queries))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

/// Normalize tool-reported absolute paths to cleaned workspace-relative paths.
/// Paths outside the workspace are dropped — they can't contribute to inputs
/// or outputs.
fn normalize_ignored_paths(
    paths: &FxHashSet<Arc<AbsolutePath>>,
    workspace_root: &AbsolutePath,
) -> FxHashSet<RelativePathBuf> {
    paths
        .iter()
        .filter_map(|p| p.strip_prefix(workspace_root).ok().flatten()?.clean().ok())
        .collect()
}

/// Whether `path` is covered by any `ignored` entry. An ignored entry matches
/// itself (exact file) and everything under it (directory subtree).
#[cfg(fspy)]
fn is_ignored(path: &RelativePathBuf, ignored: &FxHashSet<RelativePathBuf>) -> bool {
    if ignored.is_empty() {
        return false;
    }
    ignored.contains(path) || ignored.iter().any(|ig| path.strip_prefix(ig).is_some())
}

/// Select tool-reported env records to embed in the post-run fingerprint.
/// Names that the user already declared as fingerprinted are skipped because
/// their values are already in the spawn fingerprint.
fn collect_tracked_envs(
    reports: &Reports,
    metadata: &CacheMetadata,
) -> anyhow::Result<TrackedEnvValues> {
    let fingerprinted = &metadata.spawn_fingerprint.env_fingerprints().fingerprinted_envs;
    let mut tracked_envs = BTreeMap::new();

    for (name, value) in &reports.tracked_get_env {
        let name_str = name
            .inner()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("tracked env name is not valid UTF-8"))?;
        if fingerprinted.contains_key(name_str) {
            continue;
        }
        let value = value
            .as_ref()
            .map(|value| {
                let value_str = value.to_str().ok_or_else(|| {
                    anyhow::anyhow!("tracked env value for {name_str} is not valid UTF-8")
                })?;
                Ok::<_, anyhow::Error>(EnvValueHash::new(value_str))
            })
            .transpose()?;
        tracked_envs.insert(Str::from(name_str), value);
    }

    Ok(tracked_envs)
}

/// Select tool-reported bulk env query records to embed in the post-run
/// fingerprint. The full match-set is stored as value hashes.
fn collect_tracked_env_queries(reports: &Reports) -> anyhow::Result<TrackedEnvQueryValues> {
    let mut tracked_env_queries = BTreeMap::new();

    for (query, record) in &reports.tracked_get_envs {
        let mut matches = BTreeMap::new();
        for (name, value) in &record.matches {
            let name_str = name
                .inner()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("tracked env match name is not valid UTF-8"))?;
            let value_str = value.to_str().ok_or_else(|| {
                anyhow::anyhow!("tracked env match value for {name_str} is not valid UTF-8")
            })?;
            matches.insert(Str::from(name_str), EnvValueHash::new(value_str));
        }
        let query = match query {
            vt_server::EnvQuery::Glob(pattern) => {
                TrackedEnvQuery::Glob(Str::from(pattern.as_ref()))
            }
            vt_server::EnvQuery::Prefix(prefix) => {
                TrackedEnvQuery::Prefix(Str::from(prefix.as_ref()))
            }
        };
        tracked_env_queries.insert(query, matches);
    }

    Ok(tracked_env_queries)
}

/// What a run leaves for the cache.
struct Outputs {
    /// The files to archive: fspy-tracked writes when
    /// `output_config.includes_auto` is true, minus negative output globs and
    /// tool-reported `ignoreOutput` paths, and the files matching the
    /// positive output globs.
    files: FxHashSet<RelativePathBuf>,
    /// What a hit can rebuild.
    rebuildable: Rebuildable,
}

/// The archived outputs of a run.
struct ArchivedOutputs {
    /// The archive's file name in the cache directory, or `None` when the
    /// run had no outputs.
    archive: Option<Str>,
    /// Whether an archived file contains the workspace root.
    contain_root: bool,
}

impl Outputs {
    fn collect(
        cache_metadata: &CacheMetadata,
        tracking: Option<&TrackingOutcome>,
        fspy: Option<&super::FspyTracking<'_>>,
        workspace_root: &AbsolutePath,
    ) -> Result<Self, ExecutionError> {
        let output_config = &cache_metadata.output_config;
        let is_excluded = |path: &RelativePathBuf| {
            fspy.is_some_and(|fspy| fspy.output_negative_globs.is_match(path.as_str()))
                || tracking.is_some_and(|tracking| is_ignored(path, &tracking.ignored_outputs))
        };

        let mut files: FxHashSet<RelativePathBuf> = FxHashSet::default();
        // fspy can also be attached for auto-input-only tasks. In that mode
        // writes must not become auto outputs.
        if output_config.includes_auto
            && let Some(tracking) = tracking
        {
            files.extend(tracking.path_writes.keys().filter(|path| !is_excluded(path)).cloned());
        }
        if !output_config.positive_globs.is_empty() {
            let glob_paths = glob::collect_glob_paths(
                workspace_root,
                &output_config.positive_globs,
                &output_config.negative_globs,
            )
            .map_err(|source| ExecutionError::Cache { kind: CacheErrorKind::Update, source })?;
            files.extend(glob_paths);
        }

        let rebuildable = match tracking {
            Some(tracking) => {
                Rebuildable::new(workspace_root, &tracking.path_writes, &files, is_excluded)
                    .map_err(ExecutionError::PostRunFingerprint)?
            }
            None => Rebuildable::default(),
        };
        Ok(Self { files, rebuildable })
    }

    /// Create a tar.zst archive in the cache directory from the files and the
    /// output folders, so a hit restores those even when empty.
    fn archive(
        &self,
        workspace_root: &AbsolutePath,
        cache_dir: &AbsolutePath,
    ) -> anyhow::Result<ArchivedOutputs> {
        let mut files: Vec<RelativePathBuf> = self.files.iter().cloned().collect();
        files.sort();
        let mut folders: Vec<RelativePathBuf> =
            self.rebuildable.output_folders().map(|(path, _)| path.clone()).collect();
        folders.sort();

        if files.is_empty() && folders.is_empty() {
            return Ok(ArchivedOutputs { archive: None, contain_root: false });
        }

        let archive_name: Str = vt_str::format!("{}.tar.zst", uuid::Uuid::new_v4());
        let archive_path = cache_dir.join(archive_name.as_str());

        let contain_root =
            archive::create_output_archive(workspace_root, &files, &folders, &archive_path)?;

        Ok(ArchivedOutputs { archive: Some(archive_name), contain_root })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rustc_hash::FxHashSet;
    use vt_path::{AbsolutePath, RelativePathBuf};

    use super::normalize_ignored_paths;

    #[test]
    fn normalize_ignored_paths_cleans_relative_components() {
        let workspace_root =
            AbsolutePath::new(if cfg!(windows) { r"C:\repo" } else { "/repo" }).unwrap();
        let ignored =
            workspace_root.join(if cfg!(windows) { r"pkg\..\cache" } else { "pkg/../cache" });
        let mut ignored_paths = FxHashSet::default();
        ignored_paths.insert(Arc::<AbsolutePath>::from(ignored));

        let normalized = normalize_ignored_paths(&ignored_paths, workspace_root);

        let expected = RelativePathBuf::new("cache").unwrap();
        assert!(normalized.contains(&expected));
    }
}
