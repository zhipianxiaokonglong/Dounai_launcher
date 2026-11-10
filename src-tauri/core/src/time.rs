//! 状态文件写 UTC RFC 3339 时间。core 不引第三方时间库，避免为一条格式串增加依赖。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 UTC 时间，格式 `YYYY-MM-DDTHH:MM:SSZ`。
pub fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    format_utc(secs)
}

fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// 把 UNIX 天数换成公历年月日，算法按 400 年周期分块，避免查表。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    if m <= 2 {
        y += 1;
    }
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_matches_rfc3339() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn known_timestamp_matches_rfc3339() {
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_utc(2_026_111_000), "2034-03-16T08:36:40Z");
        assert_eq!(format_utc(1_735_689_600), "2025-01-01T00:00:00Z");
    }

    #[test]
    fn leap_day_is_handled() {
        assert_eq!(format_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn utc_now_has_fixed_shape() {
        let s = utc_now();
        assert_eq!(s.len(), 20, "实际：{s}");
        assert!(s.ends_with('Z'));
        assert_eq!(s.as_bytes()[10], b'T');
    }
}
