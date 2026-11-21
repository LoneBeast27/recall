//! Library groups: the time windows, months, years and games `group_clips` makes.
use super::*;

fn stamp(year: i32, month: u32, day: u32, hour: u32) -> LocalStamp {
    LocalStamp { year, month, day, hour, minute: 0, second: 0 }
}

fn clip(game: &str, started: LocalStamp, secs: f64, bytes: u64) -> ClipRecord {
    let name = format!("{game}__{:04}-{:02}-{:02}__{:02}-00-00.mp4", started.year, started.month, started.day, started.hour);
    let (game_key, game) = normalize_game(game);
    ClipRecord {
        path: format!("Z:\\none\\{name}"),
        file_name: name,
        bytes,
        mtime: 0,
        thumb: None,
        game_key,
        game,
        started,
        duration: Some(secs),
        edited: false,
        favourite: false,
        title: None,
        played: None,
    }
}

/// Wednesday 21 October 2026: the week started Monday the 19th.
const TODAY: LocalStamp = LocalStamp { year: 2026, month: 10, day: 21, hour: 12, minute: 0, second: 0 };

fn library() -> Vec<ClipRecord> {
    vec![
        clip("VALORANT", stamp(2026, 10, 21, 10), 60.0, 5),
        clip("VALORANT", stamp(2026, 10, 20, 22), 30.0, 9),
        clip("Rocket-League", stamp(2026, 10, 19, 21), 10.0, 1),
        clip("Rocket-League", stamp(2026, 10, 18, 20), 20.0, 2),
        clip("VALORANT", stamp(2026, 10, 12, 20), 5.0, 3),
        clip("Apex-Legends", stamp(2026, 10, 11, 20), 5.0, 4),
        clip("Apex-Legends", stamp(2026, 10, 2, 20), 5.0, 7),
        clip("VALORANT", stamp(2026, 9, 30, 20), 5.0, 6),
        clip("Rocket-League", stamp(2025, 12, 31, 20), 5.0, 8),
    ]
}

fn labels(groups: &[DayGroup]) -> Vec<&str> {
    groups.iter().map(|g| g.label.as_str()).collect()
}

fn grouped(mode: GroupBy, sort: SortOrder) -> (Vec<ClipRecord>, Vec<DayGroup>) {
    let records = library();
    let sorted = filter_sort(&records, &Filter::default(), sort);
    let groups = group_clips(&records, &sorted, TODAY, mode, sort);
    (records, groups)
}

#[test]
fn time_windows_from_today_back_to_months() {
    let (_, groups) = grouped(GroupBy::Time, SortOrder::Newest);
    assert_eq!(labels(&groups), ["Today", "Yesterday", "Earlier this week", "Last week", "Earlier this month", "September", "December 2025"]);
    let sizes: Vec<usize> = groups.iter().map(|g| g.items.len()).collect();
    assert_eq!(sizes, [1, 1, 1, 2, 2, 1, 1]);
    let one_day: Vec<bool> = groups.iter().map(|g| g.single_day).collect();
    assert_eq!(one_day, [true, true, false, false, false, false, false]);
    assert_eq!(groups[3].total_secs, 25.0, "Sunday the 18th and Monday the 12th are last week");
    assert_eq!(groups[0].key, "time:today:2026-10-21");
    assert_eq!(groups[3].key, "time:last-week:2026-10-12");
    // The next day, the clip of the 21st is Yesterday and today's group has another key.
    let next = LocalStamp { day: 22, ..TODAY };
    let records = library();
    let sorted = filter_sort(&records, &Filter::default(), SortOrder::Newest);
    let later = group_clips(&records, &sorted, next, GroupBy::Time, SortOrder::Newest);
    assert_eq!(later[0].label, "Yesterday");
    assert_ne!(later[0].key, groups[0].key);
    assert!(later.iter().all(|g| g.key != "time:today:2026-10-21"), "a fold of yesterday's Today does not carry over");
}

