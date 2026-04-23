use chrono::{DateTime, Local, Utc};

pub fn format_local_filename_timestamp(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .format("%Y-%m-%d_%H-%M-%S")
        .to_string()
}

pub fn format_local_display_timestamp(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %:z")
        .to_string()
}

pub fn format_local_rfc3339(dt: &DateTime<Utc>) -> String {
    dt.with_timezone(&Local).to_rfc3339()
}

pub fn format_system_time_as_local_rfc3339(time: std::time::SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .with_timezone(&Local)
        .to_rfc3339()
}

pub fn format_rfc3339_for_local_filename(timestamp: &str) -> String {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| format_local_filename_timestamp(&dt.with_timezone(&Utc)))
        .unwrap_or_else(|_| {
            timestamp
                .replace(':', "-")
                .replace('T', "_")
                .replace('Z', "")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_local_filename_timestamp_matches_local_timezone() {
        let dt = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let formatted = format_local_filename_timestamp(&dt);
        let expected = dt
            .with_timezone(&Local)
            .format("%Y-%m-%d_%H-%M-%S")
            .to_string();
        assert_eq!(formatted, expected);
    }

    #[test]
    fn test_format_local_display_timestamp_matches_local_timezone() {
        let dt = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let formatted = format_local_display_timestamp(&dt);
        let expected = dt
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S %:z")
            .to_string();
        assert_eq!(formatted, expected);
    }

    #[test]
    fn test_format_local_rfc3339_matches_local_timezone() {
        let dt = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let formatted = format_local_rfc3339(&dt);
        let expected = dt.with_timezone(&Local).to_rfc3339();
        assert_eq!(formatted, expected);
    }
}
