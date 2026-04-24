use crate::error::Result;
use crate::exporter;
use crate::providers::base::Provider;
use crate::session::SessionTracker;
use crate::utils::path;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::debug;

/// Shared synchronization logic for both watcher and batch sync
pub struct Synchronizer {
    provider: Arc<dyn Provider>,
    project_dir: PathBuf,
    tracker: Arc<SessionTracker>,
    archive_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    Synced { new_messages: usize },
    UpToDate,
    Skipped,
    Failed(String),
}

impl Synchronizer {
    pub fn new(
        provider: Arc<dyn Provider>,
        project_dir: PathBuf,
        tracker: Arc<SessionTracker>,
        archive_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            provider,
            project_dir,
            tracker,
            archive_dir,
        }
    }

    /// Sync all available sessions from the provider
    /// Returns stats: (Synced, UpToDate, Skipped, Failed)
    pub async fn sync_all(&self, force: bool) -> Result<Vec<(PathBuf, SyncStatus)>> {
        let sessions = self.provider.get_all_sessions(&self.project_dir).await?;
        let mut results = Vec::new();

        for session_path in sessions {
            let status = match self.sync_session(&session_path, force).await {
                Ok(status) => status,
                Err(e) => SyncStatus::Failed(e.to_string()),
            };
            results.push((session_path, status));
        }

        Ok(results)
    }

    /// Sync a specific session file
    pub async fn sync_session(&self, session_path: &Path, force: bool) -> Result<SyncStatus> {
        // 1. Parse session
        let session = match self.provider.parse_session(session_path).await {
            Ok(s) => s,
            Err(e) => return Ok(SyncStatus::Failed(format!("Parse error: {}", e))),
        };

        if session.messages.is_empty() {
            return Ok(SyncStatus::Skipped);
        }

        // 2. Check state
        let state = self.tracker.get_state().await;
        let (markdown_path, mut synced_count) =
            if let Some(s) = state.get_session(&session.session_id) {
                (s.markdown_path.clone(), s.synced_message_count)
            } else {
                // New session: generate filename
                let slug = session
                    .messages
                    .iter()
                    .find(|m| m.role == crate::providers::base::MessageRole::User)
                    .map(|m| crate::utils::string::slugify(&m.content))
                    .unwrap_or_else(|| session.session_id.clone());

                let timestamp =
                    crate::utils::time::format_local_filename_timestamp(&session.started_at);
                let filename = format!("{}-{}-{}.md", timestamp, self.provider.name(), slug);
                let path = path::get_waylog_dir(&self.project_dir).join(filename);

                (path, 0)
            };

        // 3. Handle force/missing file
        if force || (!markdown_path.exists() && synced_count > 0) {
            synced_count = 0;
        }

        // 4. Calculate new messages
        let total_messages = session.messages.len();
        if synced_count >= total_messages {
            return Ok(SyncStatus::UpToDate);
        }

        let new_messages: Vec<_> = session
            .messages
            .iter()
            .skip(synced_count)
            .cloned()
            .collect();

        if new_messages.is_empty() {
            return Ok(SyncStatus::UpToDate);
        }

        // 5. Write to file
        if let Some(parent) = markdown_path.parent() {
            path::ensure_dir_exists(parent)?;
        }

        if synced_count == 0 {
            exporter::create_markdown_file(&markdown_path, &session).await?;
        } else {
            exporter::append_messages(&markdown_path, &new_messages).await?;
        }

        if let Some(archive_dir) = &self.archive_dir {
            let writer = crate::archive::ArchiveWriter::new(archive_dir.clone());
            let result = writer
                .export_session(&session, session_path, self.provider.raw_extension())
                .await?;
            if let Some(reason) = result.filtered_reason {
                debug!(
                    "Skipped archive export for {} session {}: {}",
                    self.provider.name(),
                    session.session_id,
                    reason
                );
            }
        }

        // 6. Update state
        self.tracker
            .update_session(
                session.session_id.clone(),
                session_path.to_path_buf(),
                markdown_path.clone(),
                total_messages,
            )
            .await?;

        // Log purely for debug, UI is handled by caller
        debug!(
            "Synced {} messages to {}",
            new_messages.len(),
            markdown_path.display()
        );

        Ok(SyncStatus::Synced {
            new_messages: new_messages.len(),
        })
    }
}
