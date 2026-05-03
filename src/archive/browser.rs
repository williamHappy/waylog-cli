use crate::archive::meta::BrowserHistoryIndexEntry;
use crate::archive::{
    ensure_archive_dirs, read_browser_index_entries, refresh_manifest, write_browser_index_entries,
};
use crate::browser::BrowserVisitRecord;
use crate::error::{Result, WaylogError};
use chrono::{Local, Utc};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub struct BrowserArchiveWriter {
    archive_dir: PathBuf,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BrowserArchiveSummary {
    pub updated_groups: usize,
    pub unchanged_groups: usize,
    pub written_records: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserArchivePaths {
    markdown_path: PathBuf,
    raw_path: PathBuf,
}

impl BrowserArchiveWriter {
    pub fn new(archive_dir: PathBuf) -> Self {
        Self { archive_dir }
    }

    pub async fn export_visits(
        &self,
        visits: &[BrowserVisitRecord],
        force: bool,
    ) -> Result<BrowserArchiveSummary> {
        if visits.is_empty() {
            return Ok(BrowserArchiveSummary::default());
        }

        ensure_archive_dirs(&self.archive_dir).await?;
        fs::create_dir_all(self.archive_dir.join("browser-history")).await?;

        let grouped = group_visits(visits)?;
        let mut index_entries = read_browser_index_entries(&self.archive_dir).await?;
        let mut summary = BrowserArchiveSummary::default();

        for ((browser, profile, date), new_records) in grouped {
            let stable_key = format!("{browser}:{profile}:{date}");
            let paths = browser_history_paths(&self.archive_dir, &date, &browser, &profile);

            let existing_records = read_existing_records(&paths.raw_path).await?;
            let existing_ids = existing_records
                .iter()
                .map(|record| record.record_id.clone())
                .collect::<HashSet<_>>();

            let deduped_new = dedupe_records(new_records)
                .into_iter()
                .filter(|record| force || !existing_ids.contains(&record.record_id))
                .collect::<Vec<_>>();

            if deduped_new.is_empty() && !force {
                summary.unchanged_groups += 1;
                continue;
            }

            let combined_records = if force {
                dedupe_records(
                    existing_records
                        .into_iter()
                        .chain(deduped_new.clone())
                        .collect(),
                )
            } else {
                existing_records
                    .into_iter()
                    .chain(deduped_new.clone())
                    .collect::<Vec<_>>()
            };

            let combined_records = dedupe_records(combined_records);
            let rewrite_raw = force || !paths.raw_path.exists();
            let raw_records = if rewrite_raw {
                combined_records.as_slice()
            } else {
                deduped_new.as_slice()
            };
            write_raw_records(&paths.raw_path, raw_records, rewrite_raw).await?;
            write_markdown(
                &paths.markdown_path,
                &browser,
                &profile,
                &date,
                &combined_records,
            )
            .await?;

            let latest_visit_at = combined_records
                .iter()
                .filter_map(|record| record.visited_at_utc())
                .max()
                .map(|dt| crate::utils::time::format_local_rfc3339(&dt))
                .unwrap_or_default();
            let source_path = combined_records
                .last()
                .map(|record| record.source_db_path.clone())
                .unwrap_or_default();
            let source_mtime = source_mtime(Path::new(&source_path), &latest_visit_at);

            index_entries.insert(
                stable_key.clone(),
                BrowserHistoryIndexEntry {
                    stable_key,
                    browser: browser.clone(),
                    profile: profile.clone(),
                    date: date.clone(),
                    record_count: combined_records.len(),
                    latest_visit_at,
                    source_path,
                    source_mtime,
                    markdown_path: paths.markdown_path.display().to_string(),
                    raw_path: paths.raw_path.display().to_string(),
                    exported_at: crate::utils::time::format_local_rfc3339(&Utc::now()),
                },
            );

            summary.updated_groups += 1;
            summary.written_records += deduped_new.len();
        }

        write_browser_index_entries(&self.archive_dir, &index_entries).await?;
        refresh_manifest(&self.archive_dir, None).await?;
        Ok(summary)
    }
}

fn group_visits(
    visits: &[BrowserVisitRecord],
) -> Result<BTreeMap<(String, String, String), Vec<BrowserVisitRecord>>> {
    let mut grouped = BTreeMap::new();
    for record in visits {
        let date = record
            .local_date_key()
            .ok_or_else(|| WaylogError::Internal("Invalid browser visit timestamp".to_string()))?;
        grouped
            .entry((record.browser.clone(), record.profile.clone(), date))
            .or_insert_with(Vec::new)
            .push(record.clone());
    }
    Ok(grouped)
}

fn dedupe_records(records: Vec<BrowserVisitRecord>) -> Vec<BrowserVisitRecord> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for record in records {
        if seen.insert(record.record_id.clone()) {
            deduped.push(record);
        }
    }

    deduped.sort_by(|left, right| left.visited_at.cmp(&right.visited_at));
    deduped
}

async fn read_existing_records(raw_path: &Path) -> Result<Vec<BrowserVisitRecord>> {
    if !raw_path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(raw_path).await?;
    let mut records = Vec::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        records.push(serde_json::from_str(line)?);
    }
    Ok(records)
}

