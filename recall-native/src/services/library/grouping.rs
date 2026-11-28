//! Library groups: the clips the filter kept, split by a time window, day, week, month or year,
//! by game, or by a date span with games inside it.
//! Pure: reads only the records already in memory.
use std::collections::HashMap;

use super::calendar::{civil_from_days, day_label, days_from_civil, month_name, weekday};
use super::{ClipRecord, DateGroup, DayGroup, GroupBy, LocalStamp, SortOrder};

/// Which date group a clip falls in. `Day`, `Week`, `Month` and `Year` carry their first day
/// (days since 1970).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bucket {
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    ThisMonth,
    Day(i64),
    Week(i64),
    Month(i64),
    Year(i64),
}

/// The calendar edges of the time windows, worked out once per grouping.
struct Edges {
    today: i64,
    week: i64,
    month: i64,
}

impl Edges {
    fn new(today: LocalStamp) -> Self {
        let days = days_from_civil(today.year, today.month, today.day);
        Self { today: days, week: days - weekday(today.year, today.month, today.day) as i64, month: days_from_civil(today.year, today.month, 1) }
    }

    fn time(&self, days: i64) -> Bucket {
        match days {
            // A clock set wrong can date a clip after today: it is the newest there is.
            d if d >= self.today => Bucket::Today,
            d if d == self.today - 1 => Bucket::Yesterday,
            d if d >= self.week => Bucket::ThisWeek,
            d if d >= self.week - 7 => Bucket::LastWeek,
            d if d >= self.month => Bucket::ThisMonth,
            d => month_of(d),
        }
    }
}

fn month_of(days: i64) -> Bucket {
    let (year, month, _) = civil_from_days(days);
    Bucket::Month(days_from_civil(year, month, 1))
}

fn bucket(date: DateGroup, edges: &Edges, record: &ClipRecord) -> Option<Bucket> {
    let s = record.started;
    let days = days_from_civil(s.year, s.month, s.day);
    Some(match date {
        DateGroup::Off => return None,
        DateGroup::Time => edges.time(days),
        DateGroup::Day => Bucket::Day(days),
        DateGroup::Week => Bucket::Week(days - weekday(s.year, s.month, s.day) as i64),
        DateGroup::Month => month_of(days),
        DateGroup::Year => Bucket::Year(days_from_civil(s.year, 1, 1)),
    })
}

impl Bucket {
    /// First day the group covers: groups are put in time order by it.
    fn start(self, edges: &Edges) -> i64 {
        match self {
            Bucket::Today => edges.today,
            Bucket::Yesterday => edges.today - 1,
            Bucket::ThisWeek => edges.week,
            Bucket::LastWeek => edges.week - 7,
            Bucket::ThisMonth => edges.month,
            Bucket::Day(d) | Bucket::Week(d) | Bucket::Month(d) | Bucket::Year(d) => d,
        }
    }

    fn single_day(self) -> bool {
        matches!(self, Bucket::Today | Bucket::Yesterday | Bucket::Day(_))
    }

    /// The key names the dates the group covers, not "today": a group folded yesterday is
    /// another group today, so a clip saved now never lands in a folded one.
    fn label_and_key(self, today: LocalStamp, edges: &Edges) -> (String, String) {
        let (y, m, d) = civil_from_days(self.start(edges));
        let fixed = |label: &str, key: &str| (label.to_owned(), format!("time:{key}:{y:04}-{m:02}-{d:02}"));
        match self {
            Bucket::Today => fixed("Today", "today"),
            Bucket::Yesterday => fixed("Yesterday", "yesterday"),
            Bucket::ThisWeek => fixed("Earlier this week", "week"),
            Bucket::LastWeek => fixed("Last week", "last-week"),
            Bucket::ThisMonth => fixed("Earlier this month", "month"),
            Bucket::Day(d) => {
                let date = civil_from_days(d);
                (day_label(date, today), format!("day:{:04}-{:02}-{:02}", date.0, date.1, date.2))
            }
            Bucket::Week(monday) => (week_label(monday, today, edges), format!("week:{y:04}-{m:02}-{d:02}")),
            Bucket::Month(d) => {
                let (year, month, _) = civil_from_days(d);
                let name = month_name(month);
                let label = if year == today.year { name.to_owned() } else { format!("{name} {year}") };
                (label, format!("month:{year:04}-{month:02}"))
            }
            Bucket::Year(d) => {
                let year = civil_from_days(d).0;
                (year.to_string(), format!("year:{year:04}"))
            }
        }
    }
}

