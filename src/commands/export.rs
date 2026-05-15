use crate::archive::browser::BrowserArchiveWriter;
use crate::archive::{
    latest_browser_visit_per_source_profile, read_browser_index_entries, ArchiveWriter,
};
use crate::browser::chrome::ChromiumHistoryCollector;
use crate::cli::Browser;
use crate::providers::base::ChatSession;
use crate::error::{Result, WaylogError};
use crate::output::Output;
use crate::providers;
use crate::utils::path;
use chrono::{DateTime, Duration, Local, LocalResult, NaiveDate, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
struct TimeRangeFilter {
    updated_from: Option<DateTime<Utc>>,
    updated_to: Option<DateTime<Utc>>,
}

pub async fn handle_export(
    provider_name: Option<String>,
    date: Option<String>,
    from: Option<String>,
    to: Option<String>,
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

    let time_range = TimeRangeFilter::parse(date.as_deref(), from.as_deref(), to.as_deref())?;

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
            export_provider_sessions(&writer, provider.as_ref(), time_range.as_ref()).await?;
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
        let collected = collector.collect_visits_since(&since_by_profile).await?;
        for warning in &collected.warnings {
            output.warn(warning)?;
        }
        let summary = browser_writer.export_visits(&collected.visits, force).await?;
        for warning in &summary.warnings {
            output.warn(warning)?;
        }
        browser_summary = Some(summary);
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
    use super::{browser_enabled, session_matches_time_range, TimeRangeFilter};
    use crate::cli::Browser;
    use crate::providers::base::{ChatMessage, ChatSession, MessageMetadata, MessageRole};
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;

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

    #[test]
    fn test_time_range_filter_parses_single_date() {
        let filter = TimeRangeFilter::parse(Some("2026-05-12"), None, None)
            .unwrap()
            .unwrap();

        assert_eq!(
            filter.updated_from,
            Some(Utc.with_ymd_and_hms(2026, 5, 11, 16, 0, 0).unwrap())
        );
        assert_eq!(
            filter.updated_to,
            Some(Utc.with_ymd_and_hms(2026, 5, 12, 15, 59, 59).unwrap())
        );
    }

    #[test]
    fn test_time_range_filter_parses_open_ended_range() {
        let filter = TimeRangeFilter::parse(None, Some("2026-05-12"), Some("2026-05-13"))
            .unwrap()
            .unwrap();

        assert_eq!(
            filter.updated_from,
            Some(Utc.with_ymd_and_hms(2026, 5, 11, 16, 0, 0).unwrap())
        );
        assert_eq!(
            filter.updated_to,
            Some(Utc.with_ymd_and_hms(2026, 5, 13, 15, 59, 59).unwrap())
        );
    }

    #[test]
    fn test_time_range_filter_rejects_inverted_range() {
        let error = TimeRangeFilter::parse(None, Some("2026-05-13"), Some("2026-05-12"))
            .unwrap_err()
            .to_string();

        assert!(error.contains("must be earlier"));
    }

    #[test]
    fn test_session_matches_time_range_by_updated_at() {
        let filter = TimeRangeFilter::parse(Some("2026-05-12"), None, None)
            .unwrap()
            .unwrap();
        let session = test_session_at(
            Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 12, 8, 0, 0).unwrap(),
        );

        assert!(session_matches_time_range(&session, Some(&filter)));
    }

    #[test]
    fn test_session_outside_time_range_is_filtered_out() {
        let filter = TimeRangeFilter::parse(Some("2026-05-12"), None, None)
            .unwrap()
            .unwrap();
        let session = test_session_at(
            Utc.with_ymd_and_hms(2026, 5, 10, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 5, 10, 8, 0, 0).unwrap(),
        );

        assert!(!session_matches_time_range(&session, Some(&filter)));
    }

    fn test_session_at(started_at: chrono::DateTime<Utc>, updated_at: chrono::DateTime<Utc>) -> ChatSession {
        ChatSession {
            session_id: "session-1".to_string(),
            provider: "codex".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            started_at,
            updated_at,
            messages: vec![ChatMessage {
                id: "m1".to_string(),
                timestamp: started_at,
                role: MessageRole::User,
                content: "hello".to_string(),
                metadata: MessageMetadata::default(),
            }],
        }
    }
}

async fn export_provider_sessions(
    writer: &ArchiveWriter,
    provider: &dyn crate::providers::base::Provider,
    time_range: Option<&TimeRangeFilter>,
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
        if !session_matches_time_range(&session, time_range) {
            filtered += 1;
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

fn session_matches_time_range(session: &ChatSession, time_range: Option<&TimeRangeFilter>) -> bool {
    let Some(time_range) = time_range else {
        return true;
    };

    if let Some(updated_from) = time_range.updated_from {
        if session.updated_at < updated_from {
            return false;
        }
    }

    if let Some(updated_to) = time_range.updated_to {
        if session.updated_at > updated_to {
            return false;
        }
    }

    true
}

impl TimeRangeFilter {
    fn parse(date: Option<&str>, from: Option<&str>, to: Option<&str>) -> Result<Option<Self>> {
        if date.is_none() && from.is_none() && to.is_none() {
            return Ok(None);
        }

        let (updated_from, updated_to) = if let Some(date) = date {
            (
                Some(parse_time_boundary(date, BoundaryKind::Start)?),
                Some(parse_time_boundary(date, BoundaryKind::End)?),
            )
        } else {
            (
                from.map(|value| parse_time_boundary(value, BoundaryKind::Start)).transpose()?,
                to.map(|value| parse_time_boundary(value, BoundaryKind::End)).transpose()?,
            )
        };

        if let (Some(updated_from), Some(updated_to)) = (updated_from, updated_to) {
            if updated_from > updated_to {
                return Err(WaylogError::InvalidTimeRange(
                    "`from` must be earlier than or equal to `to`".to_string(),
                ));
            }
        }

        Ok(Some(Self {
            updated_from,
            updated_to,
        }))
    }
}

#[derive(Clone, Copy)]
enum BoundaryKind {
    Start,
    End,
}

fn parse_time_boundary(input: &str, kind: BoundaryKind) -> Result<DateTime<Utc>> {
    if let Ok(value) = DateTime::parse_from_rfc3339(input) {
        return Ok(value.with_timezone(&Utc));
    }

    let date = NaiveDate::parse_from_str(input, "%Y-%m-%d").map_err(|_| {
        WaylogError::InvalidTimeRange(format!(
            "unsupported time value `{}`; use RFC3339 or YYYY-MM-DD",
            input
        ))
    })?;

    let naive = match kind {
        BoundaryKind::Start => date.and_hms_opt(0, 0, 0),
        BoundaryKind::End => date.and_hms_opt(23, 59, 59),
    }
    .ok_or_else(|| WaylogError::InvalidTimeRange(format!("invalid local date `{}`", input)))?;

    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, second) => {
            let chosen = match kind {
                BoundaryKind::Start => first,
                BoundaryKind::End => second,
            };
            Ok(chosen.with_timezone(&Utc))
        }
        LocalResult::None => {
            let adjusted = match kind {
                BoundaryKind::Start => naive + Duration::hours(1),
                BoundaryKind::End => naive - Duration::hours(1),
            };
            Local
                .from_local_datetime(&adjusted)
                .earliest()
                .map(|value| value.with_timezone(&Utc))
                .ok_or_else(|| {
                    WaylogError::InvalidTimeRange(format!(
                        "could not resolve local date `{}` in current timezone",
                        input
                    ))
                })
        }
    }
}
