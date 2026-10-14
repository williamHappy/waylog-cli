pub mod browser;
pub mod coverage;
pub mod layout;
pub mod meta;

use crate::error::Result;
use crate::providers::base::ChatSession;
use crate::session_filter;
use crate::utils::time;
use chrono::{DateTime, Utc};
use meta::{BrowserHistoryIndexEntry, SessionIndexEntry};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::fs;

pub struct ArchiveWriter {
    archive_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ArchiveExportResult {
    #[cfg_attr(not(test), allow(dead_code))]
    pub paths: layout::ArchivePaths,
    pub written: bool,
    pub filtered_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct ArchiveManifest {
    updated_at: String,
    session_count: usize,
    providers: Vec<String>,
    browser_sources: Vec<String>,
    browser_record_count: usize,
    archive_version: u32,
    latest_session: String,
}

impl ArchiveWriter {
    pub fn new(archive_dir: PathBuf) -> Self {
        Self { archive_dir }
    }

    pub async fn export_session(
        &self,
        session: &ChatSession,
        source_path: &Path,
        raw_extension: &str,
    ) -> Result<ArchiveExportResult> {
        let title = session_filter::session_title(session);
        let stable_key = format!("{}:{}", session.provider, session.session_id);
        let suffix = format!("{:x}", Sha256::digest(stable_key.as_bytes()));
        let readable_base = layout::build_readable_base(
            &session.started_at.to_rfc3339(),
            &session.provider,
            &title,
            Some(&suffix[..12]),
        );
        let mut paths = layout::archive_paths(&self.archive_dir, &readable_base, raw_extension);

        if let Some(reason) = session_filter::archive_skip_reason(session) {
            return Ok(ArchiveExportResult {
                paths,
                written: false,
                filtered_reason: Some(reason.to_string()),
            });
        }

        ensure_archive_dirs(&self.archive_dir).await?;

        let metadata = fs::metadata(source_path).await?;
        let source_size = metadata.len();
        let source_mtime = metadata
            .modified()
            .ok()
            .map(crate::utils::time::format_system_time_as_local_rfc3339)
            .unwrap_or_else(|| crate::utils::time::format_local_rfc3339(&session.updated_at));

        let markdown = crate::exporter::generate_markdown(session);
        let mut index_entries = read_session_index_entries(&self.archive_dir).await?;
        let codex_source = if session.provider == "codex" {
            crate::providers::codex::read_source_kind(source_path).await?
        } else {
            None
        };
        if let Some(existing_entry) = index_entries.get(&stable_key) {
            let shared_path = index_entries.values().any(|entry| {
                entry.stable_key != stable_key
                    && (entry.markdown_path == existing_entry.markdown_path
                        || entry.raw_path == existing_entry.raw_path)
            });
            let old_md = PathBuf::from(&existing_entry.markdown_path);
            let old_raw = PathBuf::from(&existing_entry.raw_path);
            let sessions_dir = self.archive_dir.join("sessions");
            // Freeze a healthy existing path; only rename changed titles, collisions or relocated paths.
            if !shared_path
                && existing_entry.title == title
                && old_md.parent() == Some(sessions_dir.as_path())
                && old_raw.parent() == Some(sessions_dir.as_path())
            {
                paths = layout::ArchivePaths {
                    markdown_path: old_md.clone(),
                    raw_path: old_raw.clone(),
                };
            }
            let renamed = paths.markdown_path != old_md || paths.raw_path != old_raw;
            // 同一个逻辑会话被 resume/fork 后会生成多个源文件，且它们解析出的
            // session_id 相同（如 Claude `--resume`），从而碰撞到同一个 stable_key。
            // 若放任每个文件都覆盖索引项，watch 每个周期都会反复重写、刷新
            // exported_at，导致 sessions.jsonl 与 manifest.json 无谓 churn。
            // 去重规则：同一源文件仅在内容真正变化时重写；不同源文件只有在“更完整”
            // （消息更多）时才接管，否则跳过——以此为每个会话确定唯一赢家。
            let same_source = existing_entry.source_path == source_path.display().to_string();
            let should_write = if renamed || !paths.raw_path.exists() {
                true
            } else if same_source {
                same_source_session_changed(existing_entry, &markdown, session.messages.len())
                    .await?
            } else {
                session.messages.len() > existing_entry.message_count
            };
            if !should_write {
                if existing_entry.codex_source != codex_source {
                    let mut updated = existing_entry.clone();
                    updated.codex_source = codex_source;
                    index_entries.insert(stable_key.clone(), updated);
                    write_session_index_entries(&self.archive_dir, &index_entries).await?;
                }
                remove_legacy_meta_file(&paths.markdown_path).await?;
                return Ok(ArchiveExportResult {
                    paths,
                    written: false,
                    filtered_reason: None,
                });
            }
        }

        fs::write(&paths.markdown_path, markdown).await?;
        fs::copy(source_path, &paths.raw_path).await?;

        remove_legacy_meta_file(&paths.markdown_path).await?;

        let entry = SessionIndexEntry {
            stable_key: stable_key.clone(),
            provider: session.provider.clone(),
            session_id: session.session_id.clone(),
            title: title.clone(),
            started_at: crate::utils::time::format_local_rfc3339(&session.started_at),
            project_path: session.project_path.display().to_string(),
            source_path: source_path.display().to_string(),
            source_mtime,
            source_size,
            message_count: session.messages.len(),
            markdown_path: paths.markdown_path.display().to_string(),
            raw_path: paths.raw_path.display().to_string(),
            exported_at: crate::utils::time::format_local_rfc3339(&Utc::now()),
            codex_source,
        };

        index_entries.insert(stable_key.clone(), entry);
        write_session_index_entries(&self.archive_dir, &index_entries).await?;
        refresh_manifest(&self.archive_dir, Some(stable_key)).await?;

        Ok(ArchiveExportResult {
            paths,
            written: true,
            filtered_reason: None,
        })
    }
}

async fn ensure_archive_dirs(archive_dir: &Path) -> Result<()> {
    fs::create_dir_all(archive_dir.join("sessions")).await?;
    fs::create_dir_all(archive_dir.join("indexes")).await?;
    Ok(())
}

async fn remove_legacy_meta_file(markdown_path: &Path) -> Result<()> {
    let legacy_meta_path = markdown_path.with_extension("meta.json");
    if legacy_meta_path.exists() {
        fs::remove_file(legacy_meta_path).await?;
    }
    Ok(())
}

fn parse_jsonl_entries<T>(content: &str, label: &str) -> Vec<T>
where
    T: DeserializeOwned,
{
    content
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }

            match serde_json::from_str(trimmed) {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::warn!(
                        "Skipping malformed {label} JSONL line {}: {}",
                        index + 1,
                        error
                    );
                    None
                }
            }
        })
        .collect()
}

