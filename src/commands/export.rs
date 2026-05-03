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

pub async fn handle_export(
    provider_name: Option<String>,
    browser: Option<Browser>,
    no_browser: bool,
    archive_dir: Option<std::path::PathBuf>,
    force: bool,
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
    let writer = ArchiveWriter::new(archive_dir.clone());
    output.info(format!(
        "Exporting chat history to {}",
        archive_dir.display()
    ))?;

    let providers_to_export = if let Some(name) = provider_name {
        vec![providers::get_provider(&name)?]
    } else {
        providers::all_providers()
    };

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut filtered = 0usize;

    for provider in providers_to_export {
        let (provider_written, provider_skipped, provider_filtered) =
            export_provider_sessions(&writer, provider.as_ref()).await?;
        written += provider_written;
        skipped += provider_skipped;
        filtered += provider_filtered;
    }

    let mut browser_summary = None;
    if browser_enabled(browser.as_ref(), no_browser) {
        let browser_writer = BrowserArchiveWriter::new(archive_dir.clone());
        let collector = ChromiumHistoryCollector::new(browser.as_ref())?;
        let since_by_profile = if force {
            std::collections::HashMap::new()
        } else {
            let existing_entries = read_browser_index_entries(&archive_dir).await?;
            latest_browser_visit_per_source_profile(&existing_entries)
        };
        let visits = collector.collect_visits_since(&since_by_profile).await?;
        browser_summary = Some(browser_writer.export_visits(&visits, force).await?);
    }

    let browser_status = browser_summary
        .map(|summary| {
            format!(
                ", {} browser groups updated, {} browser groups unchanged, {} browser visits written",
                summary.updated_groups, summary.unchanged_groups, summary.written_records
            )
        })
        .unwrap_or_default();

    output.info(format!(
        "Archive export complete: {} written, {} unchanged, {} filtered{}",
        written, skipped, filtered, browser_status
    ))?;
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

async fn export_provider_sessions(
    writer: &ArchiveWriter,
    provider: &dyn crate::providers::base::Provider,
) -> Result<(usize, usize, usize)> {
    if !provider.has_local_data() {
        return Ok((0, 0, 0));
    }

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut filtered = 0usize;

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
                skipped += 1;
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
                skipped += 1;
                continue;
            }
        };

        if let Some(reason) = result.filtered_reason {
            tracing::info!(
                "Filtered {} session {} from archive: {}",
                provider.name(),
                session_path.display(),
                reason
            );
            filtered += 1;
        } else if result.written {
            written += 1;
        } else {
            skipped += 1;
        }
    }

    Ok((written, skipped, filtered))
}
