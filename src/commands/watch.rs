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
    pub coverage: bool,
    pub daily_status_file: Option<PathBuf>,
    pub coverage_device: Option<String>,
    pub sources: Option<Vec<String>>,
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
        coverage,
        coverage_device,
        daily_status_file,
        sources,
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

    if let Some(selected) = &sources {
        if selected.is_empty()
            || selected.iter().any(|s| {
                !["codex", "claude", "gemini", "chrome", "atlas", "none"].contains(&s.as_str())
            })
            || (selected.contains(&"none".to_string()) && selected.len() != 1)
        {
            return Err(WaylogError::Internal(
                "Invalid collection source selection".into(),
            ));
        }
    }
    let no_browser = no_browser
        || sources
            .as_ref()
            .map(|s| !s.iter().any(|v| v == "chrome" || v == "atlas"))
            .unwrap_or(false);
    let browser = if let Some(selected) = &sources {
        match (
            selected.contains(&"chrome".to_string()),
            selected.contains(&"atlas".to_string()),
        ) {
            (true, false) => Some(Browser::Chrome),
            (false, true) => Some(Browser::Atlas),
            _ => None,
        }
    } else {
        browser
    };
    let archive_dir = archive_dir.unwrap_or(path::get_default_archive_dir()?);
    let device = coverage_device.unwrap_or_else(|| {
        archive_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    if coverage
        && (archive_lock_file.is_none()
            || device.is_empty()
            || archive_lock_file
                .as_ref()
                .map(|p| p.starts_with(&archive_dir))
                .unwrap_or(false))
    {
        return Err(WaylogError::Internal(
            "Coverage requires archive lock and device identity".into(),
        ));
    }
    if let Some(file) = &daily_status_file {
        if archive_lock_file.is_none() || file.starts_with(&archive_dir) || coverage {
            return Err(WaylogError::Internal("Daily state must be outside archive and use its lock; strict coverage is separate".into()));
        }
    }
    let writer = ArchiveWriter::new(archive_dir.clone());
    let browser_writer = BrowserArchiveWriter::new(archive_dir.clone());

    output.info(format!(
        "Watching local session data and syncing to {} every {} seconds",
        archive_dir.display(),
        interval_seconds
    ))?;
    output.info("Press Ctrl+C to stop watching.")?;

    let watched_providers = if let Some(selected) = &sources {
        providers::all_providers()
            .into_iter()
            .filter(|p| selected.iter().any(|s| s == p.name()))
            .collect()
    } else if let Some(name) = provider_name {
        vec![providers::get_provider(&name)?]
    } else {
        providers::all_providers()
    };

    let mut interval = time::interval(Duration::from_secs(interval_seconds));
    // A slow archive scan must not cause immediate catch-up collection loops.
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

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
                if let Some(file) = &daily_status_file {
                    if crate::archive::daily_status::read_local(file, &device).await?.collection_enabled == Some(false) {
                        continue; // Explicit control only; no new check when collection is disabled.
                    }
                }
                let mut daily_checks = std::collections::BTreeMap::new();
                use crate::archive::coverage::DailySourceCheck;
                if let Err(error) = path::ensure_dir_exists(&archive_dir) {
                    if let Some(file) = &daily_status_file {
                        let mut failures = std::collections::BTreeMap::new();
                        for provider in &watched_providers {
                            failures.insert(format!("session:{}", provider.name()), DailySourceCheck::failed("archive", "archive_directory_unavailable"));
                        }
                        for name in ["chrome", "atlas"] {
                            if !no_browser && browser.as_ref().map(|b| format!("{b:?}").to_lowercase() == name).unwrap_or(true) {
                                failures.insert(format!("browser:{name}"), DailySourceCheck::failed("archive", "archive_directory_unavailable"));
                            }
                        }
                        crate::archive::daily_status::record_local(file, &device, &chrono::Utc::now().to_rfc3339(), std::collections::BTreeMap::new(), failures).await?;
                        tracing::warn!("Archive directory unavailable: {error}");
                        continue;
                    }
                    return Err(error);
                }
                let cycle_now = chrono::Utc::now();
                let today = cycle_now.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap()).date_naive();
                let targets = [today.pred_opt().unwrap(), today];
                let mut roster = std::collections::BTreeMap::new();
                let mut observations: std::collections::BTreeMap<String, std::collections::BTreeMap<String, crate::archive::coverage::Observation>> = targets.iter().map(|d| (d.to_string(), std::collections::BTreeMap::new())).collect();
                let mut written = 0usize;
                let mut unchanged = 0usize;
                let mut filtered = 0usize;
                let mut browser_updated = 0usize;
                let mut browser_unchanged = 0usize;

                for provider in &watched_providers {
                    let id = format!("session:{}", provider.name());
                    let found = provider.has_local_data();
                    roster.insert(id.clone(), crate::archive::coverage::Source { kind: "session".into(), enabled: true,
                        discovery: if found { "found" } else { "unavailable" }.into() });
                    for day in observations.values_mut() {
                        day.insert(id.clone(), crate::archive::coverage::Observation::pending("unknown", "source-not-discovered", "messages"));
                    }
                    if !found { continue; }
                    let session_paths = match provider.get_all_host_sessions().await {
                        Ok(paths) => paths,
                        Err(error) => {
                            if !coverage && daily_status_file.is_none() { return Err(error); }
                            daily_checks.insert(id.clone(), DailySourceCheck::failed("collection", "source_enumeration_failed"));
                            for day in observations.values_mut() { day.insert(id.clone(), crate::archive::coverage::Observation::pending("failed", "source-enumeration-failed", "messages")); }
                            continue;
                        }
                    };
                    let mut counts = std::collections::BTreeMap::new();
                    let mut read_failed = false;
                    let mut daily_error = None;
                    let mut archive_failed = false;
                    for session_path in session_paths {
                        let session = match provider.parse_session(&session_path).await {
                            Ok(session) => session,
                            Err(error) => {
                                read_failed = true;
                                daily_error.get_or_insert_with(|| DailySourceCheck::failed(
                                    if matches!(&error, WaylogError::Json(_)) { "parse" } else { "read" }, "session_read_or_parse_failed"));
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
                                archive_failed = true;
                                daily_error.get_or_insert_with(|| DailySourceCheck::failed("archive", "session_archive_failed"));
                                tracing::warn!(
                                    "Skipping failed archive export for {} session {}: {}",
                                    provider.name(),
                                    session_path.display(),
                                    error
                                );
                                continue;
                            }
                        };
                        if coverage && result.filtered_reason.is_none() {
                            for message in &session.messages {
                                let date = message.timestamp.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap()).date_naive();
                                *counts.entry(date).or_insert(0u64) += 1;
                            }
                        }
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
                    daily_checks.insert(id.clone(), daily_error.unwrap_or_else(DailySourceCheck::ok));
                    for date in targets {
                        let mut observation = crate::archive::coverage::Observation::pending(
                            if read_failed || archive_failed { "failed" } else { "partial" },
                            if read_failed { "session-read-failed" } else if archive_failed { "archive-write-failed" } else { "enumerator-does-not-prove-full-day" }, "messages");
                        if date < today { observation.attempted_ranges = vec![crate::archive::coverage::day_range(date)]; }
                        observation.record_count = if read_failed { None } else { Some(*counts.get(&date).unwrap_or(&0)) };
                        observation.archive_status = if archive_failed { "failed" } else if read_failed { "unknown" } else { "success" }.into();
                        observations.get_mut(&date.to_string()).unwrap().insert(id.clone(), observation);
                    }
                }
                for name in providers::list_providers() {
                    let id = format!("session:{name}");
                    if !roster.contains_key(&id) {
                        roster.insert(id.clone(), crate::archive::coverage::Source { kind: "session".into(), enabled: false, discovery: "not-selected".into() });
                        for day in observations.values_mut() { day.insert(id.clone(), crate::archive::coverage::Observation::pending("disabled", "provider-not-selected", "messages")); }
                    }
                }

                if browser_enabled(browser.as_ref(), no_browser) {
                    let collector = ChromiumHistoryCollector::new(browser.as_ref());
                    let existing_result = read_browser_index_entries(&archive_dir).await;
                    let browser_index_failed = existing_result.is_err();
                    let existing_entries = match existing_result {
                        Ok(value) => value,
                        Err(error) => { if !coverage && daily_status_file.is_none() { return Err(error); } std::collections::BTreeMap::new() }
                    };
                    let since_by_profile =
                        latest_browser_visit_per_source_profile(&existing_entries);
                    let collected = match collector { Ok(collector) => collector.collect_visits_since(&since_by_profile).await, Err(error) => Err(error) };
                    if let Err(error) = &collected {
                        if !coverage && daily_status_file.is_none() { return Err(WaylogError::Internal(error.to_string())); }
                    }
                    let mut collected = match collected {
                        Ok(value) => value,
                        Err(_) => crate::browser::chrome::BrowserCollectionResult { visits: vec![], warnings: vec!["coverage-browser-read-failed".into()], source_reads: vec![] },
                    };
                    if browser_index_failed { collected.warnings.push("archive-index-read-failed".into()); }
                    for warning in &collected.warnings {
                        output.warn(warning)?;
                    }
                    let summary_result = browser_writer.export_visits(&collected.visits, false).await;
                    for source in &collected.source_reads {
                        let value = if browser_index_failed {
                            Some(DailySourceCheck::failed("archive", "browser_index_read_failed"))
                        } else if source.status == "failed" {
                            Some(DailySourceCheck::failed("read", "browser_read_failed"))
                        } else if source.status == "success" {
                            Some(if summary_result.is_err() { DailySourceCheck::failed("archive", "browser_archive_failed") } else { DailySourceCheck::ok() })
                        } else { None };
                        if let Some(value) = value { daily_checks.insert(source.id.clone(), value); }
                    }
                    for name in ["chrome", "atlas"] {
                        let prefix = format!("browser:{name}:");
                        let actual: Vec<_> = daily_checks.iter().filter(|(id, _)| id.starts_with(&prefix)).map(|(_, v)| v.clone()).collect();
                        if !actual.is_empty() {
                            let outcome = actual.iter().find(|v| v.result == "failed").cloned().unwrap_or_else(DailySourceCheck::ok);
                            daily_checks.insert(format!("browser:{name}"), outcome);
                        }
                    }
                    if collected.source_reads.is_empty() && !collected.warnings.is_empty() {
                        for name in ["chrome", "atlas"] {
                            if !no_browser && browser.as_ref().map(|b| format!("{b:?}").to_lowercase() == name).unwrap_or(true) {
                                daily_checks.insert(format!("browser:{name}"), DailySourceCheck::failed("read", "browser_collection_failed"));
                            }
                        }
                    }

                    for name in ["chrome", "atlas"] {
                        let id = format!("browser:{name}");
                        let selected = browser.as_ref().map(|b| format!("{b:?}").to_lowercase() == name).unwrap_or(true);
                        roster.insert(id.clone(), crate::archive::coverage::Source { kind: "browser".into(), enabled: selected, discovery: "incremental-discovery-only".into() });
                        for date in targets {
                            let mut observation = crate::archive::coverage::Observation::pending(
                                if !selected { "disabled" } else if summary_result.is_err() || !collected.warnings.is_empty() { "failed" } else { "pending" },
                                if !selected { "browser-not-selected" } else if summary_result.is_err() { "archive-write-failed" } else if !collected.warnings.is_empty() { "browser-read-warning" } else { "incremental-read-is-not-full-day" }, "visits");
                            if selected { if date < today { observation.attempted_ranges = vec![crate::archive::coverage::day_range(date)]; } }
                            observation.archive_status = if summary_result.is_err() { "failed" } else { "unknown" }.into();
                            observations.get_mut(&date.to_string()).unwrap().insert(id.clone(), observation);
                        }
                    }
                    let indexed = match if coverage { read_browser_index_entries(&archive_dir).await } else { Ok(std::collections::BTreeMap::new()) } {
                        Ok(value) => value,
                        Err(error) => { if !coverage && daily_status_file.is_none() { return Err(error); } std::collections::BTreeMap::new() }
                    };
                    for source in &collected.source_reads {
                        roster.insert(source.id.clone(), crate::archive::coverage::Source { kind: "browser".into(), enabled: true, discovery: if source.status == "unknown" { "unavailable" } else { "found" }.into() });
                        for date in targets {
                            let full = crate::archive::coverage::day_range(date);
                            let lower = chrono::DateTime::parse_from_rfc3339(&full.start).unwrap().with_timezone(&chrono::Utc);
                            let upper = chrono::DateTime::parse_from_rfc3339(&full.end_exclusive).unwrap().with_timezone(&chrono::Utc);
                            let start = source.since.map(|s| s.max(lower)).unwrap_or(lower);
                            let end = cycle_now.min(upper);
                            let attempted = source.id.matches(':').count() == 2 && start < end;
                            let mut observation = crate::archive::coverage::Observation::pending(
                                if source.status == "failed" || summary_result.is_err() { "failed" } else if attempted { "partial" } else { "unknown" },
                                if source.status == "failed" { "browser-read-failed" } else if summary_result.is_err() { "archive-write-failed" } else { "incremental-retained-history-only" }, "visits");
                            if attempted && date < today {
                                let range = crate::archive::coverage::Range { start: start.to_rfc3339(), end_exclusive: end.to_rfc3339() };
                                observation.attempted_ranges = vec![range.clone()];
                                if source.status == "success" && summary_result.is_ok() { observation.successful_ranges = vec![range]; }
                            }
                            let count: usize = indexed.values().filter(|e| format!("browser:{}:{}", e.browser, e.profile) == source.id && e.date == date.to_string()).map(|e| e.record_count).sum();
                            if source.status == "success" && summary_result.is_ok() { observation.record_count = Some(count as u64); observation.archive_status = "success".into(); }
                            observations.get_mut(&date.to_string()).unwrap().insert(source.id.clone(), observation);
                        }
                    }
                    let summary = match summary_result {
                        Ok(summary) => summary,
                        Err(error) => { if !coverage && daily_status_file.is_none() { return Err(error); } crate::archive::browser::BrowserArchiveSummary::default() }
                    };
                    for warning in &summary.warnings {
                        output.warn(warning)?;
                    }
                    browser_updated = summary.updated_groups;
                    browser_unchanged = summary.unchanged_groups;
                }

                if let Some(file) = &daily_status_file {
                    for name in ["chrome", "atlas"] {
                        if !(!no_browser && browser.as_ref().map(|b| format!("{b:?}").to_lowercase() == name).unwrap_or(true)) {
                            roster.insert(format!("browser:{name}"), crate::archive::coverage::Source { kind: "browser".into(), enabled: false, discovery: "not-selected".into() });
                        }
                    }
                    crate::archive::daily_status::record_local(file, &device, &chrono::Utc::now().to_rfc3339(), roster.clone(), daily_checks).await?;
                    crate::archive::coverage::save_runtime(archive_lock_file.as_deref().unwrap(), &device, "running", &chrono::Utc::now().to_rfc3339()).await?;
                }
                if coverage {
                    for name in ["chrome", "atlas"] {
                        let id = format!("browser:{name}");
                        if !roster.contains_key(&id) {
                            roster.insert(id.clone(), crate::archive::coverage::Source { kind: "browser".into(), enabled: false, discovery: "disabled-by-config".into() });
                            for day in observations.values_mut() { day.insert(id.clone(), crate::archive::coverage::Observation::pending("disabled", "browser-disabled", "visits")); }
                        }
                    }
                    let version = match crate::archive::coverage::data_version(&archive_dir).await {
                        Ok(version) => Some(version),
                        Err(_) => {
                            for day in observations.values_mut() {
                                for observation in day.values_mut() {
                                    if observation.status != "disabled" {
                                        observation.status = "failed".into();
                                        observation.reason_code = Some("archive-digest-failed".into());
                                        observation.archive_status = "failed".into();
                                        observation.record_count = None;
                                        observation.successful_ranges.clear();
                                    }
                                }
                            }
                            None
                        }
                    };
                    let mut receipt = crate::archive::coverage::read(&archive_dir, &device).await?;
                    for (id, source) in receipt.sources.clone() {
                        if !roster.contains_key(&id) {
                            roster.insert(id.clone(), crate::archive::coverage::Source { kind: source.kind.clone(), enabled: source.enabled, discovery: "not-rediscovered".into() });
                            for day in observations.values_mut() { day.insert(id.clone(), crate::archive::coverage::Observation::pending(if source.enabled { "unknown" } else { "disabled" }, "source-not-rediscovered", if source.kind == "browser" { "visits" } else { "messages" })); }
                        }
                    }
                    let days = observations.into_iter().map(|(date, sources)| (date, crate::archive::coverage::Day { data_version: version.clone(), sources })).collect();
                    receipt.merge(&device, today, roster, days)?;
                    crate::archive::coverage::save_locked(&archive_dir, &receipt).await?;
                    crate::archive::coverage::save_runtime(archive_lock_file.as_deref().unwrap(), &device, "running", &cycle_now.to_rfc3339()).await?;
                }
                if written > 0 || filtered > 0 || browser_updated > 0 {
                    output.info(format!(
                        "Watch sync complete: {} updated, {} unchanged, {} filtered, {} browser groups updated, {} browser groups unchanged",
                        written, unchanged, filtered, browser_updated, browser_unchanged
                    ))?;
                }
            }
            _ = &mut stop => {
                if daily_status_file.is_some() {
                    crate::archive::coverage::save_runtime(archive_lock_file.as_deref().unwrap(), &device, "stopped", &chrono::Utc::now().to_rfc3339()).await?;
                }
                if coverage {
                    let now = chrono::Utc::now();
                    crate::archive::coverage::save_runtime(archive_lock_file.as_deref().unwrap(), &device, "stopped", &now.to_rfc3339()).await?;
                    if let Some(_guard) = try_begin_watch_cycle(archive_lock_file.as_deref(), git_dir.as_deref())? {
                        let today = now.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap()).date_naive();
                        crate::archive::coverage::mark_stopped_locked(&archive_dir, &device, today).await?;
                    }
                }
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
