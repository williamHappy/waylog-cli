pub mod layout;
pub mod meta;

use crate::error::Result;
use crate::providers::base::ChatSession;
use crate::session_filter;
use chrono::Utc;
use meta::SessionIndexEntry;
use serde::Serialize;
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
        let readable_base = layout::build_readable_base(
            &session.started_at.to_rfc3339(),
            &session.provider,
            &title,
            None,
        );
        let paths = layout::archive_paths(&self.archive_dir, &readable_base, raw_extension);

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

        let mut index_entries = read_index_entries(&self.archive_dir).await?;
        if let Some(existing_entry) = index_entries.get(&stable_key) {
            if !existing_entry.should_rewrite(&source_mtime, source_size, session.messages.len()) {
                remove_legacy_meta_file(&paths.markdown_path).await?;
                return Ok(ArchiveExportResult {
                    paths,
                    written: false,
                    filtered_reason: None,
                });
            }
        }

        fs::write(
            &paths.markdown_path,
            crate::exporter::generate_markdown(session),
        )
        .await?;
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
        };

        index_entries.insert(stable_key.clone(), entry);
        write_index_entries(&self.archive_dir, &index_entries).await?;
        write_manifest(&self.archive_dir, &index_entries, &stable_key).await?;

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

async fn read_index_entries(archive_dir: &Path) -> Result<BTreeMap<String, SessionIndexEntry>> {
    let index_path = archive_dir.join("indexes").join("sessions.jsonl");
    if !index_path.exists() {
        return Ok(BTreeMap::new());
    }

    let content = fs::read_to_string(index_path).await?;
    let mut entries = BTreeMap::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let entry: SessionIndexEntry = serde_json::from_str(line)?;
        entries.insert(entry.stable_key.clone(), entry);
    }

    Ok(entries)
}

async fn write_index_entries(
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

async fn write_manifest(
    archive_dir: &Path,
    entries: &BTreeMap<String, SessionIndexEntry>,
    latest_session: &str,
) -> Result<()> {
    let mut providers = entries
        .values()
        .map(|entry| entry.provider.clone())
        .collect::<Vec<_>>();
    providers.sort();
    providers.dedup();

    let manifest = ArchiveManifest {
        updated_at: crate::utils::time::format_local_rfc3339(&Utc::now()),
        session_count: entries.len(),
        providers,
        archive_version: 2,
        latest_session: latest_session.to_string(),
    };
    let manifest_path = archive_dir.join("indexes").join("manifest.json");
    fs::write(manifest_path, serde_json::to_vec_pretty(&manifest)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ArchiveWriter;
    use crate::providers::base::{ChatMessage, ChatSession, MessageMetadata, MessageRole};
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::TempDir;

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
    async fn test_archive_writer_skips_unchanged_session_on_second_export() {
        let temp_dir = TempDir::new().unwrap();
        let raw_path = temp_dir.path().join("source.jsonl");
        tokio::fs::write(&raw_path, "{\"type\":\"response_item\"}\n")
            .await
            .unwrap();

        let writer = ArchiveWriter::new(temp_dir.path().to_path_buf());
        let first = writer
            .export_session(&test_session(), &raw_path, "jsonl")
            .await
            .unwrap();
        let second = writer
            .export_session(&test_session(), &raw_path, "jsonl")
            .await
            .unwrap();

        assert!(first.written);
        assert!(!second.written);
        assert!(second.filtered_reason.is_none());
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
}
