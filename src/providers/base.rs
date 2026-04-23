use crate::error::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Represents a chat message from any AI provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub role: MessageRole,
    pub content: String,
    pub metadata: MessageMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageMetadata {
    /// Model used (e.g., "claude-sonnet-4.5", "gemini-2.5-flash")
    pub model: Option<String>,

    /// Token usage
    pub tokens: Option<TokenUsage>,

    /// Tool calls (for Claude Code)
    pub tool_calls: Vec<String>,

    /// Thoughts (for Gemini)
    pub thoughts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
    pub cached: u32,
}

/// Represents a complete chat session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub session_id: String,
    pub provider: String,
    pub project_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<ChatMessage>,
}

/// Provider trait - each AI CLI tool implements this
#[async_trait]
pub trait Provider: Send + Sync {
    /// Get the provider name (e.g., "codex", "claude", "gemini")
    fn name(&self) -> &str;

    /// Get the data directory for this provider
    fn data_dir(&self) -> Result<PathBuf>;

    /// Get the session directory for a specific project
    fn session_dir(&self, project_path: &Path) -> Result<PathBuf>;

    /// Find the latest session file for the current project
    async fn find_latest_session(&self, project_path: &Path) -> Result<Option<PathBuf>>;

    /// Parse a session file and return a chat session
    async fn parse_session(&self, file_path: &Path) -> Result<ChatSession>;

    /// Get all session files for a specific project
    async fn get_all_sessions(&self, project_path: &Path) -> Result<Vec<PathBuf>>;

    /// Get all session files available on the host
    async fn get_all_host_sessions(&self) -> Result<Vec<PathBuf>>;

    /// Check if the CLI tool is installed
    fn is_installed(&self) -> bool;

    /// Check if the provider has local session data available
    fn has_local_data(&self) -> bool {
        self.data_dir().map(|dir| dir.exists()).unwrap_or(false)
    }

    /// Get the command to run the CLI tool
    fn command(&self) -> &str;

    /// Get the raw session file extension without leading dot
    fn raw_extension(&self) -> &str {
        "jsonl"
    }
}
