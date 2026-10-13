use crate::archive::browser::BrowserArchiveWriter;
use crate::archive::{
    latest_browser_visit_per_source_profile, read_browser_index_entries, ArchiveWriter,
};
use crate::browser::chrome::ChromiumHistoryCollector;
use crate::cli::Browser;
use crate::error::{Result, WaylogError};
use crate::output::Output;
use crate::providers;
use crate::utils::path;
use std::fs::{File, OpenOptions, TryLockError};
#[cfg(windows)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::time;

pub struct WatchCoordination {
    pub archive_lock_file: Option<PathBuf>,
    pub git_dir: Option<PathBuf>,
    pub interval_seconds: u64,
    #[cfg(target_os = "macos")]
    pub instance_lock_file: Option<PathBuf>,
}

pub async fn handle_watch(
    provider_name: Option<String>,
    browser: Option<Browser>,
    no_browser: bool,
    archive_dir: Option<PathBuf>,
    coordination: WatchCoordination,
    output: &mut Output,
) -> Result<()> {
    let WatchCoordination {
        archive_lock_file,
        git_dir,
        interval_seconds,
        #[cfg(target_os = "macos")]
        instance_lock_file,
    } = coordination;
    #[cfg(target_os = "macos")]
    let _instance_guard = acquire_instance_lock(instance_lock_file.as_deref())?;
    if interval_seconds == 0 {
        return Err(WaylogError::PathError(
            "watch interval must be at least one second".to_string(),
        ));
    }
    if git_dir.is_some() && archive_lock_file.is_none() {
        return Err(WaylogError::PathError(
            "--git-dir requires --archive-lock-file".to_string(),
        ));
    }
    if let Some(ref name) = provider_name {
        match providers::get_provider(name) {
            Ok(_) => {}
            Err(WaylogError::ProviderNotFound(invalid_name)) => {
                output.unknown_provider(&invalid_name)?;
                return Err(WaylogError::ProviderNotFound(name.clone()));
            }
            Err(error) => return Err(error),
        }
    }

    let archive_dir = archive_dir.unwrap_or(path::get_default_archive_dir()?);
    let writer = ArchiveWriter::new(archive_dir.clone());
    let browser_writer = BrowserArchiveWriter::new(archive_dir.clone());

    output.info(format!(
        "Watching local session data and syncing to {} every {} seconds",
        archive_dir.display(),
        interval_seconds
    ))?;
    output.info("Press Ctrl+C to stop watching.")?;

    let watched_providers = if let Some(name) = provider_name {
        vec![providers::get_provider(&name)?]
    } else {
        providers::all_providers()
    };

    let mut interval = time::interval(Duration::from_secs(interval_seconds));

    // Register SIGTERM before the first collection cycle. launchd uses it on stop.
    #[cfg(target_os = "macos")]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(target_os = "macos")]
    let stop = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    };
    #[cfg(not(target_os = "macos"))]
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Keep the guard alive across the complete session and browser cycle.
                // A busy sync worker postpones collection until the next normal tick.
                let Some(_cycle_guard) = try_begin_watch_cycle(
                    archive_lock_file.as_deref(),
                    git_dir.as_deref(),
                )? else {
                    continue;
                };
                path::ensure_dir_exists(&archive_dir)?;
                let mut written = 0usize;
                let mut unchanged = 0usize;
                let mut filtered = 0usize;
                let mut browser_updated = 0usize;
                let mut browser_unchanged = 0usize;

                for provider in &watched_providers {
                    if !provider.has_local_data() {
                        continue;
                    }

                    let session_paths = provider.get_all_host_sessions().await?;
                    for session_path in session_paths {
                        let session = match provider.parse_session(&session_path).await {
                            Ok(session) => session,
                            Err(error) => {
                                tracing::warn!(
                                    "Skipping unreadable {} session {}: {}",
                                    provider.name(),
                                    session_path.display(),
                                    error
                                );
                                continue;
                            }
                        };
                        if session.messages.is_empty() {
                            continue;
                        }

                        let result = match writer
                            .export_session(&session, &session_path, provider.raw_extension())
                            .await
                        {
                            Ok(result) => result,
                            Err(error) => {
                                tracing::warn!(
                                    "Skipping failed archive export for {} session {}: {}",
                                    provider.name(),
                                    session_path.display(),
                                    error
                                );
                                continue;
                            }
                        };
                        if let Some(reason) = result.filtered_reason {
                            tracing::info!(
                                "Filtered {} session {} from archive watch sync: {}",
                                provider.name(),
                                session_path.display(),
                                reason
                            );
                            filtered += 1;
                        } else if result.written {
                            written += 1;
                        } else {
                            unchanged += 1;
                        }
                    }
                }

                if browser_enabled(browser.as_ref(), no_browser) {
                    let collector = ChromiumHistoryCollector::new(browser.as_ref())?;
                    let existing_entries = read_browser_index_entries(&archive_dir).await?;
                    let since_by_profile =
                        latest_browser_visit_per_source_profile(&existing_entries);
                    let collected = collector.collect_visits_since(&since_by_profile).await?;
                    for warning in &collected.warnings {
                        output.warn(warning)?;
                    }
                    let summary = browser_writer.export_visits(&collected.visits, false).await?;
                    for warning in &summary.warnings {
                        output.warn(warning)?;
                    }
                    browser_updated = summary.updated_groups;
                    browser_unchanged = summary.unchanged_groups;
                }

                if written > 0 || filtered > 0 || browser_updated > 0 {
                    output.info(format!(
                        "Watch sync complete: {} updated, {} unchanged, {} filtered, {} browser groups updated, {} browser groups unchanged",
                        written, unchanged, filtered, browser_updated, browser_unchanged
                    ))?;
                }
            }
            _ = &mut stop => {
                output.info("Stopped watching local session data.")?;
                break;
            }
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn acquire_instance_lock(lock_path: Option<&Path>) -> Result<Option<WatchCycleGuard>> {
    let Some(path) = lock_path else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(WatchCycleGuard {
            _lock_file: Some(file),
        })),
        Err(TryLockError::WouldBlock) => {
            Err(WaylogError::PathError("watch is already running".into()))
        }
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

