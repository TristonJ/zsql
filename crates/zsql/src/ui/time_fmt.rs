//! Relative-time formatting for a script's last-modified time (e.g. "2w",
//! "1mo"), shown as the Open Script picker's and sidebar's library row meta.
//! Pure and gpui-free.

use std::time::{Duration, SystemTime};

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
const SECONDS_PER_WEEK: u64 = 7 * SECONDS_PER_DAY;
const SECONDS_PER_MONTH: u64 = 30 * SECONDS_PER_DAY;
const SECONDS_PER_YEAR: u64 = 365 * SECONDS_PER_DAY;

/// `then` relative to `now`, bucketed into the coarsest unit that makes sense
#[must_use]
pub fn relative_time(now: SystemTime, then: SystemTime) -> String {
    let elapsed = now.duration_since(then).unwrap_or(Duration::ZERO);
    let secs = elapsed.as_secs();
    if secs < SECONDS_PER_MINUTE {
        "now".to_owned()
    } else if secs < SECONDS_PER_HOUR {
        format!("{}m", secs / SECONDS_PER_MINUTE)
    } else if secs < SECONDS_PER_DAY {
        format!("{}h", secs / SECONDS_PER_HOUR)
    } else if secs < SECONDS_PER_WEEK {
        format!("{}d", secs / SECONDS_PER_DAY)
    } else if secs < SECONDS_PER_MONTH {
        format!("{}w", secs / SECONDS_PER_WEEK)
    } else if secs < SECONDS_PER_YEAR {
        format!("{}mo", secs / SECONDS_PER_MONTH)
    } else {
        format!("{}y", secs / SECONDS_PER_YEAR)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SECONDS_PER_DAY, SECONDS_PER_HOUR, SECONDS_PER_MINUTE, SECONDS_PER_MONTH, SECONDS_PER_WEEK,
        SECONDS_PER_YEAR, relative_time,
    };
    use std::time::{Duration, SystemTime};

    /// `seconds` before `now`. Takes `now` explicitly (rather than calling
    /// `SystemTime::now()` again internally) so a test's own fixed `now` and
    /// the `then` it compares against can never drift apart by however long
    /// the two calls happened to take.
    fn ago(now: SystemTime, seconds: u64) -> SystemTime {
        now - Duration::from_secs(seconds)
    }

    #[test]
    fn under_a_minute_reads_as_now() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 30)), "now");
    }

    #[test]
    fn minutes_bucket_shows_a_bare_m_suffix() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 5 * SECONDS_PER_MINUTE)), "5m");
    }

    #[test]
    fn hours_bucket_shows_a_bare_h_suffix() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 3 * SECONDS_PER_HOUR)), "3h");
    }

    #[test]
    fn days_bucket_shows_a_bare_d_suffix() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 3 * SECONDS_PER_DAY)), "3d");
    }

    #[test]
    fn weeks_bucket_shows_a_bare_w_suffix() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 2 * SECONDS_PER_WEEK)), "2w");
    }

    #[test]
    fn months_bucket_shows_a_mo_suffix() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 3 * SECONDS_PER_MONTH)), "3mo");
    }

    #[test]
    fn years_bucket_shows_a_bare_y_suffix() {
        let now = SystemTime::now();
        assert_eq!(relative_time(now, ago(now, 2 * SECONDS_PER_YEAR)), "2y");
    }

    #[test]
    fn a_time_in_the_future_reads_as_now_rather_than_underflowing() {
        let now = SystemTime::now();
        let future = now + Duration::from_secs(5);
        assert_eq!(relative_time(now, future), "now");
    }
}