pub(crate) async fn read_session_index_entries(
    archive_dir: &Path,
) -> Result<BTreeMap<String, SessionIndexEntry>> {
    let index_path = archive_dir.join("indexes").join("sessions.jsonl");
    if !index_path.exists() {
        return Ok(BTreeMap::new());
    }

    let content = fs::read_to_string(index_path).await?;
    let mut entries = BTreeMap::new();
    for entry in parse_jsonl_entries::<SessionIndexEntry>(&content, "session index") {
        entries.insert(entry.stable_key.clone(), entry);
    }

    Ok(entries)
}

pub(crate) async fn write_session_index_entries(
    archive_dir: &Path,
    entries: &BTreeMap<String, SessionIndexEntry>,
) -> Result<()> {
    let index_path = archive_dir.join("indexes").join("sessions.jsonl");
    let mut lines = Vec::with_capacity(entries.len());
    for entry in entries.values() {
        lines.push(serde_json::to_string(entry)?);
    }
    let mut content = lines.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    fs::write(index_path, content).await?;
    Ok(())
}

pub(crate) async fn read_browser_index_entries(
    archive_dir: &Path,
) -> Result<BTreeMap<String, BrowserHistoryIndexEntry>> {
    let index_path = archive_dir.join("indexes").join("browser-history.jsonl");
    if !index_path.exists() {
        return Ok(BTreeMap::new());
    }

    let content = fs::read_to_string(index_path).await?;
    let mut entries = BTreeMap::new();
    for entry in parse_jsonl_entries::<BrowserHistoryIndexEntry>(&content, "browser index") {
        entries.insert(entry.stable_key.clone(), entry);
    }

    Ok(entries)
}

