//! Device-local coverage contract. Caller MUST hold the existing archive lock.
//! Git/network receipts and per-cycle heartbeat timestamps never belong here.
use crate::error::{Result, WaylogError};
use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub kind: String,
    pub enabled: bool,
    pub discovery: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Range {
    pub start: String,
    pub end_exclusive: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Observation {
    pub status: String,
    pub attempted_ranges: Vec<Range>,
    pub successful_ranges: Vec<Range>,
    pub record_count: Option<u64>,
    pub count_unit: String,
    pub archive_status: String,
    pub reason_code: Option<String>,
    pub evidence_scope: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_successful_ranges: Vec<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Day {
    pub data_version: Option<String>,
    pub sources: BTreeMap<String, Observation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Coverage {
    pub schema_version: u32,
    pub device_id: String,
    pub timezone: String,
    pub retention: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_date: Option<String>,
    pub sources: BTreeMap<String, Source>,
    pub days: BTreeMap<String, Day>,
}

pub fn cutoff(today: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(today.year() - 1, today.month(), today.day())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(today.year() - 1, 2, 28).unwrap())
}

pub fn day_range(date: NaiveDate) -> Range {
    Range {
        start: format!("{date}T00:00:00+08:00"),
        end_exclusive: format!("{}T00:00:00+08:00", date.succ_opt().unwrap()),
    }
}

impl Observation {
    pub fn pending(status: &str, reason: &str, unit: &str) -> Self {
        Self {
            status: status.into(),
            attempted_ranges: vec![],
            successful_ranges: vec![],
            record_count: None,
            count_unit: unit.into(),
            archive_status: "unknown".into(),
            reason_code: Some(reason.into()),
            evidence_scope: "retained-local-source-snapshot".into(),
            last_successful_ranges: vec![],
            last_failure_reason: None,
        }
    }
    pub fn validate(&self, date: NaiveDate) -> Result<()> {
        if !["messages", "visits"].contains(&self.count_unit.as_str())
            || self.evidence_scope.is_empty()
        {
            return Err(WaylogError::Internal(
                "Invalid coverage count unit/scope".into(),
            ));
        }
        let statuses = [
            "records", "zero", "partial", "pending", "unknown", "failed", "disabled", "stopped",
        ];
        if !statuses.contains(&self.status.as_str())
            || !["success", "failed", "unknown"].contains(&self.archive_status.as_str())
        {
            return Err(WaylogError::Internal("Invalid coverage status".into()));
        }
        for range in self
            .attempted_ranges
            .iter()
            .chain(&self.successful_ranges)
            .chain(&self.last_successful_ranges)
        {
            let start = chrono::DateTime::parse_from_rfc3339(&range.start)
                .map_err(|_| WaylogError::Internal("Invalid coverage range".into()))?;
            let end = chrono::DateTime::parse_from_rfc3339(&range.end_exclusive)
                .map_err(|_| WaylogError::Internal("Invalid coverage range".into()))?;
            let full = day_range(date);
            let lower = chrono::DateTime::parse_from_rfc3339(&full.start).unwrap();
            let upper = chrono::DateTime::parse_from_rfc3339(&full.end_exclusive).unwrap();
            if start >= end || start < lower || end > upper {
                return Err(WaylogError::Internal(
                    "Coverage range outside target day".into(),
                ));
            }
        }
        for successful in &self.successful_ranges {
            let covered = self.attempted_ranges.iter().any(|attempted| {
                let a = chrono::DateTime::parse_from_rfc3339(&attempted.start).unwrap();
                let b = chrono::DateTime::parse_from_rfc3339(&attempted.end_exclusive).unwrap();
                let start = chrono::DateTime::parse_from_rfc3339(&successful.start).unwrap();
                let end = chrono::DateTime::parse_from_rfc3339(&successful.end_exclusive).unwrap();
                start >= a && end <= b
            });
            if !covered {
                return Err(WaylogError::Internal(
                    "Successful range was not attempted".into(),
                ));
            }
        }
        if ["records", "zero"].contains(&self.status.as_str()) {
            if self.successful_ranges != vec![day_range(date)]
                || !self.attempted_ranges.contains(&day_range(date))
                || self.archive_status != "success"
                || self.record_count.is_none()
                || (self.status == "zero") != (self.record_count == Some(0))
            {
                return Err(WaylogError::Internal(
                    "Complete/zero coverage requires a full-day successful read and archive check"
                        .into(),
                ));
            }
        }
        Ok(())
    }
}

impl Coverage {
    pub fn empty(device: &str) -> Self {
        Self {
            schema_version: 1,
            device_id: device.into(),
            timezone: "Asia/Shanghai".into(),
            retention: "(previous-calendar-year-anniversary,today];Feb29-clamps-to-Feb28".into(),
            as_of_date: None,
            sources: BTreeMap::new(),
            days: BTreeMap::new(),
        }
    }

    pub fn validate_shape(&self, device: &str) -> Result<()> {
        if self.schema_version != 1
            || self.device_id != device
            || self.timezone != "Asia/Shanghai"
            || self.retention != "(previous-calendar-year-anniversary,today];Feb29-clamps-to-Feb28"
        {
            return Err(WaylogError::Internal(
                "Coverage identity/version/retention mismatch".into(),
            ));
        }
        if self
            .sources
            .values()
            .any(|source| !["session", "browser"].contains(&source.kind.as_str()))
        {
            return Err(WaylogError::Internal(
                "Unsupported coverage source kind".into(),
            ));
        }
        if let Some(date) = &self.as_of_date {
            NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| WaylogError::Internal("Invalid retention anchor date".into()))?;
        }
        for (date, day) in &self.days {
            let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| WaylogError::Internal("Invalid stored coverage date".into()))?;
            if let Some(version) = &day.data_version {
                if !version.starts_with("sha256:")
                    || version.len() != 71
                    || !version[7..].chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(WaylogError::Internal(
                        "Invalid stored coverage version".into(),
                    ));
                }
            }
            for (id, observation) in &day.sources {
                if !self.sources.contains_key(id) {
                    return Err(WaylogError::Internal("Unregistered stored source".into()));
                }
                observation.validate(date)?;
                if day.data_version.is_none()
                    && (["records", "zero"].contains(&observation.status.as_str())
                        || !observation.successful_ranges.is_empty())
                {
                    return Err(WaylogError::Internal(
                        "Unknown stored data version cannot prove coverage".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn merge(
        &mut self,
        device: &str,
        today: NaiveDate,
        roster: BTreeMap<String, Source>,
        observations: BTreeMap<String, Day>,
    ) -> Result<()> {
        self.validate_shape(device)?;
        let mut next = self.clone();
        next.merge_in_place(device, today, roster, observations)?;
        next.validate_shape(device)?;
        *self = next;
        Ok(())
    }

    fn merge_in_place(
        &mut self,
        device: &str,
        today: NaiveDate,
        roster: BTreeMap<String, Source>,
        observations: BTreeMap<String, Day>,
    ) -> Result<()> {
        if self.schema_version != 1 || self.device_id != device || self.timezone != "Asia/Shanghai"
        {
            return Err(WaylogError::Internal(
                "Coverage device/schema/timezone mismatch".into(),
            ));
        }
        for (date, day) in observations {
            let parsed = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                .map_err(|_| WaylogError::Internal("Invalid coverage date".into()))?;
            if parsed > today {
                return Err(WaylogError::Internal("Future coverage day".into()));
            }
            if parsed <= cutoff(today) {
                continue;
            }
            if let Some(version) = &day.data_version {
                if !version.starts_with("sha256:")
                    || version.len() != 71
                    || !version[7..].chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(WaylogError::Internal(
                        "Coverage requires a SHA256 data version".into(),
                    ));
                }
            } else if day.sources.values().any(|o| {
                ["records", "zero"].contains(&o.status.as_str()) || !o.successful_ranges.is_empty()
            }) {
                return Err(WaylogError::Internal(
                    "Unknown data version cannot prove successful coverage".into(),
                ));
            }
            for (source, observation) in &day.sources {
                if !roster.contains_key(source) && !self.sources.contains_key(source) {
                    return Err(WaylogError::Internal("Unregistered coverage source".into()));
                }
                observation.validate(parsed)?;
            }
            // Observations are authoritative only for supplied source IDs. Others
            // remain recorded, including failed/disabled or no-longer-discovered sources.
            let target = self.days.entry(date).or_insert(Day {
                data_version: day.data_version.clone(),
                sources: BTreeMap::new(),
            });
            if target.data_version != day.data_version {
                for previous in target.sources.values_mut() {
                    *previous = Observation::pending(
                        "pending",
                        "data-version-changed-recheck-required",
                        &previous.count_unit,
                    );
                }
            }
            target.data_version = day.data_version;
            for (id, mut value) in day.sources {
                if let Some(previous) = target.sources.get(&id) {
                    if value.successful_ranges.is_empty() {
                        value.last_successful_ranges = previous.last_successful_ranges.clone();
                    }
                    value.last_failure_reason = previous.last_failure_reason.clone();
                }
                if !value.successful_ranges.is_empty() {
                    value.last_successful_ranges = value.successful_ranges.clone();
                }
                if value.status == "failed" {
                    value.last_failure_reason = value.reason_code.clone();
                }
                target.sources.insert(id, value);
            }
        }
        if self
            .as_of_date
            .as_ref()
            .map(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap() > today)
            .unwrap_or(false)
        {
            return Err(WaylogError::Internal(
                "Clock moved behind coverage retention anchor".into(),
            ));
        }
        self.as_of_date = Some(today.to_string());
        self.sources.extend(roster);
        self.days.retain(|date, _| {
            NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map(|d| d > cutoff(today) && d <= today)
                .unwrap_or(false)
        });
        Ok(())
    }
}

pub async fn save_locked(archive: &Path, coverage: &Coverage) -> Result<bool> {
    coverage.validate_shape(&coverage.device_id)?;
    let path = archive.join("indexes/coverage.json");
    if fs::symlink_metadata(&path)
        .await
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(WaylogError::Internal("Coverage symlink refused".into()));
    }
    let mut bytes = serde_json::to_vec_pretty(coverage)?;
    bytes.push(b'\n');
    if fs::read(&path).await.ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    fs::create_dir_all(path.parent().unwrap()).await?;
    let temporary = path.with_file_name(format!(".coverage-{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temporary, &bytes).await?;
    // fsync before atomic replace. Same directory ensures one filesystem.
    let file = fs::OpenOptions::new().write(true).open(&temporary).await?;
    file.sync_all().await?;
    drop(file);
    if let Err(error) = fs::rename(&temporary, &path).await {
        let _ = fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(true)
}

pub async fn read(archive: &Path, device: &str) -> Result<Coverage> {
    let path = archive.join("indexes/coverage.json");
    match fs::read(path).await {
        Ok(bytes) => {
            let coverage: Coverage = serde_json::from_slice(&bytes)?;
            coverage.validate_shape(device)?;
            Ok(coverage)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Coverage::empty(device)),
        Err(error) => Err(error.into()),
    }
}

/// Canonical SHA256 over sorted relative file paths and per-file SHA256. Excludes
/// coverage and manifest timestamps; never contains log bodies or sensitive URLs.
pub async fn data_version(archive: &Path) -> Result<String> {
    let entries = walkdir::WalkDir::new(archive)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| WaylogError::Internal("Cannot enumerate archive assets".into()))?;
    if entries.iter().any(|e| e.file_type().is_symlink()) {
        return Err(WaylogError::Internal(
            "Archive symlink cannot prove data version".into(),
        ));
    }
    let mut paths: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| {
            let relative = p
                .strip_prefix(archive)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            relative.starts_with("sessions/")
                || relative.starts_with("browser-history/")
                || relative == "indexes/sessions.jsonl"
                || relative == "indexes/browser-history.jsonl"
        })
        .collect();
    paths.sort();
    let mut version = Sha256::new();
    for path in paths {
        let relative = path
            .strip_prefix(archive)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&path).await?;
        if relative.starts_with("indexes/") {
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| WaylogError::Internal("Unreadable archive index".into()))?;
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let value: serde_json::Value = serde_json::from_str(line)?;
                if !value.is_object() {
                    return Err(WaylogError::Internal(
                        "Archive index is not an object".into(),
                    ));
                }
            }
        }
        let digest = format!("{:x}", Sha256::digest(bytes));
        version.update(relative.as_bytes());
        version.update(b"\0");
        version.update(digest.as_bytes());
        version.update(b"\n");
    }
    Ok(format!("sha256:{:x}", version.finalize()))
}

/// Local heartbeat is outside the archive. Its timestamp never enters coverage.
pub async fn save_runtime(lock_file: &Path, device: &str, state: &str, at: &str) -> Result<()> {
    let parent = lock_file
        .parent()
        .ok_or_else(|| WaylogError::Internal("Missing local state directory".into()))?;
    let path = parent.join("collection-state.json");
    let temporary = parent.join(format!(".collection-state-{}.tmp", uuid::Uuid::new_v4()));
    let value =
        serde_json::json!({"schemaVersion":1,"deviceId":device,"state":state,"lastCycleAt":at});
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?).await?;
    if let Err(error) = fs::rename(&temporary, &path).await {
        let _ = fs::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(())
}

pub async fn mark_stopped_locked(archive: &Path, device: &str, today: NaiveDate) -> Result<()> {
    let mut coverage = read(archive, device).await?;
    if coverage.device_id != device {
        return Err(WaylogError::Internal("Coverage device mismatch".into()));
    }
    if let Some(day) = coverage.days.get_mut(&today.to_string()) {
        for (id, source) in &coverage.sources {
            if source.enabled {
                day.sources.insert(
                    id.clone(),
                    Observation::pending(
                        "stopped",
                        "explicit-stop",
                        if source.kind == "browser" {
                            "visits"
                        } else {
                            "messages"
                        },
                    ),
                );
            }
        }
        save_locked(archive, &coverage).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }
    fn version() -> Option<String> {
        Some(format!("sha256:{}", "a".repeat(64)))
    }
    fn roster() -> BTreeMap<String, Source> {
        [(
            "session:codex".into(),
            Source {
                kind: "session".into(),
                enabled: true,
                discovery: "found".into(),
            },
        )]
        .into()
    }
    fn observation(d: NaiveDate, count: u64) -> Observation {
        Observation {
            status: if count == 0 { "zero" } else { "records" }.into(),
            attempted_ranges: vec![day_range(d)],
            successful_ranges: vec![day_range(d)],
            record_count: Some(count),
            count_unit: "messages".into(),
            archive_status: "success".into(),
            reason_code: None,
            evidence_scope: "synthetic-complete-retained-snapshot".into(),
            last_successful_ranges: vec![],
            last_failure_reason: None,
        }
    }
    fn days(d: NaiveDate, o: Observation) -> BTreeMap<String, Day> {
        [(
            d.to_string(),
            Day {
                data_version: version(),
                sources: [("session:codex".into(), o)].into(),
            },
        )]
        .into()
    }
    #[test]
    fn zero_requires_proven_whole_day_not_no_new_messages() {
        let d = date("2026-10-13");
        let mut o = observation(d, 0);
        o.successful_ranges.clear();
        assert!(o.validate(d).is_err());
        o.status = "partial".into();
        assert!(o.validate(d).is_ok());
        o.status = "failed".into();
        o.record_count = None;
        assert!(o.validate(d).is_ok());
    }
    #[test]
    fn calendar_year_retention_and_leap_day_are_explicit() {
        assert_eq!(cutoff(date("2024-02-29")), date("2023-02-28"));
        assert_eq!(cutoff(date("2025-02-28")), date("2024-02-28"));
        let mut c = Coverage::empty("fixture-mac");
        c.days.insert(
            "2024-02-28".into(),
            Day {
                data_version: version(),
                sources: BTreeMap::new(),
            },
        );
        c.days.insert(
            "2024-02-29".into(),
            Day {
                data_version: version(),
                sources: BTreeMap::new(),
            },
        );
        c.merge("fixture-mac", date("2025-02-28"), roster(), BTreeMap::new())
            .unwrap();
        assert!(!c.days.contains_key("2024-02-28"));
        assert!(c.days.contains_key("2024-02-29"));
    }
    #[test]
    fn source_failures_disabled_and_history_do_not_disappear() {
        let mut c = Coverage::empty("fixture-mac");
        let d = date("2026-10-13");
        c.merge("fixture-mac", d, roster(), days(d, observation(d, 5)))
            .unwrap();
        let mut sources = BTreeMap::new();
        sources.insert(
            "browser:chrome".into(),
            Source {
                kind: "browser".into(),
                enabled: false,
                discovery: "disabled".into(),
            },
        );
        let another = [(
            d.to_string(),
            Day {
                data_version: version(),
                sources: [(
                    "browser:chrome".into(),
                    Observation::pending("disabled", "configured-off", "visits"),
                )]
                .into(),
            },
        )]
        .into();
        c.merge("fixture-mac", d, sources, another).unwrap();
        assert_eq!(c.days[&d.to_string()].sources.len(), 2);
        assert!(c.sources.contains_key("session:codex"));
    }
    #[test]
    fn device_and_future_dates_fail_closed() {
        let mut c = Coverage::empty("fixture-mac");
        let d = date("2026-10-13");
        assert!(c
            .merge("fixture-windows", d, roster(), BTreeMap::new())
            .is_err());
        assert!(c
            .merge(
                "fixture-mac",
                d,
                roster(),
                days(d.succ_opt().unwrap(), observation(d.succ_opt().unwrap(), 1))
            )
            .is_err());
    }
    #[test]
    fn ranges_outside_target_day_are_rejected() {
        let d = date("2026-10-13");
        let mut o = observation(d, 1);
        o.successful_ranges[0].end_exclusive = "2026-10-15T00:00:00+08:00".into();
        assert!(o.validate(d).is_err());
    }
    #[test]
    fn changed_data_version_invalidates_unsupplied_source_claims() {
        let d = date("2026-10-13");
        let mut c = Coverage::empty("fixture-mac");
        c.merge("fixture-mac", d, roster(), days(d, observation(d, 1)))
            .unwrap();
        let newer = [(
            d.to_string(),
            Day {
                data_version: Some(format!("sha256:{}", "b".repeat(64))),
                sources: BTreeMap::new(),
            },
        )]
        .into();
        c.merge("fixture-mac", d, BTreeMap::new(), newer).unwrap();
        assert_eq!(
            c.days[&d.to_string()].sources["session:codex"].status,
            "pending"
        );
    }
    #[tokio::test]
    async fn duplicate_receipts_are_byte_identical_and_atomic_reader_sees_valid_json() {
        let temp = tempfile::tempdir().unwrap();
        let d = date("2026-10-13");
        let mut c = Coverage::empty("fixture-mac");
        c.merge("fixture-mac", d, roster(), days(d, observation(d, 0)))
            .unwrap();
        assert!(save_locked(temp.path(), &c).await.unwrap());
        let p = temp.path().join("indexes/coverage.json");
        let meta = fs::metadata(&p).await.unwrap().modified().unwrap();
        assert!(!save_locked(temp.path(), &c).await.unwrap());
        assert_eq!(fs::metadata(&p).await.unwrap().modified().unwrap(), meta);
        assert_eq!(read(temp.path(), "fixture-mac").await.unwrap(), c);
        assert_eq!(
            std::fs::read_dir(temp.path().join("indexes"))
                .unwrap()
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn receipt_and_manifest_do_not_change_data_digest() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("sessions"))
            .await
            .unwrap();
        fs::write(temp.path().join("sessions/fake.md"), "fictional text\n")
            .await
            .unwrap();
        let a = data_version(temp.path()).await.unwrap();
        save_locked(temp.path(), &Coverage::empty("fixture-mac"))
            .await
            .unwrap();
        fs::write(
            temp.path().join("indexes/manifest.json"),
            "{\"updated_at\":\"fake\"}",
        )
        .await
        .unwrap();
        assert_eq!(data_version(temp.path()).await.unwrap(), a);
        fs::write(temp.path().join("sessions/fake.md"), "fictional text B\n")
            .await
            .unwrap();
        assert_ne!(data_version(temp.path()).await.unwrap(), a);
    }
    #[tokio::test]
    async fn malformed_existing_file_is_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("indexes"))
            .await
            .unwrap();
        fs::write(temp.path().join("indexes/coverage.json"), "broken")
            .await
            .unwrap();
        assert!(read(temp.path(), "fixture-mac").await.is_err());
        assert_eq!(
            fs::read_to_string(temp.path().join("indexes/coverage.json"))
                .await
                .unwrap(),
            "broken"
        );
    }
    #[test]
    fn stopped_unknown_and_failed_are_not_zero() {
        for state in ["stopped", "unknown", "failed", "pending", "disabled"] {
            let o = Observation::pending(state, "fixture", "messages");
            assert!(o.validate(date("2026-10-13")).is_ok());
            assert!(o.record_count.is_none());
        }
    }
    #[tokio::test]
    async fn local_heartbeat_and_explicit_stop_are_separate_from_synced_state() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("archive");
        let d = date("2026-10-13");
        let mut c = Coverage::empty("fixture-mac");
        c.merge("fixture-mac", d, roster(), days(d, observation(d, 2)))
            .unwrap();
        save_locked(&archive, &c).await.unwrap();
        let before = fs::read(archive.join("indexes/coverage.json"))
            .await
            .unwrap();
        save_runtime(
            &temp.path().join("archive-access.lock"),
            "fixture-mac",
            "running",
            "2026-10-13T12:00:00Z",
        )
        .await
        .unwrap();
        save_runtime(
            &temp.path().join("archive-access.lock"),
            "fixture-mac",
            "running",
            "2026-10-13T12:01:00Z",
        )
        .await
        .unwrap();
        assert_eq!(
            before,
            fs::read(archive.join("indexes/coverage.json"))
                .await
                .unwrap()
        );
        mark_stopped_locked(&archive, "fixture-mac", d)
            .await
            .unwrap();
        let stopped = read(&archive, "fixture-mac").await.unwrap();
        assert_eq!(
            stopped.days[&d.to_string()].sources["session:codex"].status,
            "stopped"
        );
    }
    #[test]
    fn malformed_updates_do_not_partially_mutate_history() {
        let d = date("2026-10-13");
        let mut c = Coverage::empty("fixture-mac");
        let before = c.clone();
        let mut update = days(d, observation(d, 2));
        update.insert(
            "2026-99-99".into(),
            Day {
                data_version: version(),
                sources: BTreeMap::new(),
            },
        );
        assert!(c.merge("fixture-mac", d, roster(), update).is_err());
        assert_eq!(c, before);
    }
    #[test]
    fn missing_archive_digest_cannot_claim_success() {
        let d = date("2026-10-13");
        let mut c = Coverage::empty("fixture-mac");
        let bad = [(
            d.to_string(),
            Day {
                data_version: None,
                sources: [("session:codex".into(), observation(d, 0))].into(),
            },
        )]
        .into();
        assert!(c.merge("fixture-mac", d, roster(), bad).is_err());
        let failed = [(
            d.to_string(),
            Day {
                data_version: None,
                sources: [(
                    "session:codex".into(),
                    Observation::pending("failed", "archive-digest-failed", "messages"),
                )]
                .into(),
            },
        )]
        .into();
        c.merge("fixture-mac", d, roster(), failed).unwrap();
    }
    #[test]
    fn schema_samples_deserialize_validate_and_remain_device_scoped() {
        for text in [
            include_str!("../../docs/examples/coverage-v1/wk-me.coverage.json"),
            include_str!("../../docs/examples/coverage-v1/wk-jzx.coverage.json"),
        ] {
            let receipt: Coverage = serde_json::from_str(text).unwrap();
            let mut checked = Coverage::empty(&receipt.device_id);
            checked
                .merge(
                    &receipt.device_id,
                    date("2026-10-14"),
                    receipt.sources.clone(),
                    receipt.days.clone(),
                )
                .unwrap();
            assert_eq!(checked.days.len(), 3);
        }
    }
    #[tokio::test]
    async fn malformed_archive_index_cannot_produce_a_success_digest() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("indexes"))
            .await
            .unwrap();
        fs::write(temp.path().join("indexes/sessions.jsonl"), "not-json\n")
            .await
            .unwrap();
        assert!(data_version(temp.path()).await.is_err());
    }
    #[test]
    fn failed_and_recovered_reads_retain_recent_outcomes_without_zeroing() {
        let d = date("2026-10-13");
        let mut c = Coverage::empty("fixture-mac");
        c.merge("fixture-mac", d, roster(), days(d, observation(d, 3)))
            .unwrap();
        c.merge(
            "fixture-mac",
            d,
            roster(),
            days(
                d,
                Observation::pending("failed", "fixture-read-failed", "messages"),
            ),
        )
        .unwrap();
        let failed = &c.days[&d.to_string()].sources["session:codex"];
        assert_eq!(failed.last_successful_ranges, vec![day_range(d)]);
        assert_eq!(
            failed.last_failure_reason.as_deref(),
            Some("fixture-read-failed")
        );
        c.merge("fixture-mac", d, roster(), days(d, observation(d, 3)))
            .unwrap();
        assert_eq!(
            c.days[&d.to_string()].sources["session:codex"]
                .last_failure_reason
                .as_deref(),
            Some("fixture-read-failed")
        );
    }
}