async fn write_raw_records(
    raw_path: &Path,
    records: &[BrowserVisitRecord],
    rewrite: bool,
) -> Result<()> {
    if rewrite {
        let mut content = String::new();
        for record in records {
            content.push_str(&serde_json::to_string(record)?);
            content.push('\n');
        }
        fs::write(raw_path, content).await?;
        return Ok(());
    }

    let mut file = fs::OpenOptions::new().append(true).open(raw_path).await?;
    for record in records {
        file.write_all(serde_json::to_string(record)?.as_bytes())
            .await?;
        file.write_all(b"\n").await?;
    }
    Ok(())
}

async fn write_markdown(
    markdown_path: &Path,
    browser: &str,
    profile: &str,
    date: &str,
    records: &[BrowserVisitRecord],
) -> Result<()> {
    let mut ordered = records.to_vec();
    ordered.sort_by(|left, right| right.visited_at.cmp(&left.visited_at));

    let mut content = String::new();
    content.push_str(&format!(
        "# {} {} history for {}\n\n",
        browser, profile, date
    ));

    for record in ordered {
        let time_label = record
            .visited_at_utc()
            .map(|dt| dt.with_timezone(&Local).format("%H:%M:%S").to_string())
            .unwrap_or_else(|| record.visited_at.clone());

        content.push_str(&format!(
            "- {} | {} | {}\n",
            time_label,
            if record.title.is_empty() {
                "(untitled)"
            } else {
                record.title.as_str()
            },
            record.url
        ));
        if record.visit_count > 0 || record.typed_count > 0 {
            content.push_str(&format!(
                "  visit_count={} typed_count={}\n",
                record.visit_count, record.typed_count
            ));
        }
    }

    fs::write(markdown_path, content).await?;
    Ok(())
}

fn browser_history_paths(
    archive_dir: &Path,
    date: &str,
    browser: &str,
    profile: &str,
) -> BrowserArchivePaths {
    let profile_slug = profile
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let base = format!("{date}_{browser}_{profile_slug}");
    let browser_dir = archive_dir.join("browser-history");

    BrowserArchivePaths {
        markdown_path: browser_dir.join(format!("{base}.md")),
        raw_path: browser_dir.join(format!("{base}.raw.jsonl")),
    }
}

fn source_mtime(source_path: &Path, fallback: &str) -> String {
    std::fs::metadata(source_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(crate::utils::time::format_system_time_as_local_rfc3339)
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn visit(record_id: &str, profile: &str, visited_at: &str) -> BrowserVisitRecord {
        BrowserVisitRecord {
            record_id: record_id.to_string(),
            browser: "chrome".to_string(),
            profile: profile.to_string(),
            url: format!("https://example.com/{record_id}"),
            title: format!("Visit {record_id}"),
            visited_at: visited_at.to_string(),
            visit_count: 1,
            typed_count: 0,
            transition: Some("0".to_string()),
            referrer_visit_id: None,
            source_db_path: "/tmp/History".to_string(),
        }
    }

    #[tokio::test]
    async fn test_browser_archive_writer_creates_files_and_index() {
        let temp_dir = TempDir::new().unwrap();
        let writer = BrowserArchiveWriter::new(temp_dir.path().to_path_buf());
        let summary = writer
            .export_visits(&[visit("1", "Default", "2026-05-03T10:00:00+08:00")], false)
            .await
            .unwrap();

        assert_eq!(summary.updated_groups, 1);
        assert!(temp_dir
            .path()
            .join("browser-history")
            .join("2026-05-03_chrome_default.md")
            .exists());
        assert!(temp_dir
            .path()
            .join("indexes")
            .join("browser-history.jsonl")
            .exists());
    }

    #[tokio::test]
    async fn test_browser_archive_writer_skips_duplicate_record_on_second_export() {
        let temp_dir = TempDir::new().unwrap();
        let writer = BrowserArchiveWriter::new(temp_dir.path().to_path_buf());
        let visits = vec![visit("1", "Default", "2026-05-03T10:00:00+08:00")];

        writer.export_visits(&visits, false).await.unwrap();
        let summary = writer.export_visits(&visits, false).await.unwrap();

        assert_eq!(summary.updated_groups, 0);
        assert_eq!(summary.unchanged_groups, 1);
    }

    #[tokio::test]
    async fn test_browser_archive_writer_appends_new_visit() {
        let temp_dir = TempDir::new().unwrap();
        let writer = BrowserArchiveWriter::new(temp_dir.path().to_path_buf());

        writer
            .export_visits(&[visit("1", "Default", "2026-05-03T10:00:00+08:00")], false)
            .await
            .unwrap();
        let summary = writer
            .export_visits(&[visit("2", "Default", "2026-05-03T11:00:00+08:00")], false)
            .await
            .unwrap();

        assert_eq!(summary.updated_groups, 1);
        assert_eq!(summary.written_records, 1);
        let raw = fs::read_to_string(
            temp_dir
                .path()
                .join("browser-history")
                .join("2026-05-03_chrome_default.raw.jsonl"),
        )
        .await
        .unwrap();
        assert_eq!(raw.lines().count(), 2);
    }
}
