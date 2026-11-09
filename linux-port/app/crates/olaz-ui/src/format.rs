//! vi_VN formatting used by the screens: list times, day separators, byte counts.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};

pub fn local(ms: f64) -> DateTime<Local> {
    Local.timestamp_millis_opt(ms as i64).single().unwrap_or_else(now)
}

/// The current local time from [`olaz_core::clock`] (fixed during visual tests).
pub fn now() -> DateTime<Local> {
    Local.timestamp_millis_opt(olaz_core::clock::now_ms()).single().unwrap_or_else(Local::now)
}

const WEEKDAYS_SHORT: [&str; 7] = ["T2", "T3", "T4", "T5", "T6", "T7", "CN"];
const WEEKDAYS_LONG: [&str; 7] = ["Thứ Hai", "Thứ Ba", "Thứ Tư", "Thứ Năm", "Thứ Sáu", "Thứ Bảy", "Chủ Nhật"];

fn days_between(a: NaiveDate, b: NaiveDate) -> i64 {
    (b - a).num_days()
}

/// `ConversationListTime`: same day "HH:mm" · yesterday "Hôm qua" · 2–6 days → weekday "CN, T2…T7" · else "dd/MM".
pub fn list_time(ms: f64, now: DateTime<Local>) -> String {
    let d = local(ms);
    let diff = days_between(d.date_naive(), now.date_naive());
    match diff {
        i64::MIN..=0 => format!("{:02}:{:02}", d.hour(), d.minute()),
        1 => "Hôm qua".into(),
        2..=6 => WEEKDAYS_SHORT[d.weekday().num_days_from_monday() as usize].into(),
        _ => format!("{:02}/{:02}", d.day(), d.month()),
    }
}

/// Day separator label: "Hôm nay" / "Hôm qua" / this year "Thứ Ba 05/11" / other years "05/11/2025".
pub fn day_label(ms: f64, now: DateTime<Local>) -> String {
    let d = local(ms);
    match days_between(d.date_naive(), now.date_naive()) {
        0 => "Hôm nay".into(),
        1 => "Hôm qua".into(),
        _ if d.year() == now.year() => format!("{} {:02}/{:02}", WEEKDAYS_LONG[d.weekday().num_days_from_monday() as usize], d.day(), d.month()),
        _ => format!("{:02}/{:02}/{}", d.day(), d.month(), d.year()),
    }
}

pub fn hhmm(ms: f64) -> String {
    let d = local(ms);
    format!("{:02}:{:02}", d.hour(), d.minute())
}

pub fn same_day(a: f64, b: f64) -> bool {
    local(a).date_naive() == local(b).date_naive()
}

/// `ByteCountFormatter(.file)` in vi_VN: 1000-based, one decimal, comma separator ("12,3 MB").
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["byte", "KB", "MB", "GB", "TB"];
    if n < 1000 {
        return format!("{n} byte");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let s = if i == 1 { format!("{}", v.round() as u64) } else { format!("{:.1}", v).replace('.', ",") };
    format!("{s} {}", UNITS[i])
}

/// Call duration "m:ss" / "h:mm:ss".
pub fn clock(seconds: i64) -> String {
    let s = seconds.max(0);
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Grouped digits with dots (vi_VN): 12345 → "12.345".
pub fn grouped(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn list_times() {
        let now = at(2026, 11, 9, 15, 0); // Monday
        assert_eq!(list_time(at(2026, 11, 9, 8, 5).timestamp_millis() as f64, now), "08:05");
        assert_eq!(list_time(at(2026, 11, 8, 23, 0).timestamp_millis() as f64, now), "Hôm qua");
        assert_eq!(list_time(at(2026, 11, 3, 9, 0).timestamp_millis() as f64, now), "T3");
        assert_eq!(list_time(at(2026, 11, 2, 9, 0).timestamp_millis() as f64, now), "02/11");
        assert_eq!(list_time(at(2026, 11, 8, 9, 0).timestamp_millis() as f64, at(2026, 11, 10, 9, 0)), "CN");
    }

    #[test]
    fn day_labels() {
        let now = at(2026, 11, 9, 15, 0);
        assert_eq!(day_label(now.timestamp_millis() as f64, now), "Hôm nay");
        assert_eq!(day_label(at(2026, 11, 8, 1, 0).timestamp_millis() as f64, now), "Hôm qua");
        assert_eq!(day_label(at(2026, 11, 3, 1, 0).timestamp_millis() as f64, now), "Thứ Ba 03/11");
        assert_eq!(day_label(at(2025, 1, 2, 1, 0).timestamp_millis() as f64, now), "02/01/2025");
    }

    #[test]
    fn byte_counts() {
        assert_eq!(bytes(512), "512 byte");
        assert_eq!(bytes(12_345), "12 KB");
        assert_eq!(bytes(12_345_678), "12,3 MB");
        assert_eq!(bytes(337_901_830), "337,9 MB");
        assert_eq!(bytes(2_100_000_000), "2,1 GB");
        assert_eq!(clock(125), "2:05");
        assert_eq!(clock(3725), "1:02:05");
        assert_eq!(grouped(1234567), "1.234.567");
    }
}
