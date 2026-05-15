use crate::browser::BrowserVisitRecord;
use crate::cli::Browser;
use crate::error::{Result, WaylogError};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CHROME_EPOCH_OFFSET_MICROS: i64 = 11_644_473_600_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromiumProfileHistory {
    pub browser: String,
    pub profile: String,
    pub history_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChromiumSource {
    browser: &'static str,
    root_dir: PathBuf,
}

pub struct ChromiumHistoryCollector {
    sources: Vec<ChromiumSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCollectionResult {
    pub visits: Vec<BrowserVisitRecord>,
    pub warnings: Vec<String>,
}

impl ChromiumHistoryCollector {
    pub fn new(selected_browser: Option<&Browser>) -> Result<Self> {
        Ok(Self {
            sources: default_sources(selected_browser)?,
        })
    }

    #[cfg(test)]
    fn with_sources(sources: Vec<(&'static str, PathBuf)>) -> Self {
        Self {
            sources: sources
                .into_iter()
                .map(|(browser, root_dir)| ChromiumSource { browser, root_dir })
                .collect(),
        }
    }

    #[cfg(test)]
    fn discover_profiles(&self) -> Result<Vec<ChromiumProfileHistory>> {
        let mut profiles = Vec::new();
        for source in &self.sources {
            profiles.extend(discover_profiles_in_root(source)?);
        }
        profiles.sort_by(|left, right| {
            left.browser
                .cmp(&right.browser)
                .then_with(|| left.profile.cmp(&right.profile))
        });
        Ok(profiles)
    }

    pub async fn collect_visits_since(
        &self,
        since_by_source_profile: &HashMap<String, DateTime<Utc>>,
    ) -> Result<BrowserCollectionResult> {
        let mut visits = Vec::new();
        let mut warnings = Vec::new();

        for source in &self.sources {
            let profiles = match discover_profiles_in_root(source) {
                Ok(profiles) => profiles,
                Err(error) => {
                    let message = format!(
                        "Skipping unreadable {} browser root {}: {}",
                        source.browser,
                        source.root_dir.display(),
                        error
                    );
                    tracing::warn!("{message}");
                    warnings.push(message);
                    continue;
                }
            };

            for profile in profiles {
                let since = since_by_source_profile
                    .get(&source_profile_key(&profile.browser, &profile.profile))
                    .cloned();
                let browser_name = profile.browser.clone();
                let profile_name = profile.profile.clone();
                let history_path = profile.history_path.clone();
                let profile_visits = tokio::task::spawn_blocking(move || {
                    read_profile_visits(&browser_name, &profile_name, &history_path, since)
                })
                .await
                .map_err(|error| WaylogError::Internal(error.to_string()))?;

                match profile_visits {
                    Ok(profile_visits) => visits.extend(profile_visits),
                    Err(error) => {
                        let message = format!(
                            "Skipping unreadable {} browser profile {} ({}): {}",
                            profile.browser,
                            profile.profile,
                            profile.history_path.display(),
                            error
                        );
                        tracing::warn!("{message}");
                        warnings.push(message);
                    }
                }
            }
        }

        visits.sort_by(|left, right| {
            left.visited_at
                .cmp(&right.visited_at)
                .then_with(|| left.browser.cmp(&right.browser))
                .then_with(|| left.profile.cmp(&right.profile))
        });
        Ok(BrowserCollectionResult { visits, warnings })
    }
}

fn default_sources(selected_browser: Option<&Browser>) -> Result<Vec<ChromiumSource>> {
    let all = vec![
        ChromiumSource {
            browser: "chrome",
            root_dir: default_chrome_root_dir()?,
        },
        ChromiumSource {
            browser: "atlas",
            root_dir: default_atlas_root_dir()?,
        },
    ];

    Ok(match selected_browser {
        None => all,
        Some(Browser::Chrome) => all
            .into_iter()
            .filter(|source| source.browser == "chrome")
            .collect(),
        Some(Browser::Atlas) => all
            .into_iter()
            .filter(|source| source.browser == "atlas")
            .collect(),
    })
}

fn default_chrome_root_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let local_app_data = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            WaylogError::PathError("LOCALAPPDATA is not set; cannot locate Chrome data".to_string())
        })?;
        return Ok(chrome_root_dir_from_local_app_data(Path::new(
            &local_app_data,
        )));
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(crate::utils::path::home_dir()?
            .join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome"))
    }
}

