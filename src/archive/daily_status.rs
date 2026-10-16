//! Lightweight operational checks, stored locally until the sync worker publishes.
//! Caller holds the existing archive lock. No source enumeration or archive hashes.
use super::coverage::{Coverage, DailySourceCheck};
use crate::error::Result;
use std::{collections::BTreeMap, path::Path};

pub async fn read_local(path: &Path, device: &str) -> Result<Coverage> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let document: serde_json::Value = serde_json::from_slice(&bytes)?;
            let value: Coverage =
                serde_json::from_value(document.get("receipt").unwrap_or(&document).clone())?;
            value.validate_shape(device)?;
            Ok(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Coverage::empty(device)),
        Err(e) => Err(e.into()),
    }
}

pub async fn record_local(
    path: &Path,
    device: &str,
    at: &str,
    roster: BTreeMap<String, super::coverage::Source>,
    checks: BTreeMap<String, DailySourceCheck>,
) -> Result<bool> {
    let mut local = read_local(path, device).await?;
    local.sources.extend(roster);
    // A disabled browser root also disables its previously discovered profiles.
    for root in ["browser:chrome", "browser:atlas"] {
        if local.sources.get(root).map(|s| !s.enabled).unwrap_or(false) {
            for (id, source) in local.sources.iter_mut() {
                if id.starts_with(&format!("{root}:")) {
                    source.enabled = false;
                }
            }
        }
    }
    if !local.observe_daily(at, checks)? {
        return Ok(false);
    }
    // Control revision is local only; preserve it atomically with the receipt.
    let mut document: serde_json::Value = match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e.into()),
    };
    let revision = document
        .get("controlRevision")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    document = serde_json::json!({"schemaVersion":1,"deviceId":device,"controlRevision":revision,"receipt":local});
    let mut bytes = serde_json::to_vec_pretty(&document)?;
    bytes.push(b'\n');
    let parent = path.parent().unwrap();
    tokio::fs::create_dir_all(parent).await?;
    let temp = parent.join(format!(".daily-status-{}.tmp", uuid::Uuid::new_v4()));
    tokio::fs::write(&temp, bytes).await?;
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&temp)
        .await?;
    file.sync_all().await?;
    drop(file);
    if let Err(e) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(temp).await;
        return Err(e.into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::coverage::{Coverage, Source};
    use super::*;
    fn checks(id: &str, check: DailySourceCheck) -> BTreeMap<String, DailySourceCheck> {
        BTreeMap::from([(id.into(), check)])
    }
    #[test]
    fn actual_checks_carry_unresolved_failures_and_recover_only_same_source() {
        let mut c = Coverage::empty("fictional");
        c.observe_daily(
            "2026-10-14T16:00:00Z",
            checks(
                "session:codex",
                DailySourceCheck::failed("parse", "invalid_json"),
            ),
        )
        .unwrap();
        assert!(c.daily_checks.contains_key("2026-10-15"));
        c.observe_daily(
            "2026-10-16T01:00:00+08:00",
            checks("session:claude", DailySourceCheck::ok()),
        )
        .unwrap();
        assert_eq!(c.daily_checks["2026-10-16"].result, "failed");
        c.observe_daily(
            "2026-10-16T02:00:00+08:00",
            checks("session:codex", DailySourceCheck::ok()),
        )
        .unwrap();
        assert_eq!(c.daily_checks["2026-10-16"].result, "ok");
        assert!(c
            .observe_daily(
                "2026-10-15T01:00:00+08:00",
                checks("session:codex", DailySourceCheck::ok())
            )
            .is_err());
    }
    #[test]
    fn empty_cycle_no_fake_day_and_explicit_false_is_preserved() {
        let mut c = Coverage::empty("fictional");
        c.collection_enabled = Some(false);
        assert!(!c
            .observe_daily("2026-10-15T01:00:00+08:00", BTreeMap::new())
            .unwrap());
        assert!(c.daily_checks.is_empty());
        c.observe_daily(
            "2026-10-15T01:00:00+08:00",
            checks("session:codex", DailySourceCheck::ok()),
        )
        .unwrap();
        assert_eq!(c.collection_enabled, Some(false));
    }
    #[test]
    fn disabled_source_and_annual_pruning() {
        let mut c = Coverage::empty("fictional");
        c.observe_daily(
            "2025-10-15T01:00:00+08:00",
            checks(
                "session:codex",
                DailySourceCheck::failed("read", "unreadable"),
            ),
        )
        .unwrap();
        c.sources.insert(
            "session:codex".into(),
            Source {
                kind: "session".into(),
                enabled: false,
                discovery: "explicit".into(),
            },
        );
        c.observe_daily(
            "2026-10-15T01:00:00+08:00",
            checks("session:claude", DailySourceCheck::ok()),
        )
        .unwrap();
        assert_eq!(c.daily_checks.len(), 1);
        assert_eq!(c.daily_checks["2026-10-15"].result, "ok");
        c.validate_shape("fictional").unwrap();
    }
    #[test]
    fn minimal_contract_and_reason_codes() {
        let c:Coverage=serde_json::from_str(r#"{"schemaVersion":1,"deviceId":"fictional","timezone":"Asia/Shanghai","collectionEnabled":true,"dailyChecks":{}}"#).unwrap();
        c.validate_shape("fictional").unwrap();
        let mut c = c;
        assert!(c
            .observe_daily(
                "2026-10-15T01:00:00+08:00",
                checks(
                    "session:codex",
                    DailySourceCheck::failed("read", "bad.code")
                )
            )
            .is_err());
        assert!(serde_json::from_str::<Coverage>(r#"{"schemaVersion":1,"deviceId":"fictional","timezone":"Asia/Shanghai","collectionEnabled":null}"#).is_err());
    }
    #[test]
    fn legacy_read_compatibility_and_recovery_do_not_claim_day_proof() {
        use super::super::coverage::{Day, Observation};
        let mut c = Coverage::empty("fictional");
        c.sources.insert(
            "session:codex".into(),
            Source {
                kind: "session".into(),
                enabled: true,
                discovery: "fictional".into(),
            },
        );
        let row = Observation::pending("failed", "old_failure", "messages");
        c.days.insert(
            "2026-10-15".into(),
            Day {
                data_version: None,
                sources: BTreeMap::from([("session:codex".into(), row)]),
            },
        );
        let old = serde_json::to_vec(&c).unwrap();
        let mut c: Coverage = serde_json::from_slice(&old).unwrap();
        c.observe_daily(
            "2026-10-15T10:00:00+08:00",
            checks("session:codex", DailySourceCheck::ok()),
        )
        .unwrap();
        c.validate_shape("fictional").unwrap();
        let row = &c.days["2026-10-15"].sources["session:codex"];
        assert_eq!(row.status, "unknown");
        assert_eq!(row.last_failure_reason.as_deref(), Some("old_failure"));
        assert!(row.successful_ranges.is_empty());
        assert!(c.days["2026-10-15"].data_version.is_none());
        assert!(
            serde_json::from_str::<DailySourceCheck>(r#"{"result":"failed","stage":null}"#)
                .is_err()
        );
    }

    #[tokio::test]
    async fn native_local_control_wrapper_preserves_revision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.json");
        let local = serde_json::json!({"schemaVersion":1,"deviceId":"fictional","controlRevision":4,"receipt":Coverage::empty("fictional")});
        tokio::fs::write(&path, serde_json::to_vec(&local).unwrap())
            .await
            .unwrap();
        record_local(
            &path,
            "fictional",
            "2026-10-15T01:00:00+08:00",
            BTreeMap::new(),
            checks("session:codex", DailySourceCheck::ok()),
        )
        .await
        .unwrap();
        let result: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(result["controlRevision"], 4);
        assert_eq!(
            read_local(&path, "fictional")
                .await
                .unwrap()
                .daily_checks
                .len(),
            1
        );
    }
}