pub(crate) async fn write_browser_index_entries(
    archive_dir: &Path,
    entries: &BTreeMap<String, BrowserHistoryIndexEntry>,
) -> Result<()> {
    let index_path = archive_dir.join("indexes").join("browser-history.jsonl");
    let mut lines = Vec::with_capacity(entries.len());
    for entry in entries.values() {
        lines.push(serde_json::to_string(entry)?);
    }
    let mut content = lines.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    fs::write(index_path, content).await?;
    Ok(())
}

pub(crate) async fn refresh_manifest(
    archive_dir: &Path,
    latest_session_override: Option<String>,
) -> Result<()> {
    let session_entries = read_session_index_entries(archive_dir).await?;
    let browser_entries = read_browser_index_entries(archive_dir).await?;

    let mut providers = session_entries
        .values()
        .map(|entry| entry.provider.clone())
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();

    let mut browser_sources = browser_entries
        .values()
        .map(|entry| entry.browser.clone())
        .collect::<Vec<_>>();
    browser_sources.sort();
    browser_sources.dedup();

    let browser_record_count = browser_entries
        .values()
        .map(|entry| entry.record_count)
        .sum::<usize>();

    let latest_session = latest_session_override
        .or_else(|| latest_session_key(&session_entries))
        .unwrap_or_default();

    let manifest = ArchiveManifest {
        updated_at: time::format_local_rfc3339(&Utc::now()),
        session_count: session_entries.len(),
        providers,
        browser_sources,
        browser_record_count,
        archive_version: 3,
        latest_session,
    };
    let manifest_path = archive_dir.join("indexes").join("manifest.json");
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest)?).await?;
    Ok(())
}

pub(crate) fn latest_browser_visit_per_source_profile(
    entries: &BTreeMap<String, BrowserHistoryIndexEntry>,
) -> std::collections::HashMap<String, DateTime<Utc>> {
    let mut latest_by_profile = std::collections::HashMap::new();

    for entry in entries.values() {
        let Ok(visited_at) = DateTime::parse_from_rfc3339(&entry.latest_visit_at) else {
            continue;
        };
        let visited_at = visited_at.with_timezone(&Utc);
        latest_by_profile
            .entry(format!("{}:{}", entry.browser, entry.profile))
            .and_modify(|current| {
                if visited_at > *current {
                    *current = visited_at;
                }
            })
            .or_insert(visited_at);
    }

    latest_by_profile
}

fn latest_session_key(entries: &BTreeMap<String, SessionIndexEntry>) -> Option<String> {
    entries
        .values()
        .max_by(|left, right| {
            let left_started_at = DateTime::parse_from_rfc3339(&left.started_at).ok();
            let right_started_at = DateTime::parse_from_rfc3339(&right.started_at).ok();
            match (left_started_at, right_started_at) {
                (Some(left_started_at), Some(right_started_at)) => {
                    left_started_at.cmp(&right_started_at)
                }
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (None, None) => left.stable_key.cmp(&right.stable_key),
            }
        })
        .map(|entry| entry.stable_key.clone())
}

async fn same_source_session_changed(
    existing_entry: &SessionIndexEntry,
    markdown: &str,
    message_count: usize,
) -> Result<bool> {
    if existing_entry.message_count != message_count {
        return Ok(true);
    }

    let markdown_path = PathBuf::from(&existing_entry.markdown_path);
    if !markdown_path.exists() {
        return Ok(true);
    }

    let existing_markdown = fs::read_to_string(markdown_path).await?;
    Ok(existing_markdown != markdown)
}

