//! Days run on UTC so everyone in the world gets the same prompts at the same time.

use chrono::{DateTime, Days, NaiveDate};

/// Dive #1.
pub const LAUNCH: NaiveDate = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();

pub fn date_for(now_ms: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(now_ms)
        .expect("timestamp in range")
        .date_naive()
}

pub fn key(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// Days since the Unix epoch; used to seed the prompt schedule.
pub fn index(date: NaiveDate) -> i64 {
    (date - NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days()
}

pub fn dive_number(date: NaiveDate) -> i64 {
    (date - LAUNCH).num_days() + 1
}

/// Milliseconds timestamp of the next UTC midnight.
pub fn next_dive_at_ms(now_ms: i64) -> i64 {
    let tomorrow = date_for(now_ms) + Days::new(1);
    tomorrow
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_day_is_dive_one() {
        let ms = LAUNCH
            .and_hms_opt(13, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(dive_number(date_for(ms)), 1);
        assert_eq!(key(date_for(ms)), "2026-10-01");
        assert_eq!(
            next_dive_at_ms(ms),
            NaiveDate::from_ymd_opt(2026, 10, 2)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        );
    }
}
