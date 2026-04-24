use crate::providers::base::{ChatSession, MessageRole};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveSkipReason {
    TrivialGreeting,
    TrivialShellProbe,
    EmptyUntitledSession,
    CommitMessageGenerator,
    BoilerplateOnlyPrompt,
}

impl fmt::Display for ArchiveSkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::TrivialGreeting => "trivial greeting session",
            Self::TrivialShellProbe => "trivial shell-probe session",
            Self::EmptyUntitledSession => "empty untitled session",
            Self::CommitMessageGenerator => "commit-message generator session",
            Self::BoilerplateOnlyPrompt => "boilerplate-only setup session",
        };
        f.write_str(label)
    }
}

pub fn session_title(session: &ChatSession) -> String {
    first_user_line(session)
        .filter(|line| !line.trim().is_empty())
        .unwrap_or("Untitled Session")
        .to_string()
}

pub fn archive_skip_reason(session: &ChatSession) -> Option<ArchiveSkipReason> {
    let title = session_title(session);
    let normalized_title = normalize_title(&title);
    let message_count = session.messages.len();

    if is_trivial_greeting(&normalized_title, &title) && message_count <= 2 {
        return Some(ArchiveSkipReason::TrivialGreeting);
    }

    if is_trivial_shell_probe(&normalized_title) && message_count <= 2 {
        return Some(ArchiveSkipReason::TrivialShellProbe);
    }

    if normalized_title == "untitled session" && message_count <= 2 {
        return Some(ArchiveSkipReason::EmptyUntitledSession);
    }

    if is_commit_message_prompt(&normalized_title) && message_count <= 4 {
        return Some(ArchiveSkipReason::CommitMessageGenerator);
    }

    if is_boilerplate_title(&normalized_title) && message_count <= 2 {
        return Some(ArchiveSkipReason::BoilerplateOnlyPrompt);
    }

    None
}

fn first_user_line(session: &ChatSession) -> Option<&str> {
    session
        .messages
        .iter()
        .find(|message| matches!(message.role, MessageRole::User))
        .and_then(|message| {
            message
                .content
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
        })
}

fn normalize_title(title: &str) -> String {
    title.trim().to_lowercase()
}

fn is_trivial_greeting(normalized_title: &str, original_title: &str) -> bool {
    let title_len = original_title.trim().chars().count();
    let exact = matches!(
        normalized_title,
        "hi" | "hello" | "hey" | "你好" | "您好" | "嗨"
    );
    let prefixed = normalized_title.starts_with("hi,")
        || normalized_title.starts_with("hi，")
        || normalized_title.starts_with("hello,")
        || normalized_title.starts_with("hello，")
        || normalized_title.starts_with("你好，")
        || normalized_title.starts_with("嗨，");

    (exact || prefixed) && title_len <= 24
}

fn is_trivial_shell_probe(normalized_title: &str) -> bool {
    matches!(normalized_title, "exit" | "whoami" | "pwd" | "ls" | "date")
}

fn is_commit_message_prompt(normalized_title: &str) -> bool {
    normalized_title.contains("git commit")
        || normalized_title.contains("commit message")
        || normalized_title.contains("github commit")
        || normalized_title.contains("write a concise git commit message")
        || normalized_title.contains("生成规范的 commit")
        || normalized_title.contains("生成高质量 commit")
}

fn is_boilerplate_title(normalized_title: &str) -> bool {
    normalized_title.contains("agents-instructions")
        || normalized_title.contains("context-from-my-ide-setup")
        || normalized_title.contains("context from my ide setup")
}

#[cfg(test)]
mod tests {
    use super::{archive_skip_reason, session_title, ArchiveSkipReason};
    use crate::providers::base::{ChatMessage, ChatSession, MessageMetadata, MessageRole};
    use chrono::Utc;
    use std::path::PathBuf;

    fn session_from_messages(messages: Vec<(MessageRole, &str)>) -> ChatSession {
        let now = Utc::now();
        ChatSession {
            session_id: "session-1".to_string(),
            provider: "codex".to_string(),
            project_path: PathBuf::from("/tmp/project"),
            started_at: now,
            updated_at: now,
            messages: messages
                .into_iter()
                .enumerate()
                .map(|(idx, (role, content))| ChatMessage {
                    id: format!("m-{}", idx),
                    timestamp: now,
                    role,
                    content: content.to_string(),
                    metadata: MessageMetadata::default(),
                })
                .collect(),
        }
    }

    #[test]
    fn skips_short_greeting_sessions() {
        let session = session_from_messages(vec![
            (MessageRole::User, "hi"),
            (MessageRole::Assistant, "hello"),
        ]);

        assert_eq!(
            archive_skip_reason(&session),
            Some(ArchiveSkipReason::TrivialGreeting)
        );
    }

    #[test]
    fn keeps_meaningful_sessions_that_start_with_greeting() {
        let session = session_from_messages(vec![
            (
                MessageRole::User,
                "hi，帮我分析这个导出目录结构是否适合作为长期知识库",
            ),
            (MessageRole::Assistant, "可以，我们先看目标。"),
            (MessageRole::User, "继续"),
        ]);

        assert_eq!(archive_skip_reason(&session), None);
    }

    #[test]
    fn skips_commit_message_generator_sessions() {
        let session = session_from_messages(vec![
            (
                MessageRole::User,
                "你是一个 git commit message 生成助手，请根据以下信息生成规范的 commit message",
            ),
            (MessageRole::Assistant, "feat: add archive filtering"),
        ]);

        assert_eq!(
            archive_skip_reason(&session),
            Some(ArchiveSkipReason::CommitMessageGenerator)
        );
    }

    #[test]
    fn skips_agents_instruction_greeting_sessions() {
        let session = session_from_messages(vec![
            (
                MessageRole::User,
                "<agents-instructions>\n# Global Instructions\n</agents-instructions>\n\nhi",
            ),
            (MessageRole::Assistant, "你好"),
        ]);

        assert_eq!(session_title(&session), "<agents-instructions>");
        assert_eq!(
            archive_skip_reason(&session),
            Some(ArchiveSkipReason::BoilerplateOnlyPrompt)
        );
    }

    #[test]
    fn keeps_real_sessions() {
        let session = session_from_messages(vec![
            (MessageRole::User, "帮我设计 waylog 的归档过滤器"),
            (MessageRole::Assistant, "可以，先明确噪音类别。"),
        ]);

        assert_eq!(archive_skip_reason(&session), None);
    }
}