#[cfg(test)]
mod tests {
    use super::{latest_session_key, ArchiveWriter};
    use crate::archive::meta::SessionIndexEntry;
    use crate::providers::base::{ChatMessage, ChatSession, MessageMetadata, MessageRole};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::TempDir;
    use tokio::fs;

    fn test_session() -> ChatSession {
        let now = Utc::now();
        ChatSession {
            session_id: "session-1".to_string(),
            provider: "codex".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            started_at: now,
            updated_at: now,
            messages: vec![
                ChatMessage {
                    id: "1".to_string(),
                    timestamp: now,
                    role: MessageRole::User,
                    content: "设计统一归档目录结构".to_string(),
                    metadata: MessageMetadata::default(),
                },
                ChatMessage {
                    id: "2".to_string(),
                    timestamp: now,
                    role: MessageRole::Assistant,
                    content: "这里是方案".to_string(),
                    metadata: MessageMetadata::default(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn different_ids_same_title_and_time_never_share_a_file() {
        let tmp = TempDir::new().unwrap();
        let writer = ArchiveWriter::new(tmp.path().join("archive"));
        let mut a = test_session();
        a.provider = "claude".into();
        a.session_id = "one".into();
        let mut b = a.clone();
        b.session_id = "two".into();
        b.messages[1].content = "第二段独立会话的回答".into();
        let source_a = tmp.path().join("one.jsonl");
        let source_b = tmp.path().join("two.jsonl");
        fs::write(&source_a, "first source").await.unwrap();
        fs::write(&source_b, "second source").await.unwrap();
        let first = writer.export_session(&a, &source_a, "jsonl").await.unwrap();
        let second = writer.export_session(&b, &source_b, "jsonl").await.unwrap();
        assert_ne!(first.paths.markdown_path, second.paths.markdown_path);
        assert_eq!(
            fs::read_to_string(&first.paths.raw_path).await.unwrap(),
            "first source"
        );
        assert_eq!(
            fs::read_to_string(&second.paths.raw_path).await.unwrap(),
            "second source"
        );
        assert!(
            !writer
                .export_session(&a, &source_a, "jsonl")
                .await
                .unwrap()
                .written
        );
        assert!(
            !writer
                .export_session(&b, &source_b, "jsonl")
                .await
                .unwrap()
                .written
        );
        // Simulate an old index where two IDs referenced the same overwritten files.
        let mut legacy = super::read_session_index_entries(&writer.archive_dir)
            .await
            .unwrap();
        let first_entry = legacy.get("claude:one").unwrap().clone();
        let second_entry = legacy.get_mut("claude:two").unwrap();
        second_entry.markdown_path = first_entry.markdown_path;
        second_entry.raw_path = first_entry.raw_path;
        super::write_session_index_entries(&writer.archive_dir, &legacy)
            .await
            .unwrap();
        writer.export_session(&a, &source_a, "jsonl").await.unwrap();
        assert!(
            writer
                .export_session(&b, &source_b, "jsonl")
                .await
                .unwrap()
                .written
        );
        let repaired = super::read_session_index_entries(&writer.archive_dir)
            .await
            .unwrap();
        assert_ne!(
            repaired["claude:one"].raw_path,
            repaired["claude:two"].raw_path
        );
        for (key, expected) in [
            ("claude:one", "first source"),
            ("claude:two", "second source"),
        ] {
            assert_eq!(
                fs::read_to_string(&repaired[key].raw_path).await.unwrap(),
                expected
            );
        }
    }

    /// 构造指定 session_id 与消息数的会话，内容均为实质文本以通过噪声过滤。
    fn session_with_messages(session_id: &str, message_count: usize) -> ChatSession {
        let now = Utc::now();
        let messages = (0..message_count)
            .map(|i| ChatMessage {
                id: format!("m-{}", i),
                timestamp: now,
                role: if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                content: format!("第{}条关于统一归档目录结构的详细讨论内容", i),
                metadata: MessageMetadata::default(),
            })
            .collect();

        ChatSession {
            session_id: session_id.to_string(),
            provider: "claude".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            started_at: now,
            updated_at: now,
            messages,
        }
    }

    #[tokio::test]
    async fn test_archive_writer_creates_markdown_raw_and_index() {
        let temp_dir = TempDir::new().unwrap();
        let raw_path = temp_dir.path().join("source.jsonl");
        tokio::fs::write(&raw_path, "{\"type\":\"response_item\"}\n")
            .await
            .unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let result = writer
            .export_session(&test_session(), &raw_path, "jsonl")
            .await
            .unwrap();

        assert!(result.paths.markdown_path.exists());
        assert!(result.paths.raw_path.exists());
        assert!(temp_dir
            .path()
            .join("indexes")
            .join("sessions.jsonl")
            .exists());
        assert!(temp_dir
            .path()
            .join("indexes")
            .join("manifest.json")
            .exists());
    }

    #[tokio::test]
    async fn test_archive_writer_dedupes_multi_file_session_id_to_most_complete() {
        // 模拟一个会话被 resume 成多个源文件，共享同一 session_id：
        // 大文件(消息多)应稳定胜出，小文件(消息少)被跳过，避免每周期 churn。
        let temp_dir = TempDir::new().unwrap();
        let big_raw = temp_dir.path().join("big.jsonl");
        let small_raw = temp_dir.path().join("small.jsonl");
        tokio::fs::write(&big_raw, "{}\n").await.unwrap();
        tokio::fs::write(&small_raw, "{}\n").await.unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let big = session_with_messages("shared-session", 6);
        let small = session_with_messages("shared-session", 2);

        // 大文件先归档 → 写入
        let r1 = writer
            .export_session(&big, &big_raw, "jsonl")
            .await
            .unwrap();
        assert!(r1.written);

        // 小文件(同 session_id、更少消息) → 跳过，不覆盖
        let r2 = writer
            .export_session(&small, &small_raw, "jsonl")
            .await
            .unwrap();
        assert!(!r2.written);
        assert!(r2.filtered_reason.is_none());

        // 再次处理小文件依旧跳过：验证不会反复重写(无 churn)
        let r3 = writer
            .export_session(&small, &small_raw, "jsonl")
            .await
            .unwrap();
        assert!(!r3.written);

        // 索引仍指向大文件
        let entries = super::read_session_index_entries(temp_dir.path())
            .await
            .unwrap();
        let entry = entries.get("claude:shared-session").unwrap();
        assert_eq!(entry.message_count, 6);
        assert_eq!(entry.source_path, big_raw.display().to_string());
    }

    #[tokio::test]
    async fn test_archive_writer_adopts_more_complete_file_for_same_session_id() {
        // 当出现“更完整”的新文件(消息更多)时，应接管为新的赢家。
        let temp_dir = TempDir::new().unwrap();
        let first_raw = temp_dir.path().join("first.jsonl");
        let bigger_raw = temp_dir.path().join("bigger.jsonl");
        tokio::fs::write(&first_raw, "{}\n").await.unwrap();
        tokio::fs::write(&bigger_raw, "{}\n").await.unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());

        let r1 = writer
            .export_session(
                &session_with_messages("shared-session", 4),
                &first_raw,
                "jsonl",
            )
            .await
            .unwrap();
        assert!(r1.written);

        let r2 = writer
            .export_session(
                &session_with_messages("shared-session", 8),
                &bigger_raw,
                "jsonl",
            )
            .await
            .unwrap();
        assert!(r2.written);

        let entries = super::read_session_index_entries(temp_dir.path())
            .await
            .unwrap();
        let entry = entries.get("claude:shared-session").unwrap();
        assert_eq!(entry.message_count, 8);
        assert_eq!(entry.source_path, bigger_raw.display().to_string());
    }

    #[tokio::test]
    async fn test_archive_writer_skips_unchanged_session_on_second_export() {
        let temp_dir = TempDir::new().unwrap();
        let raw_path = temp_dir.path().join("source.jsonl");
        tokio::fs::write(&raw_path, "{\"type\":\"response_item\"}\n")
            .await
            .unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let session = test_session();
        let first = writer
            .export_session(&session, &raw_path, "jsonl")
            .await
            .unwrap();
        let second = writer
            .export_session(&session, &raw_path, "jsonl")
            .await
            .unwrap();

        assert!(first.written);
        assert!(!second.written);
        assert!(second.filtered_reason.is_none());
    }

    #[tokio::test]
    async fn test_archive_writer_skips_when_raw_metadata_changes_but_markdown_is_same() {
        let temp_dir = TempDir::new().unwrap();
        let raw_path = temp_dir.path().join("source.jsonl");
        tokio::fs::write(&raw_path, "{\"type\":\"response_item\"}\n")
            .await
            .unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let session = test_session();
        let first = writer
            .export_session(&session, &raw_path, "jsonl")
            .await
            .unwrap();
        let entries = super::read_session_index_entries(temp_dir.path())
            .await
            .unwrap();
        let first_entry = entries.get("codex:session-1").unwrap().clone();

        tokio::fs::write(
            &raw_path,
            "{\"type\":\"session_meta\",\"model_provider\":\"openai\"}\n{\"type\":\"response_item\"}\n",
        )
        .await
        .unwrap();

        let second = writer
            .export_session(&session, &raw_path, "jsonl")
            .await
            .unwrap();
        let entries = super::read_session_index_entries(temp_dir.path())
            .await
            .unwrap();
        let second_entry = entries.get("codex:session-1").unwrap();

        assert!(first.written);
        assert!(!second.written);
        assert_eq!(second_entry.source_size, first_entry.source_size);
        assert_eq!(second_entry.exported_at, first_entry.exported_at);
    }

    #[test]
    fn test_latest_session_key_uses_started_at_instead_of_sorted_key() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "codex:rollout-2026-03-10T15-42-52-z".to_string(),
            SessionIndexEntry {
                codex_source: None,
                stable_key: "codex:rollout-2026-03-10T15-42-52-z".to_string(),
                provider: "codex".to_string(),
                session_id: "rollout-2026-03-10T15-42-52-z".to_string(),
                title: "old".to_string(),
                started_at: "2026-03-10T15:42:52+08:00".to_string(),
                project_path: "/tmp/project".to_string(),
                source_path: "/tmp/old.jsonl".to_string(),
                source_mtime: "2026-03-10T15:42:52+08:00".to_string(),
                source_size: 100,
                message_count: 1,
                markdown_path: "sessions/old.md".to_string(),
                raw_path: "sessions/old.raw.jsonl".to_string(),
                exported_at: "2026-03-10T15:42:52+08:00".to_string(),
            },
        );
        entries.insert(
            "codex:rollout-2026-06-24T17-15-55-a".to_string(),
            SessionIndexEntry {
                codex_source: None,
                stable_key: "codex:rollout-2026-06-24T17-15-55-a".to_string(),
                provider: "codex".to_string(),
                session_id: "rollout-2026-06-24T17-15-55-a".to_string(),
                title: "new".to_string(),
                started_at: "2026-06-24T17:15:55+08:00".to_string(),
                project_path: "/tmp/project".to_string(),
                source_path: "/tmp/new.jsonl".to_string(),
                source_mtime: "2026-06-24T17:15:55+08:00".to_string(),
                source_size: 100,
                message_count: 1,
                markdown_path: "sessions/new.md".to_string(),
                raw_path: "sessions/new.raw.jsonl".to_string(),
                exported_at: "2026-06-24T17:15:55+08:00".to_string(),
            },
        );

        assert_eq!(
            latest_session_key(&entries).as_deref(),
            Some("codex:rollout-2026-06-24T17-15-55-a")
        );
    }

    #[tokio::test]
    async fn test_archive_writer_removes_legacy_meta_file() {
        let temp_dir = TempDir::new().unwrap();
        let raw_path = temp_dir.path().join("source.jsonl");
        tokio::fs::write(&raw_path, "{\"type\":\"response_item\"}\n")
            .await
            .unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let result = writer
            .export_session(&test_session(), &raw_path, "jsonl")
            .await
            .unwrap();

        let legacy_meta_path = result.paths.markdown_path.with_extension("meta.json");
        tokio::fs::write(&legacy_meta_path, "{}").await.unwrap();

        writer
            .export_session(&test_session(), &raw_path, "jsonl")
            .await
            .unwrap();

        assert!(!legacy_meta_path.exists());
    }

    #[tokio::test]
    async fn test_archive_writer_filters_noise_sessions() {
        let temp_dir = TempDir::new().unwrap();
        let raw_path = temp_dir.path().join("source.jsonl");
        tokio::fs::write(&raw_path, "{\"type\":\"response_item\"}\n")
            .await
            .unwrap();

        let now = Utc::now();
        let noisy_session = ChatSession {
            session_id: "session-noise".to_string(),
            provider: "codex".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            started_at: now,
            updated_at: now,
            messages: vec![
                ChatMessage {
                    id: "1".to_string(),
                    timestamp: now,
                    role: MessageRole::User,
                    content: "hi".to_string(),
                    metadata: MessageMetadata::default(),
                },
                ChatMessage {
                    id: "2".to_string(),
                    timestamp: now,
                    role: MessageRole::Assistant,
                    content: "hello".to_string(),
                    metadata: MessageMetadata::default(),
                },
            ],
        };

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let result = writer
            .export_session(&noisy_session, &raw_path, "jsonl")
            .await
            .unwrap();

        assert!(!result.written);
        assert_eq!(
            result.filtered_reason.as_deref(),
            Some("trivial greeting session")
        );
        assert!(!temp_dir
            .path()
            .join("indexes")
            .join("sessions.jsonl")
            .exists());
    }

    #[tokio::test]
    async fn test_read_browser_index_entries_skips_malformed_lines() {
        let temp_dir = TempDir::new().unwrap();
        let index_dir = temp_dir.path().join("indexes");
        tokio::fs::create_dir_all(&index_dir).await.unwrap();
        tokio::fs::write(
            index_dir.join("browser-history.jsonl"),
            concat!(
                "{\"stable_key\":\"chrome:Default:2026-05-03\",\"browser\":\"chrome\",\"profile\":\"Default\",\"date\":\"2026-05-03\",\"record_count\":1,\"latest_visit_at\":\"2026-05-03T10:00:00+08:00\",\"source_path\":\"/tmp/History\",\"source_mtime\":\"2026-05-03T10:00:00+08:00\",\"markdown_path\":\"browser-history/2026-05-03_chrome_default.md\",\"raw_path\":\"browser-history/2026-05-03_chrome_default.raw.jsonl\",\"exported_at\":\"2026-05-03T10:05:00+08:00\"}\n",
                "{\"stable_key\":\n",
                "{\"stable_key\":\"atlas:user-1:2026-05-03\",\"browser\":\"atlas\",\"profile\":\"user-1\",\"date\":\"2026-05-03\",\"record_count\":2,\"latest_visit_at\":\"2026-05-03T11:00:00+08:00\",\"source_path\":\"/tmp/AtlasHistory\",\"source_mtime\":\"2026-05-03T11:00:00+08:00\",\"markdown_path\":\"browser-history/2026-05-03_atlas_user-1.md\",\"raw_path\":\"browser-history/2026-05-03_atlas_user-1.raw.jsonl\",\"exported_at\":\"2026-05-03T11:05:00+08:00\"}\n"
            ),
        )
        .await
        .unwrap();

        let entries = super::read_browser_index_entries(temp_dir.path())
            .await
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries.contains_key("chrome:Default:2026-05-03"));
        assert!(entries.contains_key("atlas:user-1:2026-05-03"));
    }
}
