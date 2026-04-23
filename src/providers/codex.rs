use crate::error::Result;
use crate::providers::base::*;
use crate::utils::path;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

pub struct CodexProvider;

impl CodexProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn name(&self) -> &str {
        "codex"
    }

    fn data_dir(&self) -> Result<PathBuf> {
        Ok(path::home_dir()?.join(".codex").join("sessions"))
    }

    fn session_dir(&self, _project_path: &Path) -> Result<PathBuf> {
        // Codex organizes by date: ~/.codex/sessions/YYYY/MM/DD/
        let now = Utc::now();
        Ok(self
            .data_dir()?
            .join(now.format("%Y").to_string())
            .join(now.format("%m").to_string())
            .join(now.format("%d").to_string()))
    }

    async fn find_latest_session(&self, project_path: &Path) -> Result<Option<PathBuf>> {
        // For 'run' mode, only scan recent days (last 7 days) for performance
        let base_session_dir = self.data_dir()?;

        if !base_session_dir.exists() {
            return Ok(None);
        }

        let now = Utc::now();
        let mut candidates = Vec::new();

        // Check last 7 days
        for days_ago in 0..7 {
            let date = now - chrono::Duration::days(days_ago);
            let day_dir = base_session_dir
                .join(date.format("%Y").to_string())
                .join(date.format("%m").to_string())
                .join(date.format("%d").to_string());

            if !day_dir.exists() {
                continue;
            }

            // Scan this day's directory
            if let Ok(mut entries) = fs::read_dir(&day_dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.is_file()
                        && path.extension().and_then(|s| s.to_str()) == Some("jsonl")
                        && self
                            .probe_project_path(&path, project_path)
                            .await
                            .unwrap_or(false)
                    {
                        if let Ok(metadata) = fs::metadata(&path).await {
                            if let Ok(modified) = metadata.modified() {
                                candidates.push((path, modified));
                            }
                        }
                    }
                }
            }
        }

        // Sort by modification time, newest first
        candidates.sort_by_key(|entry| std::cmp::Reverse(entry.1));

        Ok(candidates.into_iter().next().map(|(p, _)| p))
    }

    async fn get_all_sessions(&self, project_path: &Path) -> Result<Vec<PathBuf>> {
        self.collect_sessions(Some(project_path)).await
    }

    async fn get_all_host_sessions(&self) -> Result<Vec<PathBuf>> {
        self.collect_sessions(None).await
    }

    fn has_local_data(&self) -> bool {
        self.data_dir().map(|dir| dir.exists()).unwrap_or(false)
    }

    async fn parse_session(&self, file_path: &Path) -> Result<ChatSession> {
        let file = fs::File::open(file_path).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        let mut messages = Vec::new();
        let mut session_id = String::new();
        let mut started_at = Utc::now();
        let mut session_project_path = PathBuf::new();

        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }

            if let Ok(event) = serde_json::from_str::<CodexEvent>(&line) {
                // Pick session metadata
                if session_id.is_empty() {
                    session_id = file_path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                }

                match event.event_type.as_str() {
                    "session_meta" | "turn_context" => {
                        if let Some(cwd) = event.payload.as_ref().and_then(|p| p.cwd.clone()) {
                            session_project_path = PathBuf::from(cwd);
                        }
                    }
                    "response_item" => {
                        if let Some(payload) = event.payload {
                            if let Some(msg) =
                                self.parse_response_item(payload, &event.timestamp)?
                            {
                                if messages.is_empty() {
                                    started_at = msg.timestamp;
                                }

                                // Simple deduplication
                                let is_duplicate =
                                    messages.last().is_some_and(|last: &ChatMessage| {
                                        last.role == msg.role && last.content == msg.content
                                    });
                                if !is_duplicate {
                                    messages.push(msg);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(ChatSession {
            session_id,
            provider: self.name().to_string(),
            project_path: session_project_path,
            started_at,
            updated_at: messages.last().map(|m| m.timestamp).unwrap_or(started_at),
            messages,
        })
    }

    fn is_installed(&self) -> bool {
        which::which("codex").is_ok()
    }

    fn command(&self) -> &str {
        "codex"
    }
}

impl CodexProvider {
    async fn collect_sessions(&self, project_path: Option<&Path>) -> Result<Vec<PathBuf>> {
        let base_session_dir = self.data_dir()?;

        if !base_session_dir.exists() {
            return Ok(Vec::new());
        }

        let mut candidates = Vec::new();
        let walker = walkdir::WalkDir::new(&base_session_dir);

        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let matches_scope = match project_path {
                    Some(project_path) => self
                        .probe_project_path(path, project_path)
                        .await
                        .unwrap_or(false),
                    None => true,
                };

                if matches_scope {
                    if let Ok(metadata) = fs::metadata(path).await {
                        if let Ok(modified) = metadata.modified() {
                            candidates.push((path.to_path_buf(), modified));
                        }
                    }
                }
            }
        }

        candidates.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        Ok(candidates.into_iter().map(|(path, _)| path).collect())
    }

    async fn probe_project_path(
        &self,
        file_path: &Path,
        target_project_path: &Path,
    ) -> Result<bool> {
        let file = fs::File::open(file_path).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        // Normalize target path for comparison (handle both Unix and Windows separators)
        let target_str = target_project_path
            .to_string_lossy()
            .trim_end_matches('/')
            .trim_end_matches('\\')
            .to_string();

        // Scan first 50 lines (session_meta is usually first)
        let mut checked_lines = 0;
        while let Some(line) = lines.next_line().await? {
            if checked_lines >= 50 {
                break;
            }
            checked_lines += 1;

            if let Ok(event) = serde_json::from_str::<CodexEvent>(&line) {
                if let Some(cwd_str) = event.payload.and_then(|p| p.cwd) {
                    let session_cwd = cwd_str
                        .trim_end_matches('/')
                        .trim_end_matches('\\')
                        .to_string();

                    // Direct match
                    if session_cwd == target_str {
                        return Ok(true);
                    }

                    // Subdirectory match (safety: ensure we don't match root by accident)
                    if (target_str.starts_with(&session_cwd) && session_cwd.len() > 1)
                        || (session_cwd.starts_with(&target_str) && target_str.len() > 1)
                    {
                        return Ok(true);
                    }

                    // If we found a CWD but it definitely doesn't match, we can stop
                    return Ok(false);
                }
            }
        }
        Ok(false)
    }

    fn parse_response_item(
        &self,
        payload: CodexPayload,
        timestamp: &str,
    ) -> Result<Option<ChatMessage>> {
        let role = match payload.role.as_deref() {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            _ => return Ok(None),
        };

        // Extract text content
        let content = payload
            .content
            .and_then(|c| c.into_iter().find_map(|item| item.text))
            .unwrap_or_default();

        if content.is_empty() {
            return Ok(None);
        }

        let timestamp = DateTime::parse_from_rfc3339(timestamp)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        // Filter out system injections which Codex logs as "user" messages
        if role == MessageRole::User {
            // 1. Environment context
            if content.contains("<environment_context>") {
                return Ok(None);
            }
            // 2. AGENTS.md instructions
            if content.contains("<INSTRUCTIONS>") || content.contains("# AGENTS.md instructions") {
                return Ok(None);
            }
        }

        Ok(Some(ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp,
            role,
            content,
            metadata: MessageMetadata {
                model: None,
                tokens: None,
                tool_calls: Vec::new(),
                thoughts: Vec::new(),
            },
        }))
    }
}

// Codex JSONL event structures
#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    timestamp: String,
    payload: Option<CodexPayload>,
}

#[derive(Debug, Deserialize)]
struct CodexPayload {
    role: Option<String>,
    cwd: Option<String>,
    content: Option<Vec<CodexContent>>,
}

#[derive(Debug, Deserialize)]
struct CodexContent {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    content_type: String,
    text: Option<String>,
}