/// Closing the exclusively locked file releases the lock on success, error or
/// cancellation. The sync worker uses this same file (flock on Unix, byte-range
/// locking on Windows); keep it outside the archive and never replace it.
struct WatchCycleGuard {
    _lock_file: Option<File>,
}

impl Drop for WatchCycleGuard {
    fn drop(&mut self) {
        if let Some(file) = &self._lock_file {
            // Explicit unlock also releases an inherited Unix file description
            // if another thread is concurrently spawning a child process.
            if let Err(error) = file.unlock() {
                tracing::warn!("Failed to release watch archive lock: {}", error);
            }
        }
    }
}

fn try_begin_watch_cycle(
    archive_lock_file: Option<&Path>,
    git_dir: Option<&Path>,
) -> Result<Option<WatchCycleGuard>> {
    let lock_file = if let Some(lock_path) = archive_lock_file {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        match file.try_lock() {
            Ok(()) => {
                // msvcrt.locking peers reserve byte zero. Seed it while holding
                // the lock so those peers never need to write to a locked file.
                #[cfg(windows)]
                {
                    let mut file = &file;
                    if file.metadata()?.len() == 0 {
                        file.write_all(&[0])?;
                        file.flush()?;
                    }
                }
                Some(file)
            }
            Err(TryLockError::WouldBlock) => {
                tracing::debug!("Deferring watch cycle: archive is locked by the sync worker");
                return Ok(None);
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
    } else {
        None
    };
    let guard = WatchCycleGuard {
        _lock_file: lock_file,
    };

    if let Some(git_dir) = git_dir {
        if !git_dir.is_dir() {
            return Err(WaylogError::PathError(format!(
                "Watch Git directory is missing or is not a directory: {}",
                git_dir.display()
            )));
        }
        for marker in [
            "rebase-merge",
            "rebase-apply",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
        ] {
            if git_dir.join(marker).try_exists()? {
                tracing::debug!(
                    "Deferring watch cycle: unfinished Git operation {} in {}",
                    marker,
                    git_dir.display()
                );
                return Ok(None);
            }
        }
    }

    Ok(Some(guard))
}

fn browser_enabled(browser: Option<&Browser>, no_browser: bool) -> bool {
    !no_browser && matches!(browser, None | Some(Browser::Chrome) | Some(Browser::Atlas))
}

#[cfg(test)]
mod tests {
    use super::{browser_enabled, try_begin_watch_cycle};
    use crate::cli::Browser;
    use crate::error::{Result, WaylogError};
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    #[cfg(target_os = "macos")]
    #[test]
    fn native_watch_singleton_is_separate_from_cycle_lock_and_releases() {
        let tmp = TempDir::new().unwrap();
        let singleton = tmp.path().join("watch.lock");
        let cycles = tmp.path().join("archive-access.lock");
        let instance = super::acquire_instance_lock(Some(&singleton)).unwrap();
        assert!(super::acquire_instance_lock(Some(&singleton)).is_err());
        assert!(try_begin_watch_cycle(Some(&cycles), None)
            .unwrap()
            .is_some());
        drop(instance);
        assert!(super::acquire_instance_lock(Some(&singleton))
            .unwrap()
            .is_some());
    }

    fn mock_cycle(lock: &Path, git_dir: Option<&Path>, archive: &Path) -> Result<bool> {
        let Some(_guard) = try_begin_watch_cycle(Some(lock), git_dir)? else {
            return Ok(false);
        };
        std::fs::create_dir_all(archive)?;
        std::fs::write(archive.join("collection.txt"), "collected")?;
        Ok(true)
    }

    #[test]
    fn test_standalone_watch_cycle_needs_no_lock_file() {
        assert!(try_begin_watch_cycle(None, None).unwrap().is_some());
    }

    #[test]
    fn test_busy_lock_skips_without_archive_writes_and_resumes() {
        let tmp = TempDir::new().unwrap();
        let lock = tmp.path().join("archive-access.lock");
        let archive = tmp.path().join("archive");
        let sync_guard = try_begin_watch_cycle(Some(&lock), None).unwrap().unwrap();
        assert!(!mock_cycle(&lock, None, &archive).unwrap());
        assert!(!archive.exists());
        drop(sync_guard);
        assert!(mock_cycle(&lock, None, &archive).unwrap());
        assert_eq!(
            std::fs::read_to_string(archive.join("collection.txt")).unwrap(),
            "collected"
        );
    }

    #[test]
    fn test_unfinished_git_operations_pause_and_release_lock() {
        for marker in [
            "rebase-merge",
            "rebase-apply",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
        ] {
            let tmp = TempDir::new().unwrap();
            let lock = tmp.path().join("archive-access.lock");
            let archive = tmp.path().join("archive");
            let git_dir = tmp.path().join("git-dir");
            std::fs::create_dir(&git_dir).unwrap();
            std::fs::write(git_dir.join(marker), "unfinished").unwrap();

            assert!(!mock_cycle(&lock, Some(&git_dir), &archive).unwrap());
            assert!(!archive.exists());
            assert!(try_begin_watch_cycle(Some(&lock), None).unwrap().is_some());

            std::fs::remove_file(git_dir.join(marker)).unwrap();
            assert!(mock_cycle(&lock, Some(&git_dir), &archive).unwrap());
        }
    }

    #[test]
    fn test_cycle_guard_releases_lock_on_error() {
        let tmp = TempDir::new().unwrap();
        let lock = tmp.path().join("archive-access.lock");
        let failing_cycle = || -> Result<()> {
            let _guard = try_begin_watch_cycle(Some(&lock), None)?.unwrap();
            Err(WaylogError::Internal("mock collection failure".to_string()))
        };
        assert!(failing_cycle().is_err());
        assert!(try_begin_watch_cycle(Some(&lock), None).unwrap().is_some());

        let missing_git_dir = tmp.path().join("missing-git-dir");
        assert!(try_begin_watch_cycle(Some(&lock), Some(&missing_git_dir)).is_err());
        assert!(try_begin_watch_cycle(Some(&lock), None).unwrap().is_some());
    }

    #[test]
    fn test_lock_open_preserves_existing_contents() {
        let tmp = TempDir::new().unwrap();
        let lock = tmp.path().join("archive-access.lock");
        std::fs::write(&lock, "existing lock content").unwrap();
        let guard = try_begin_watch_cycle(Some(&lock), None).unwrap().unwrap();
        drop(guard);
        assert_eq!(
            std::fs::read_to_string(&lock).unwrap(),
            "existing lock content"
        );
    }

    #[test]
    fn test_cross_process_lock_contention_and_release() {
        let tmp = TempDir::new().unwrap();
        let lock = tmp.path().join("archive-access.lock");
        let guard = try_begin_watch_cycle(Some(&lock), None).unwrap().unwrap();
        let run_peer = |expected: &str| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "commands::watch::tests::test_lock_peer_process",
                    "--nocapture",
                ])
                .env("WAYLOG_TEST_LOCK_PEER_FILE", &lock)
                .env("WAYLOG_TEST_LOCK_PEER_EXPECT", expected)
                .output()
                .unwrap()
        };
        let blocked = run_peer("blocked");
        assert!(blocked.status.success(), "{:?}", blocked);
        drop(guard);
        let acquired = run_peer("acquired");
        assert!(acquired.status.success(), "{:?}", acquired);
    }

    #[test]
    fn test_lock_peer_process() {
        let Some(lock) = std::env::var_os("WAYLOG_TEST_LOCK_PEER_FILE") else {
            return;
        };
        let guard = try_begin_watch_cycle(Some(Path::new(&lock)), None).unwrap();
        let expected = std::env::var("WAYLOG_TEST_LOCK_PEER_EXPECT").unwrap();
        assert_eq!(guard.is_some(), expected == "acquired");
    }

    #[test]
    fn test_browser_enabled_by_default() {
        assert!(browser_enabled(None, false));
    }

    #[test]
    fn test_browser_enabled_when_chrome_is_explicit() {
        assert!(browser_enabled(Some(&Browser::Chrome), false));
    }

    #[test]
    fn test_browser_enabled_when_atlas_is_explicit() {
        assert!(browser_enabled(Some(&Browser::Atlas), false));
    }

    #[test]
    fn test_browser_disabled_when_no_browser_flag_is_set() {
        assert!(!browser_enabled(None, true));
    }
}
