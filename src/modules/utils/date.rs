//! Minimal RFC 7231 `IMF-fixdate` HTTP-date formatting (#44), used for the
//! `Last-Modified` header on the download endpoint
//! (`src/modules/router/middlewares.rs`).
//!
//! No date/time crate (`chrono`, `time`, `httpdate`, ...) is a dependency
//! of this crate, and `Cargo.toml` is out of scope for this change, so this
//! implements just the one algorithm needed: turning a `SystemTime` into
//! the exact `Sun, 06 Nov 1994 08:49:37 GMT` shape RFC 7231 §7.1.1.1
//! requires.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Index 0 is `1970-01-01` itself, which was a Thursday.
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Formats `time` as an RFC 7231 IMF-fixdate. Times before the Unix epoch
/// saturate to the epoch itself - there's no meaningful "negative"
/// `Last-Modified`/`Date` for this crate's purposes.
pub fn format_http_date(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days = (secs / 86_400) as i64;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60,
    );
    let weekday = WEEKDAYS[(days.rem_euclid(7)) as usize];
    let (year, month, day) = civil_from_days(days);

    format!(
        "{weekday}, {day:02} {month} {year:04} {hour:02}:{minute:02}:{second:02} GMT",
        month = MONTHS[(month - 1) as usize]
    )
}

/// Howard Hinnant's `civil_from_days` algorithm: converts a day count since
/// `1970-01-01` into a `(year, month, day)` proleptic-Gregorian civil date.
/// Public-domain, chosen here because it's a dozen lines of pure integer
/// arithmetic with no external date crate needed.
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// A fixed `Last-Modified` value for the permanently-immutable,
/// content-addressed `/api/images/files/{key}` download response (#44).
///
/// That response is written to storage exactly once and never mutated
/// afterwards (served `Cache-Control: ...immutable`), and its cache key
/// already commits to every input that could change it
/// (`CacheService::generate_key`, `src/services/cache/handler.rs`). There is
/// no real creation timestamp available in this module though -
/// `StorageService`, which owns the on-disk/S3 object metadata, is owned by
/// another agent for this change (see the final report) - so this reports
/// the Unix epoch: a value that is truthfully "not modified since" any date
/// a real client could plausibly send, for content that, by construction,
/// never changes once written.
pub fn immutable_resource_last_modified() -> String {
    format_http_date(UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_formats_correctly() {
        assert_eq!(
            format_http_date(UNIX_EPOCH),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
    }

    #[test]
    fn immutable_last_modified_is_the_epoch() {
        assert_eq!(
            immutable_resource_last_modified(),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
    }

    #[test]
    fn known_reference_date_from_rfc_7231() {
        // RFC 7231 §7.1.1.1's own example date.
        let time = UNIX_EPOCH + Duration::from_secs(784_111_777);
        assert_eq!(format_http_date(time), "Sun, 06 Nov 1994 08:49:37 GMT");
    }

    #[test]
    fn leap_day_formats_correctly() {
        // 2000-02-29 was a Tuesday, and 2000 is a leap year despite being
        // divisible by 100 (because it's also divisible by 400) - the exact
        // edge case `civil_from_days` has to get right. 11016 is the day
        // count from 1970-01-01 to 2000-02-29 inclusive of the former.
        let time = UNIX_EPOCH + Duration::from_secs(11_016 * 86_400);
        assert_eq!(format_http_date(time), "Tue, 29 Feb 2000 00:00:00 GMT");
    }

    #[test]
    fn time_before_epoch_saturates_to_epoch() {
        let before_epoch = UNIX_EPOCH.checked_sub(Duration::from_secs(1));
        if let Some(time) = before_epoch {
            assert_eq!(format_http_date(time), "Thu, 01 Jan 1970 00:00:00 GMT");
        }
    }
}