#[test]
fn on_a_monday_yesterday_belongs_to_last_week_and_shows_as_yesterday() {
    let monday = LocalStamp { day: 19, ..TODAY };
    let records = vec![clip("VALORANT", stamp(2026, 10, 18, 10), 1.0, 1), clip("VALORANT", stamp(2026, 10, 13, 10), 1.0, 1), clip("VALORANT", stamp(2026, 10, 3, 10), 1.0, 1)];
    let sorted = filter_sort(&records, &Filter::default(), SortOrder::Newest);
    let groups = group_clips(&records, &sorted, monday, GroupBy::Time, SortOrder::Newest);
    assert_eq!(labels(&groups), ["Yesterday", "Last week", "Earlier this month"]);
    // A clock set ahead dates a clip after today: it counts as today, not as a group of its own.
    let ahead = vec![clip("VALORANT", stamp(2026, 10, 25, 10), 1.0, 1)];
    assert_eq!(labels(&group_clips(&ahead, &[0], TODAY, GroupBy::Time, SortOrder::Newest)), ["Today"]);
}

#[test]
fn months_years_and_days() {
    let (_, months) = grouped(GroupBy::Month, SortOrder::Newest);
    assert_eq!(labels(&months), ["October", "September", "December 2025"]);
    assert_eq!(months[0].key, "month:2026-10");
    assert_eq!(months[0].items.len(), 7);
    let (_, years) = grouped(GroupBy::Year, SortOrder::Newest);
    assert_eq!(labels(&years), ["2026", "2025"]);
    let (_, days) = grouped(GroupBy::Day, SortOrder::Newest);
    assert_eq!(days.len(), 9);
    assert_eq!(days[2].label, "Monday, 19 October");
    assert_eq!(days[2].key, "day:2026-10-19");
    assert!(days.iter().all(|g| g.single_day));
}

#[test]
fn oldest_first_reverses_the_groups_and_the_clips_inside() {
    let (records, groups) = grouped(GroupBy::Month, SortOrder::Oldest);
    assert_eq!(labels(&groups), ["December 2025", "September", "October"]);
    let october: Vec<u32> = groups[2].items.iter().map(|&i| records[i].started.day).collect();
    assert_eq!(october, [2, 11, 12, 18, 19, 20, 21]);
}

#[test]
fn a_size_sort_keeps_time_groups_in_time_order_and_sorts_inside_them() {
    let (records, groups) = grouped(GroupBy::Month, SortOrder::Largest);
    assert_eq!(labels(&groups), ["October", "September", "December 2025"], "the largest clip (2025) does not pull its year first");
    let sizes: Vec<u64> = groups[0].items.iter().map(|&i| records[i].bytes).collect();
    assert_eq!(sizes, [9, 7, 5, 4, 3, 2, 1]);
}

#[test]
fn games_come_in_the_order_of_their_first_clip() {
    let (records, groups) = grouped(GroupBy::Game, SortOrder::Newest);
    assert_eq!(labels(&groups), ["VALORANT", "Rocket League", "Apex Legends"]);
    assert_eq!(groups[0].items.len(), 4);
    assert_eq!(groups[1].key, "game:rocketleague");
    assert!(groups.iter().all(|g| !g.single_day));
    assert!(groups[0].items.windows(2).all(|w| records[w[0]].started > records[w[1]].started), "newest first inside a game");
    let (_, by_size) = grouped(GroupBy::Game, SortOrder::Largest);
    assert_eq!(labels(&by_size), ["VALORANT", "Rocket League", "Apex Legends"], "VALORANT holds the largest clip (9)");
    let (_, oldest) = grouped(GroupBy::Game, SortOrder::Oldest);
    assert_eq!(labels(&oldest), ["Rocket League", "VALORANT", "Apex Legends"]);
}

#[test]
fn every_mode_keeps_every_clip_once() {
    for mode in GroupBy::ALL {
        for sort in [SortOrder::Newest, SortOrder::Oldest, SortOrder::Longest, SortOrder::Largest] {
            let (records, groups) = grouped(mode, sort);
            let mut seen: Vec<usize> = groups.iter().flat_map(|g| g.items.iter().copied()).collect();
            seen.sort_unstable();
            assert_eq!(seen, (0..records.len()).collect::<Vec<_>>(), "{mode:?} {sort:?}");
            assert!(groups.iter().all(|g| !g.items.is_empty()));
        }
        assert!(group_clips(&library(), &[], TODAY, mode, SortOrder::Newest).is_empty());
    }
}

#[test]
fn group_words_round_trip() {
    for mode in GroupBy::ALL {
        assert_eq!(GroupBy::from_word(mode.word()), Some(mode));
    }
    assert_eq!(GroupBy::from_word("week"), None);
    assert_eq!(short_month(9), "Sep");
}
