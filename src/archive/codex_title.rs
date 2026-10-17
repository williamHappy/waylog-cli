//! Best-effort local Codex names, used only when creating an archive entry.
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, BufReader};

pub(super) async fn read(source_path: &Path) -> Option<String> {
    let codex_home = source_path.ancestors().find_map(|path| {
        matches!(
            path.file_name()?.to_str()?,
            "sessions" | "archived_sessions"
        )
        .then(|| path.parent())
        .flatten()
    })?;
    let mut lines = BufReader::new(tokio::fs::File::open(source_path).await.ok()?).lines();
    let mut thread_id = None;
    for _ in 0..32 {
        let Some(line) = lines.next_line().await.ok()? else {
            break;
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event["type"] == "session_meta" {
            thread_id = event["payload"]["id"].as_str().map(str::to_owned);
            break;
        }
    }
    let thread_id = thread_id?;
    let home = codex_home.to_path_buf();
    // SQLite is blocking; never open the live database with write/create flags.
    let id = thread_id.clone();
    if let Some(name) = tokio::task::spawn_blocking(move || read_database(&home, &id))
        .await
        .ok()
        .flatten()
    {
        return Some(name);
    }
    let file = tokio::fs::File::open(codex_home.join("session_index.jsonl"))
        .await
        .ok()?;
    let mut lines = BufReader::new(file).lines();
    let mut name = None;
    while let Some(line) = lines.next_line().await.ok()? {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if entry["id"].as_str() == Some(&thread_id) {
            // The index is append-only: the last record wins, including an empty name.
            name = nonempty(entry["thread_name"].as_str());
        }
    }
    name
}

fn nonempty(name: Option<&str>) -> Option<String> {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

fn read_database(home: &Path, thread_id: &str) -> Option<String> {
    let database = std::fs::read_dir(home)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let version = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            Some((version, path))
        })
        .max_by_key(|(version, _)| *version)?
        .1;
    let connection =
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    connection
        .busy_timeout(std::time::Duration::from_millis(100))
        .ok()?;
    let name: Option<String> = connection
        .query_row(
            "SELECT name FROM threads WHERE id = ?1",
            [thread_id],
            |row| row.get(0),
        )
        .ok()?;
    nonempty(name.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture(home: &Path) -> std::path::PathBuf {
        let dir = home.join("sessions/2026/10/17");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let source = dir.join("rollout-unrelated-filename.jsonl");
        tokio::fs::write(
            &source,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-1\"}}\n",
        )
        .await
        .unwrap();
        source
    }

    #[tokio::test]
    async fn database_name_wins_over_title_and_stale_index() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = fixture(temp.path()).await;
        let db = Connection::open(temp.path().join("state_5.sqlite")).unwrap();
        db.execute_batch(
            "CREATE TABLE threads (id TEXT, title TEXT, name TEXT);
            INSERT INTO threads VALUES ('thread-1', 'first user message', '界面标题');",
        )
        .unwrap();
        tokio::fs::write(
            temp.path().join("session_index.jsonl"),
            "{\"id\":\"thread-1\",\"thread_name\":\"旧标题\"}\n",
        )
        .await
        .unwrap();
        assert_eq!(read(&source).await.as_deref(), Some("界面标题"));
        db.execute("UPDATE threads SET name = '重命名后的标题'", [])
            .unwrap();
        assert_eq!(read(&source).await.as_deref(), Some("重命名后的标题"));
    }

    #[tokio::test]
    async fn unavailable_database_uses_last_matching_index_record() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = fixture(temp.path()).await;
        tokio::fs::write(temp.path().join("state_5.sqlite"), "broken database")
            .await
            .unwrap();
        let index = temp.path().join("session_index.jsonl");
        tokio::fs::write(
            &index,
            concat!(
                "{\"id\":\"thread-1\",\"thread_name\":\"旧名称\"}\n",
                "{invalid\n",
                "{\"id\":\"other\",\"thread_name\":\"别的会话\"}\n",
                "{\"id\":\"thread-1\",\"thread_name\":\" 新名称 \"}\n"
            ),
        )
        .await
        .unwrap();
        assert_eq!(read(&source).await.as_deref(), Some("新名称"));
        tokio::fs::write(&index, "{\"id\":\"thread-1\",\"thread_name\":\" \"}\n")
            .await
            .unwrap();
        assert_eq!(read(&source).await, None);
    }

    #[tokio::test]
    async fn missing_metadata_or_name_returns_none_without_creating_storage() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = fixture(temp.path()).await;
        assert_eq!(read(&source).await, None);
        assert!(!temp.path().join("state_5.sqlite").exists());
        assert!(!temp.path().join("session_index.jsonl").exists());
        tokio::fs::write(&source, "{\"type\":\"session_meta\",\"payload\":{}}\n")
            .await
            .unwrap();
        assert_eq!(read(&source).await, None);
    }
}
