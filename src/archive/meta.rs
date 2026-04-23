use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionIndexEntry {
    pub stable_key: String,
    pub provider: String,
    pub session_id: String,
    pub title: String,
    pub started_at: String,
    pub project_path: String,
    pub source_path: String,
    pub source_mtime: String,
    pub source_size: u64,
    pub message_count: usize,
    pub markdown_path: String,
    pub raw_path: String,
    pub exported_at: String,
}

impl SessionIndexEntry {
    pub fn should_rewrite(
        &self,
        source_mtime: &str,
        source_size: u64,
        message_count: usize,
    ) -> bool {
        self.source_mtime != source_mtime
            || self.source_size != source_size
            || self.message_count != message_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_should_rewrite_when_source_mtime_changes() {
        let old_meta = SessionIndexEntry {
            stable_key: "codex:session-1".to_string(),
            provider: "codex".to_string(),
            session_id: "session-1".to_string(),
            title: "设计统一归档方案".to_string(),
            started_at: "2026-04-24T19:00:00+08:00".to_string(),
            project_path: "/tmp/project".to_string(),
            source_path: "/tmp/source.jsonl".to_string(),
            source_mtime: "2026-04-24T19:00:00+08:00".to_string(),
            source_size: 100,
            message_count: 10,
            markdown_path: "sessions/base.md".to_string(),
            raw_path: "sessions/base.raw.jsonl".to_string(),
            exported_at: "2026-04-24T19:00:10+08:00".to_string(),
        };

        let changed = old_meta.should_rewrite("2026-04-24T19:30:00+08:00", 100, 10);
        assert!(changed);
    }

    #[test]
    fn test_should_rewrite_when_message_count_grows() {
        let old_meta = SessionIndexEntry {
            stable_key: "claude:session-2".to_string(),
            provider: "claude".to_string(),
            session_id: "session-2".to_string(),
            title: "设计归档目录结构".to_string(),
            started_at: "2026-04-24T19:00:00+08:00".to_string(),
            project_path: "/tmp/project".to_string(),
            source_path: "/tmp/source.jsonl".to_string(),
            source_mtime: "2026-04-24T19:00:00+08:00".to_string(),
            source_size: 100,
            message_count: 10,
            markdown_path: "sessions/base.md".to_string(),
            raw_path: "sessions/base.raw.jsonl".to_string(),
            exported_at: Utc::now().to_rfc3339(),
        };

        let changed = old_meta.should_rewrite("2026-04-24T19:00:00+08:00", 100, 11);
        assert!(changed);
    }

    #[test]
    fn test_should_not_rewrite_when_source_is_unchanged() {
        let old_meta = SessionIndexEntry {
            stable_key: "claude:session-2".to_string(),
            provider: "claude".to_string(),
            session_id: "session-2".to_string(),
            title: "设计归档目录结构".to_string(),
            started_at: "2026-04-24T19:00:00+08:00".to_string(),
            project_path: "/tmp/project".to_string(),
            source_path: "/tmp/source.jsonl".to_string(),
            source_mtime: "2026-04-24T19:00:00+08:00".to_string(),
            source_size: 100,
            message_count: 10,
            markdown_path: "sessions/base.md".to_string(),
            raw_path: "sessions/base.raw.jsonl".to_string(),
            exported_at: Utc::now().to_rfc3339(),
        };

        let changed = old_meta.should_rewrite("2026-04-24T19:00:00+08:00", 100, 10);
        assert!(!changed);
    }
}
