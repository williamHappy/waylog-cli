use crate::error::{Result, WaylogError};
use crate::providers::base::*;
use crate::utils::path;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::fs;

pub struct GeminiProvider;

impl GeminiProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn data_dir(&self) -> Result<PathBuf> {
        path::get_ai_data_dir("gemini").map(|p| p.join("tmp"))
    }

    fn session_dir(&self, project_path: &Path) -> Result<PathBuf> {
        let hash = path::encode_path_gemini(project_path);
        Ok(self.data_dir()?.join(hash).join("chats"))
    }

    async fn find_latest_session(&self, project_path: &Path) -> Result<Option<PathBuf>> {
        let candidates = self.get_all_sessions(project_path).await?;
        Ok(candidates.into_iter().next())
    }

    async fn get_all_sessions(&self, project_path: &Path) -> Result<Vec<PathBuf>> {
        let session_dir = self.session_dir(project_path)?;
        self.collect_sessions_in_dir(&session_dir).await
    }

    async fn get_all_host_sessions(&self) -> Result<Vec<PathBuf>> {
        let base_dir = self.data_dir()?;

        if !base_dir.exists() {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::new();
        for entry in walkdir::WalkDir::new(&base_dir) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let path = entry.path();
            if path.is_file() && Self::is_chat_session_file(path) {
                let metadata = fs::metadata(path).await?;
                let modified = metadata.modified()?;
                candidates.push((path.to_path_buf(), modified));
            }
        }

        candidates.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        Ok(candidates.into_iter().map(|(path, _)| path).collect())
    }

    async fn parse_session(&self, file_path: &Path) -> Result<ChatSession> {
        let content = fs::read_to_string(file_path).await?;
        let session_data: GeminiSession =
            serde_json::from_str(&content).map_err(WaylogError::Json)?;

        let messages = session_data
            .messages
            .into_iter()
            .filter_map(|msg| self.parse_message(msg).ok().flatten())
            .collect::<Vec<_>>();

        let started_at = DateTime::parse_from_rfc3339(&session_data.start_time)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        let updated_at = DateTime::parse_from_rfc3339(&session_data.last_updated)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(started_at);

        // Decode project path from hash
        let project_path = file_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_default();

        Ok(ChatSession {
            session_id: session_data.session_id,
            provider: self.name().to_string(),
            project_path,
            started_at,
            updated_at,
            messages,
        })
    }

    fn is_installed(&self) -> bool {
        // Gemini CLI might not be in PATH, check for data directory instead
        self.data_dir().map(|d| d.exists()).unwrap_or(false)
    }

    fn command(&self) -> &str {
        "gemini"
    }

    fn raw_extension(&self) -> &str {
        "json"
    }
}

impl GeminiProvider {
    fn is_chat_session_file(path: &Path) -> bool {
        path.extension().and_then(|s| s.to_str()) == Some("json")
            && path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                == Some("chats")
    }

    async fn collect_sessions_in_dir(&self, session_dir: &Path) -> Result<Vec<PathBuf>> {
        if !session_dir.exists() {
            return Ok(Vec::new());
        }

        let mut entries = fs::read_dir(session_dir).await?;
        let mut candidates = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if Self::is_chat_session_file(&path) {
                let metadata = fs::metadata(&path).await?;
                let modified = metadata.modified()?;
                candidates.push((path, modified));
            }
        }

        candidates.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        Ok(candidates.into_iter().map(|(path, _)| path).collect())
    }

    fn parse_message(&self, msg: GeminiMessage) -> Result<Option<ChatMessage>> {
        let role = match msg.message_type.as_str() {
            "user" => MessageRole::User,
            "gemini" => MessageRole::Assistant,
            _ => return Ok(None),
        };

        if msg.content.is_empty() {
            return Ok(None);
        }

        let timestamp = DateTime::parse_from_rfc3339(&msg.timestamp)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        // Extract thoughts (Gemini-specific feature)
        let thoughts = msg
            .thoughts
            .unwrap_or_default()
            .into_iter()
            .map(|t| format!("{}: {}", t.subject, t.description))
            .collect();

        // Extract token usage
        let tokens = msg.tokens.map(|t| TokenUsage {
            input: t.input,
            output: t.output,
            cached: t.cached,
        });

        Ok(Some(ChatMessage {
            id: msg.id,
            timestamp,
            role,
            content: msg.content,
            metadata: MessageMetadata {
                model: msg.model,
                tokens,
                tool_calls: Vec::new(),
                thoughts,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::GeminiProvider;
    use std::path::Path;

    #[test]
    fn test_is_chat_session_file_only_matches_chats_directory() {
        assert!(GeminiProvider::is_chat_session_file(Path::new(
            "/tmp/project-hash/chats/session-1.json"
        )));
        assert!(!GeminiProvider::is_chat_session_file(Path::new(
            "/tmp/project-hash/logs.json"
        )));
        assert!(!GeminiProvider::is_chat_session_file(Path::new(
            "/tmp/settings.json"
        )));
    }
}

// Gemini JSON session structures
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSession {
    session_id: String,
    #[allow(dead_code)]
    project_hash: String,
    start_time: String,
    last_updated: String,
    messages: Vec<GeminiMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiMessage {
    id: String,
    timestamp: String,

    #[serde(rename = "type")]
    message_type: String,

    content: String,
    model: Option<String>,
    thoughts: Option<Vec<GeminiThought>>,
    tokens: Option<GeminiTokens>,
}

#[derive(Debug, Deserialize)]
struct GeminiThought {
    subject: String,
    description: String,
    #[allow(dead_code)]
    timestamp: String,
}

#[derive(Debug, Deserialize)]
struct GeminiTokens {
    input: u32,
    output: u32,
    cached: u32,
}