/// "This week", "Last week", "Week of 9 November", or with the year when it differs from today's.
fn week_label(monday: i64, today: LocalStamp, edges: &Edges) -> String {
    if monday == edges.week {
        return "This week".into();
    }
    if monday == edges.week - 7 {
        return "Last week".into();
    }
    let (year, month, day) = civil_from_days(monday);
    let name = month_name(month);
    if year == today.year { format!("Week of {day} {name}") } else { format!("Week of {day} {name} {year}") }
}

fn new_group(label: String, key: String, record: &ClipRecord, single_day: bool) -> DayGroup {
    DayGroup { label, key, date: record.started.date_key(), single_day, items: vec![], total_secs: 0.0, durations_complete: true }
}

fn push(group: &mut DayGroup, records: &[ClipRecord], index: usize) {
    group.items.push(index);
    match records[index].duration {
        Some(d) => group.total_secs += d,
        None => group.durations_complete = false,
    }
}

/// Splits `sorted` (already in `sort` order) into groups. Clips keep their sorted order inside a
/// group. Date groups run newest first (oldest first for `SortOrder::Oldest`), whatever the
/// sort; game groups, alone or inside a date group, come in the order their first clip does.
pub fn group_clips(records: &[ClipRecord], sorted: &[usize], today: LocalStamp, mode: GroupBy, sort: SortOrder) -> Vec<DayGroup> {
    let Some(&first) = sorted.first() else { return vec![] };
    let mut groups: Vec<DayGroup> = Vec::new();
    if mode.is_none() {
        let mut all = new_group("All clips".into(), "all".into(), &records[first], false);
        sorted.iter().for_each(|&i| push(&mut all, records, i));
        groups.push(all);
        return groups;
    }
    let edges = Edges::new(today);
    let mut at: HashMap<(Option<Bucket>, &str), usize> = HashMap::new();
    let mut starts: Vec<i64> = Vec::new();
    for &index in sorted {
        let record = &records[index];
        let b = bucket(mode.date, &edges, record);
        let game = if mode.game { record.game_key.as_str() } else { "" };
        let slot = *at.entry((b, game)).or_insert_with(|| {
            let (label, key) = match (b, mode.game) {
                (Some(b), false) => b.label_and_key(today, &edges),
                (Some(b), true) => {
                    let (label, key) = b.label_and_key(today, &edges);
                    (format!("{label} · {}", record.game), format!("{key}|game:{game}"))
                }
                (None, _) => (record.game.clone(), format!("game:{game}")),
            };
            groups.push(new_group(label, key, record, b.is_some_and(Bucket::single_day)));
            starts.push(b.map_or(0, |b| b.start(&edges)));
            groups.len() - 1
        });
        push(&mut groups[slot], records, index);
    }
    if mode.date == DateGroup::Off {
        return groups;
    }
    // Newest/Oldest already met the groups in time order; the other sorts did not. The sort is
    // stable: games inside one date group keep the order their first clip came in.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    if sort == SortOrder::Oldest {
        order.sort_by_key(|&g| starts[g]);
    } else {
        order.sort_by_key(|&g| std::cmp::Reverse(starts[g]));
    }
    let mut taken: Vec<Option<DayGroup>> = groups.into_iter().map(Some).collect();
    order.into_iter().filter_map(|g| taken[g].take()).collect()
}
