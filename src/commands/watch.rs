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
use std::time::Duration;
use tokio::time;

const WATCH_INTERVAL_SECS: u64 = 30;

pub async fn handle_watch(
    provider_name: Option<String>,
    browser: Option<Browser>,
    no_browser: bool,
    archive_dir: Option<std::path::PathBuf>,
    output: &mut Output,
) -> Result<()> {
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
    path::ensure_dir_exists(&archive_dir)?;
    let writer = ArchiveWriter::new(archive_dir.clone());
    let browser_writer = BrowserArchiveWriter::new(archive_dir.clone());

    output.info(format!(
        "Watching local session data and syncing to {} every {} seconds",
        archive_dir.display(),
        WATCH_INTERVAL_SECS
    ))?;
    output.info("Press Ctrl+C to stop watching.")?;

    let watched_providers = if let Some(name) = provider_name {
        vec![providers::get_provider(&name)?]
    } else {
        providers::all_providers()
    };

    let mut interval = time::interval(Duration::from_secs(WATCH_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = interval.tick() => {
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
                    let visits = collector.collect_visits_since(&since_by_profile).await?;
                    let summary = browser_writer.export_visits(&visits, false).await?;
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
            _ = tokio::signal::ctrl_c() => {
                output.info("Stopped watching local session data.")?;
                break;
            }
        }
    }

    Ok(())
}

fn browser_enabled(browser: Option<&Browser>, no_browser: bool) -> bool {
    !no_browser && matches!(browser, None | Some(Browser::Chrome) | Some(Browser::Atlas))
}

#[cfg(test)]
mod tests {
    use super::browser_enabled;
    use crate::cli::Browser;

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
