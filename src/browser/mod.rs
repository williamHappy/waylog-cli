pub mod chrome;

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BrowserVisitRecord {
    pub record_id: String,
    pub browser: String,
    pub profile: String,
    pub url: String,
    pub title: String,
    pub visited_at: String,
    pub visit_count: u32,
    pub typed_count: u32,
    pub transition: Option<String>,
    pub referrer_visit_id: Option<i64>,
    pub source_db_path: String,
}

impl BrowserVisitRecord {
    pub fn visited_at_utc(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.visited_at)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    pub fn local_date_key(&self) -> Option<String> {
        self.visited_at_utc()
            .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
    }
}