fn default_atlas_root_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let local_app_data = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
            WaylogError::PathError("LOCALAPPDATA is not set; cannot locate Atlas data".to_string())
        })?;
        return Ok(atlas_root_dir_from_local_app_data(Path::new(
            &local_app_data,
        )));
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(crate::utils::path::home_dir()?
            .join("Library")
            .join("Application Support")
            .join("com.openai.atlas")
            .join("browser-data")
            .join("host"))
    }
}

fn discover_profiles_in_root(source: &ChromiumSource) -> Result<Vec<ChromiumProfileHistory>> {
    if !source.root_dir.exists() {
        return Ok(Vec::new());
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(&source.root_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let history_path = entry.path().join("History");
        if history_path.exists() {
            profiles.push(ChromiumProfileHistory {
                browser: source.browser.to_string(),
                profile: entry.file_name().to_string_lossy().to_string(),
                history_path,
            });
        }
    }

    profiles.sort_by(|left, right| left.profile.cmp(&right.profile));
    Ok(profiles)
}

fn read_profile_visits(
    browser: &str,
    profile: &str,
    history_path: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<BrowserVisitRecord>> {
    let temp_db_path = copy_history_db(browser, history_path)?;
    let result =
        read_profile_visits_from_copy(browser, profile, history_path, &temp_db_path, since);
    let _ = std::fs::remove_file(&temp_db_path);
    result
}

fn copy_history_db(browser: &str, history_path: &Path) -> Result<PathBuf> {
    let temp_path =
        std::env::temp_dir().join(format!("waylog-{browser}-{}.db", uuid::Uuid::new_v4()));
    std::fs::copy(history_path, &temp_path)?;
    Ok(temp_path)
}

fn read_profile_visits_from_copy(
    browser: &str,
    profile: &str,
    history_path: &Path,
    temp_db_path: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<BrowserVisitRecord>> {
    let connection = Connection::open(temp_db_path).map_err(|error| {
        WaylogError::Internal(format!("Failed to open {browser} History DB: {error}"))
    })?;

    let query = r#"
        SELECT
            v.id,
            v.visit_time,
            v.from_visit,
            v.transition,
            u.url,
            COALESCE(u.title, ''),
            COALESCE(u.visit_count, 0),
            COALESCE(u.typed_count, 0)
        FROM visits v
        JOIN urls u ON v.url = u.id
        WHERE (?1 IS NULL OR v.visit_time > ?1)
        ORDER BY v.visit_time ASC
    "#;

    let since_micros = since.map(chrome_datetime_to_micros);
    let mut statement = connection.prepare(query).map_err(|error| {
        WaylogError::Internal(format!(
            "Failed to prepare {browser} History query: {error}"
        ))
    })?;

    let rows = statement
        .query_map(params![since_micros], |row| {
            let visit_id: i64 = row.get(0)?;
            let visit_time: i64 = row.get(1)?;
            let from_visit: Option<i64> = row.get(2)?;
            let transition: Option<i64> = row.get(3)?;
            let url: String = row.get(4)?;
            let title: String = row.get(5)?;
            let visit_count: i64 = row.get(6)?;
            let typed_count: i64 = row.get(7)?;
            Ok((
                visit_id,
                visit_time,
                from_visit,
                transition,
                url,
                title,
                visit_count,
                typed_count,
            ))
        })
        .map_err(|error| {
            WaylogError::Internal(format!("Failed to query {browser} History: {error}"))
        })?;

    let mut visits = Vec::new();
    for row in rows {
        let (visit_id, visit_time, from_visit, transition, url, title, visit_count, typed_count) =
            row.map_err(|error| {
                WaylogError::Internal(format!("Failed to read {browser} History row: {error}"))
            })?;
        let visited_at = chrome_micros_to_datetime(visit_time)?;

        visits.push(BrowserVisitRecord {
            record_id: format!("{browser}:{profile}:{visit_time}:{url}:{visit_id}"),
            browser: browser.to_string(),
            profile: profile.to_string(),
            url,
            title,
            visited_at: crate::utils::time::format_local_rfc3339(&visited_at),
            visit_count: visit_count.max(0) as u32,
            typed_count: typed_count.max(0) as u32,
            transition: transition.map(|value| value.to_string()),
            referrer_visit_id: from_visit,
            source_db_path: history_path.display().to_string(),
        });
    }

    Ok(visits)
}

fn source_profile_key(browser: &str, profile: &str) -> String {
    format!("{browser}:{profile}")
}

fn chrome_micros_to_datetime(value: i64) -> Result<DateTime<Utc>> {
    let unix_micros = value
        .checked_sub(CHROME_EPOCH_OFFSET_MICROS)
        .ok_or_else(|| WaylogError::Internal("Chrome timestamp underflow".to_string()))?;
    Utc.timestamp_micros(unix_micros)
        .single()
        .ok_or_else(|| WaylogError::Internal("Invalid Chrome timestamp".to_string()))
}

fn chrome_datetime_to_micros(value: DateTime<Utc>) -> i64 {
    value.timestamp_micros() + CHROME_EPOCH_OFFSET_MICROS
}

#[cfg(target_os = "windows")]
fn chrome_root_dir_from_local_app_data(local_app_data: &Path) -> PathBuf {
    local_app_data
        .join("Google")
        .join("Chrome")
        .join("User Data")
}

#[cfg(target_os = "windows")]
fn atlas_root_dir_from_local_app_data(local_app_data: &Path) -> PathBuf {
    local_app_data
        .join("com.openai.atlas")
        .join("browser-data")
        .join("host")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_discover_profiles_finds_chrome_default_and_numbered_profiles() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::create_dir_all(temp_dir.path().join("Default")).unwrap();
        std::fs::write(temp_dir.path().join("Default").join("History"), "").unwrap();
        std::fs::create_dir_all(temp_dir.path().join("Profile 1")).unwrap();
        std::fs::write(temp_dir.path().join("Profile 1").join("History"), "").unwrap();
        std::fs::create_dir_all(temp_dir.path().join("Noise Dir")).unwrap();

        let collector =
            ChromiumHistoryCollector::with_sources(vec![("chrome", temp_dir.path().to_path_buf())]);
        let profiles = collector.discover_profiles().unwrap();
        let names = profiles
            .into_iter()
            .map(|item| item.profile)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["Default".to_string(), "Profile 1".to_string()]);
    }

    #[test]
    fn test_discover_profiles_finds_atlas_user_profile() {
        let temp_dir = TempDir::new().unwrap();
        let atlas_profile = "user-abc";
        std::fs::create_dir_all(temp_dir.path().join(atlas_profile)).unwrap();
        std::fs::write(temp_dir.path().join(atlas_profile).join("History"), "").unwrap();

        let collector =
            ChromiumHistoryCollector::with_sources(vec![("atlas", temp_dir.path().to_path_buf())]);
        let profiles = collector.discover_profiles().unwrap();

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].browser, "atlas");
        assert_eq!(profiles[0].profile, atlas_profile);
    }

    #[test]
    fn test_chrome_timestamp_round_trip() {
        let timestamp = DateTime::parse_from_rfc3339("2026-05-03T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let micros = chrome_datetime_to_micros(timestamp);
        let restored = chrome_micros_to_datetime(micros).unwrap();

        assert_eq!(restored, timestamp);
    }

    #[test]
    fn test_read_profile_visits_from_copy_reads_urls_and_visits() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("History");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE urls (
                    id INTEGER PRIMARY KEY,
                    url LONGVARCHAR,
                    title LONGVARCHAR,
                    visit_count INTEGER DEFAULT 0,
                    typed_count INTEGER DEFAULT 0
                );
                CREATE TABLE visits (
                    id INTEGER PRIMARY KEY,
                    url INTEGER,
                    visit_time INTEGER,
                    from_visit INTEGER,
                    transition INTEGER DEFAULT 0
                );
                "#,
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO urls (id, url, title, visit_count, typed_count) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![1i64, "https://example.com", "Example", 3i64, 1i64],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO visits (id, url, visit_time, from_visit, transition) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![9i64, 1i64, chrome_datetime_to_micros(Utc.with_ymd_and_hms(2026, 5, 3, 2, 0, 0).unwrap()), Option::<i64>::None, 7i64],
            )
            .unwrap();

        let visits =
            read_profile_visits_from_copy("atlas", "Default", &db_path, &db_path, None).unwrap();

        assert_eq!(visits.len(), 1);
        assert_eq!(visits[0].browser, "atlas");
        assert_eq!(visits[0].profile, "Default");
        assert_eq!(visits[0].url, "https://example.com");
        assert_eq!(visits[0].title, "Example");
        assert_eq!(visits[0].visit_count, 3);
        assert_eq!(visits[0].typed_count, 1);
    }

    #[tokio::test]
    async fn test_collect_visits_since_skips_unreadable_profile() {
        let temp_dir = TempDir::new().unwrap();

        let good_profile_dir = temp_dir.path().join("Default");
        std::fs::create_dir_all(&good_profile_dir).unwrap();
        let good_db_path = good_profile_dir.join("History");
        let connection = Connection::open(&good_db_path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE urls (
                    id INTEGER PRIMARY KEY,
                    url LONGVARCHAR,
                    title LONGVARCHAR,
                    visit_count INTEGER DEFAULT 0,
                    typed_count INTEGER DEFAULT 0
                );
                CREATE TABLE visits (
                    id INTEGER PRIMARY KEY,
                    url INTEGER,
                    visit_time INTEGER,
                    from_visit INTEGER,
                    transition INTEGER DEFAULT 0
                );
                "#,
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO urls (id, url, title, visit_count, typed_count) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![1i64, "https://example.com", "Example", 1i64, 0i64],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO visits (id, url, visit_time, from_visit, transition) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![1i64, 1i64, chrome_datetime_to_micros(Utc.with_ymd_and_hms(2026, 5, 3, 2, 0, 0).unwrap()), Option::<i64>::None, 0i64],
            )
            .unwrap();

        let broken_profile_dir = temp_dir.path().join("Broken");
        std::fs::create_dir_all(&broken_profile_dir).unwrap();
        std::fs::write(broken_profile_dir.join("History"), "not-a-sqlite-db").unwrap();

        let collector =
            ChromiumHistoryCollector::with_sources(vec![("atlas", temp_dir.path().to_path_buf())]);
        let result = collector
            .collect_visits_since(&HashMap::new())
            .await
            .unwrap();

        assert_eq!(result.visits.len(), 1);
        assert_eq!(result.visits[0].profile, "Default");
        assert_eq!(result.visits[0].browser, "atlas");
        assert_eq!(result.warnings.len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_windows_chrome_root_dir_uses_local_app_data() {
        let root = chrome_root_dir_from_local_app_data(Path::new("C:\\Users\\me\\AppData\\Local"));
        assert_eq!(
            root,
            PathBuf::from("C:\\Users\\me\\AppData\\Local")
                .join("Google")
                .join("Chrome")
                .join("User Data")
        );
    }
}
