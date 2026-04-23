use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePaths {
    pub markdown_path: PathBuf,
    pub raw_path: PathBuf,
}

pub fn build_readable_base(
    started_at: &str,
    provider: &str,
    title: &str,
    collision_suffix: Option<&str>,
) -> String {
    let timestamp = crate::utils::time::format_rfc3339_for_local_filename(started_at);

    let slug = crate::utils::string::slugify(title);
    match collision_suffix {
        Some(suffix) if !suffix.is_empty() => {
            format!("{}_{}_{}_{}", timestamp, provider, slug, suffix)
        }
        _ => format!("{}_{}_{}", timestamp, provider, slug),
    }
}

pub fn archive_paths(archive_dir: &Path, readable_base: &str, raw_extension: &str) -> ArchivePaths {
    let extension = raw_extension.trim_start_matches('.');
    let sessions_dir = archive_dir.join("sessions");

    ArchivePaths {
        markdown_path: sessions_dir.join(format!("{}.md", readable_base)),
        raw_path: sessions_dir.join(format!("{}.raw.{}", readable_base, extension)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, Utc};
    use std::path::Path;

    #[test]
    fn test_build_readable_base_keeps_provider_and_title() {
        let base = build_readable_base(
            "2026-04-24T11:05:10Z",
            "codex",
            "为 waylog 设计统一归档方案",
            None,
        );

        let expected_prefix = chrono::DateTime::parse_from_rfc3339("2026-04-24T11:05:10Z")
            .unwrap()
            .with_timezone(&Utc)
            .with_timezone(&Local)
            .format("%Y-%m-%d_%H-%M-%S")
            .to_string();
        assert_eq!(
            base,
            format!("{}_codex_为-waylog-设计统一归档方案", expected_prefix)
        );
    }

    #[test]
    fn test_build_readable_base_appends_hash_suffix_when_present() {
        let base = build_readable_base(
            "2026-04-24T11:05:10Z",
            "claude",
            "设计统一归档目录结构",
            Some("a1b2c3"),
        );

        let expected_prefix = chrono::DateTime::parse_from_rfc3339("2026-04-24T11:05:10Z")
            .unwrap()
            .with_timezone(&Utc)
            .with_timezone(&Local)
            .format("%Y-%m-%d_%H-%M-%S")
            .to_string();
        assert_eq!(
            base,
            format!("{}_claude_设计统一归档目录结构_a1b2c3", expected_prefix)
        );
    }

    #[test]
    fn test_archive_paths_are_flattened_under_sessions_dir() {
        let paths = archive_paths(
            Path::new("/tmp/archive"),
            "2026-04-24_11-05-10_codex_设计统一归档方案",
            "jsonl",
        );

        assert_eq!(
            paths.markdown_path,
            Path::new("/tmp/archive/sessions/2026-04-24_11-05-10_codex_设计统一归档方案.md")
        );
        assert_eq!(
            paths.raw_path,
            Path::new("/tmp/archive/sessions/2026-04-24_11-05-10_codex_设计统一归档方案.raw.jsonl")
        );
    }
}
