//! Pure civil-calendar arithmetic (proleptic Gregorian) and day-group labels.
use super::LocalStamp;

const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
pub(super) fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub(super) fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
    (year, month, day)
}

pub(super) fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

pub(super) fn valid_date(year: i32, month: u32, day: u32) -> bool {
    (1970..=2200).contains(&year) && (1..=12).contains(&month) && day >= 1 && day <= days_in_month(year, month)
}

/// 0 = Monday … 6 = Sunday. 1970-01-01 was a Thursday.
pub(super) fn weekday(year: i32, month: u32, day: u32) -> usize {
    (days_from_civil(year, month, day) + 3).rem_euclid(7) as usize
}

/// Wall-clock stamp for `secs` shifted by a fixed UTC offset (fallback + tests).
pub(super) fn stamp_from_unix_offset(secs: i64, offset_secs: i64) -> LocalStamp {
    let shifted = secs + offset_secs;
    let (year, month, day) = civil_from_days(shifted.div_euclid(86_400));
    let in_day = shifted.rem_euclid(86_400);
    LocalStamp { year, month, day, hour: (in_day / 3600) as u32, minute: (in_day % 3600 / 60) as u32, second: (in_day % 60) as u32 }
}

/// "Today", "Yesterday", "Saturday, 17 October", or with the year when it differs from today's.
pub(super) fn day_label(date: (i32, u32, u32), today: LocalStamp) -> String {
    let today_days = days_from_civil(today.year, today.month, today.day);
    let days = days_from_civil(date.0, date.1, date.2);
    if days == today_days {
        return "Today".into();
    }
    if days == today_days - 1 {
        return "Yesterday".into();
    }
    let (year, month, day) = date;
    let weekday = WEEKDAYS[weekday(year, month, day)];
    let month_name = MONTHS.get(month.saturating_sub(1) as usize).copied().unwrap_or("?");
    if year == today.year { format!("{weekday}, {day} {month_name}") } else { format!("{weekday}, {day} {month_name} {year}") }
}
