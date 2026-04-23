use crate::error::Result;
use crate::providers::base::Provider;
use crate::session::SessionTracker;
use crate::synchronizer::Synchronizer;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{debug, info};

/// Sync interval in seconds
const SYNC_INTERVAL_SECS: u64 = 30;

/// Periodic sync watcher (simplified - no file watching)
pub struct FileWatcher {
    synchronizer: Synchronizer,
}

impl FileWatcher {
    pub fn new(
        provider: Arc<dyn Provider>,
        project_dir: PathBuf,
        tracker: Arc<SessionTracker>,
        archive_dir: Option<PathBuf>,
    ) -> Self {
        let synchronizer = Synchronizer::new(
            provider.clone(),
            project_dir.clone(),
            tracker.clone(),
            archive_dir,
        );

        Self { synchronizer }
    }

    /// Start periodic sync loop
    pub async fn watch(&self) -> Result<()> {
        info!(
            "Starting periodic sync (every {} seconds)",
            SYNC_INTERVAL_SECS
        );

        let mut interval = time::interval(Duration::from_secs(SYNC_INTERVAL_SECS));

        loop {
            interval.tick().await;

            if let Err(e) = self.sync_available().await {
                tracing::error!("Periodic sync error: {}", e);
            }
        }
    }

    /// Sync all known sessions for the current project so newly created sessions
    /// are discovered without waiting for the final cleanup pass.
    async fn sync_available(&self) -> Result<()> {
        let results = self.synchronizer.sync_all(false).await?;
        if results.is_empty() {
            debug!("No session file found");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::base::{
        ChatMessage, ChatSession, MessageMetadata, MessageRole, Provider,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    struct MockProvider {
        name: String,
        sessions: HashMap<PathBuf, ChatSession>,
        latest_session: Option<PathBuf>,
    }

    impl MockProvider {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                sessions: HashMap::new(),
                latest_session: None,
            }
        }

        fn add_session(&mut self, path: PathBuf, session: ChatSession) {
            self.sessions.insert(path, session);
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }

        fn data_dir(&self) -> Result<PathBuf> {
            Ok(std::env::temp_dir())
        }

        fn session_dir(&self, _project_path: &Path) -> Result<PathBuf> {
            Ok(std::env::temp_dir().join("sessions"))
        }

        async fn find_latest_session(&self, _project_path: &Path) -> Result<Option<PathBuf>> {
            Ok(self.latest_session.clone())
        }

        async fn parse_session(&self, file_path: &Path) -> Result<ChatSession> {
            self.sessions.get(file_path).cloned().ok_or_else(|| {
                crate::error::WaylogError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Session not found: {}", file_path.display()),
                ))
            })
        }

        async fn get_all_sessions(&self, _project_path: &Path) -> Result<Vec<PathBuf>> {
            Ok(self.sessions.keys().cloned().collect())
        }

        async fn get_all_host_sessions(&self) -> Result<Vec<PathBuf>> {
            Ok(self.sessions.keys().cloned().collect())
        }

        fn is_installed(&self) -> bool {
            true
        }

        fn command(&self) -> &str {
            "mock"
        }
    }

    fn create_test_session(session_id: &str, message_count: usize) -> ChatSession {
        let now = Utc::now();
        let mut messages = Vec::new();
        for i in 0..message_count {
            messages.push(ChatMessage {
                id: format!("msg-{}", i),
                timestamp: now,
                role: if i % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                content: format!("Message {}", i),
                metadata: MessageMetadata::default(),
            });
        }

        ChatSession {
            session_id: session_id.to_string(),
            provider: "mock".to_string(),
            project_path: PathBuf::from("/test/project"),
            started_at: now,
            updated_at: now,
            messages,
        }
    }

    #[tokio::test]
    async fn test_sync_available_discovers_new_session_even_if_latest_is_stale() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path().join("project");
        tokio::fs::create_dir_all(&project_dir).await.unwrap();

        let old_path = temp_dir.path().join("old.jsonl");
        let new_path = temp_dir.path().join("new.jsonl");
        tokio::fs::write(&old_path, "{}").await.unwrap();
        tokio::fs::write(&new_path, "{}").await.unwrap();

        let mut provider = MockProvider::new("mock");
        provider.add_session(old_path.clone(), create_test_session("old-session", 2));
        provider.add_session(new_path.clone(), create_test_session("new-session", 3));
        provider.latest_session = Some(old_path);

        let provider: Arc<dyn Provider> = Arc::new(provider);
        let tracker = Arc::new(
            SessionTracker::new(project_dir.clone(), provider.clone())
                .await
                .unwrap(),
        );

        let watcher = FileWatcher::new(provider, project_dir, tracker.clone(), None);
        watcher.sync_available().await.unwrap();

        assert_eq!(tracker.get_synced_count("old-session").await, 2);
        assert_eq!(tracker.get_synced_count("new-session").await, 3);
    }
}
