use std::time::{SystemTime, UNIX_EPOCH};

/// The current time as RFC 3339 UTC with milliseconds.
pub fn now() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before 1970");
    format_ms(d.as_millis() as i64)
}

fn format_ms(ms: i64) -> String {
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::format_ms;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_ms(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(format_ms(1_794_153_731_413), "2026-11-08T16:02:11.413Z");
    }
}
