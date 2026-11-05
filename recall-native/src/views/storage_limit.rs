//! Storage-limit wording: the recorder's `storage_warning`, `storage_cleaned` and
//! `storage_preview` events as toast and confirm text (pure; tested without a window).
//! The recorder decides when to warn (80% and 90% of the limit, and over it with
//! nothing left that may go); clips only ever go to the Recycle Bin.
use crate::services::library::{LocalStamp, format_bytes};

/// GiB; 0 = no limit. The labels match Settings → Media → Storage limit.
pub const CAPS: [(u64, &str); 5] = [(0, "No limit"), (250, "250 GB"), (500, "500 GB"), (1000, "1 TB"), (2000, "2 TB")];
const GIB: u64 = 1 << 30;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/// Storage toasts stay up longer than the 3 s info toast: they ask the user to act.
pub const TOAST_SECS: u64 = 8;

/// The dry run the recorder answered for a limit the user just picked.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub cap_gb: u32,
    pub used_bytes: u64,
    pub removed: u64,
    pub freed_bytes: u64,
    /// Local stamp of the newest clip that would go.
    pub through: Option<LocalStamp>,
    /// When the dry run answered: an old answer is asked again before it moves anything.
    pub at: std::time::Instant,
}

/// How long the app waits for the recorder's dry run.
pub const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// How long a dry run's numbers stay good enough to confirm.
pub const CONFIRM_FRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// "1 TB" for a cap in bytes (the Settings label), else "N GB".
pub fn cap_label(cap_bytes: u64) -> String {
    let gb = cap_bytes / GIB;
    CAPS.iter().find(|(g, _)| *g == gb).map(|(_, l)| (*l).to_owned()).unwrap_or_else(|| format!("{gb} GB"))
}

/// "3 Oct", or "3 Oct 2025" outside this year.
pub fn short_date(at: LocalStamp, today: LocalStamp) -> String {
    let month = MONTHS.get(at.month.saturating_sub(1) as usize).copied().unwrap_or("?");
    if at.year == today.year { format!("{} {month}", at.day) } else { format!("{} {month} {}", at.day, at.year) }
}

fn percent(used: u64, cap: u64) -> u64 {
    if cap == 0 { 0 } else { (used as u128 * 100 / cap as u128) as u64 }
}

fn clips(n: u64) -> String {
    if n == 1 { "1 recording".into() } else { format!("{n} recordings") }
}

/// Toast after the limit moved clips: (title, detail).
pub fn cleaned(removed: u64, freed: u64, through: Option<LocalStamp>, cap: u64, today: LocalStamp) -> (String, String) {
    let title = match through {
        Some(at) => format!("Recordings from {} and earlier moved to the Recycle Bin", short_date(at, today)),
        None => format!("Moved {} to the Recycle Bin", clips(removed)),
    };
    let limit = if cap > 0 { format!(" · limit {}", cap_label(cap)) } else { String::new() };
    (title, format!("{}, {} freed{limit}. Restore them from the Recycle Bin.", clips(removed), format_bytes(freed)))
}

/// Toast before the limit is reached (80/90) or when it is stuck over it (100): (title, detail).
pub fn warning(level: u8, used: u64, cap: u64, next: Option<LocalStamp>, today: LocalStamp) -> (String, String) {
    let label = cap_label(cap);
    if level >= 100 {
        return (
            format!("Recordings are over the {label} limit"),
            format!("{} used. Nothing more can go: the rest are favourites, edited or new. Raise the limit in Settings → Media.", format_bytes(used)),
        );
    }
    let title = format!("Recordings are {}% of the {label} limit", percent(used, cap).min(99));
    let first = match next {
        Some(at) => format!("recordings from {} and earlier go to the Recycle Bin first", short_date(at, today)),
        None => "the oldest recordings go to the Recycle Bin".into(),
    };
    (title, format!("{} of {label} used. Past the limit, {first}. Favourites and edited clips stay.", format_bytes(used)))
}

/// Confirm bar for a limit that would move clips now: (question, detail, action).
pub fn confirm(p: &Preview, today: LocalStamp) -> (String, String, String) {
    let label = cap_label(u64::from(p.cap_gb) * GIB);
    let from = p.through.map(|at| format!(" from {} and earlier", short_date(at, today))).unwrap_or_default();
    (
        format!("Set the limit to {label}?"),
        format!("{}{from} ({}) go to the Recycle Bin now. Favourites and edited clips stay.", clips(p.removed), format_bytes(p.freed_bytes)),
        format!("Move {} and set {label}", if p.removed == 1 { "1" } else { "them" }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> LocalStamp {
        LocalStamp { year, month, day, ..LocalStamp::default() }
    }
    const TODAY: LocalStamp = LocalStamp { year: 2026, month: 11, day: 5, hour: 20, minute: 0, second: 0 };

    #[test]
    fn labels_and_dates() {
        assert_eq!(cap_label(1000 * GIB), "1 TB");
        assert_eq!(cap_label(250 * GIB), "250 GB");
        assert_eq!(cap_label(300 * GIB), "300 GB", "a hand-edited cap");
        assert_eq!(short_date(day(2026, 10, 3), TODAY), "3 Oct");
        assert_eq!(short_date(day(2025, 12, 31), TODAY), "31 Dec 2025");
    }

    #[test]
    fn cleaned_names_the_date() {
        let (t, d) = cleaned(4, 12_300_000_000, Some(day(2026, 10, 3)), 1000 * GIB, TODAY);
        assert_eq!(t, "Recordings from 3 Oct and earlier moved to the Recycle Bin");
        assert_eq!(d, "4 recordings, 12.30 GB freed · limit 1 TB. Restore them from the Recycle Bin.");
        let (t, _) = cleaned(1, 1, None, 0, TODAY);
        assert_eq!(t, "Moved 1 recording to the Recycle Bin");
    }

    #[test]
    fn warnings_say_how_full_and_what_goes_first() {
        let cap = 1000 * GIB;
        let (t, d) = warning(80, cap * 81 / 100, cap, Some(day(2026, 9, 14)), TODAY);
        assert_eq!(t, "Recordings are 81% of the 1 TB limit");
        assert!(d.contains("recordings from 14 Sep and earlier go to the Recycle Bin first"), "{d}");
        let (t, _) = warning(90, cap, cap, None, TODAY);
        assert_eq!(t, "Recordings are 99% of the 1 TB limit", "at the limit but not over it");
        let (t, d) = warning(100, cap + GIB, cap, None, TODAY);
        assert_eq!(t, "Recordings are over the 1 TB limit");
        assert!(d.contains("Raise the limit"));
    }

    #[test]
    fn confirm_wording() {
        let p = Preview { cap_gb: 1000, used_bytes: 2_140_000_000_000, removed: 240, freed_bytes: 1_100_000_000_000, through: Some(day(2026, 10, 3)), at: std::time::Instant::now() };
        let (q, d, a) = confirm(&p, TODAY);
        assert_eq!(q, "Set the limit to 1 TB?");
        assert_eq!(d, "240 recordings from 3 Oct and earlier (1.10 TB) go to the Recycle Bin now. Favourites and edited clips stay.");
        assert_eq!(a, "Move them and set 1 TB");
    }
}
