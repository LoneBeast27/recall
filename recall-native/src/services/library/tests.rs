//! Pure-function tests: names, parsing, calendar, grouping, sort/filter, formatting.
use super::calendar::{civil_from_days, day_label, days_from_civil, stamp_from_unix_offset, weekday};
use super::*;

fn stamp(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> LocalStamp {
    LocalStamp { year, month, day, hour, minute, second }
}

fn record(name: &str, started: LocalStamp, duration: Option<f64>, bytes: u64) -> ClipRecord {
    let prefix = parse_file_name(name).map(|(p, _)| p).unwrap_or_else(|| name.into());
    let (game_key, game) = normalize_game(&prefix);
    ClipRecord {
        path: format!("Z:\\none\\{name}"),
        file_name: name.into(),
        bytes,
        mtime: 0,
        thumb: None,
        game_key,
        game,
        started,
        duration,
        edited: false,
        favourite: false,
        title: None,
    }
}

#[test]
fn normalize_covers_the_real_inventory() {
    let cases = [
        ("VALORANT", "valorant", "VALORANT"),
        ("Valorant", "valorant", "VALORANT"),
        ("Rocket-League", "rocketleague", "Rocket League"),
        ("Rocket-League®", "rocketleague", "Rocket League"),
        ("Apex-Legends", "apexlegends", "Apex Legends"),
        ("Overwatch", "overwatch", "Overwatch"),
        ("Overwatch®-2", "overwatch2", "Overwatch 2"),
        ("Marvel-Rivals", "marvelrivals", "Marvel Rivals"),
        ("DESKTOPCAPTURE", "desktop", "Desktop"),
        ("Desktop", "desktop", "Desktop"),
        ("Genshin-Impact", "genshinimpact", "Genshin Impact"),
        ("GenshinImpact", "genshinimpact", "Genshin Impact"),
        ("Battlefield™-6", "battlefield6", "Battlefield 6"),
        ("Counter-Strike-2", "counterstrike2", "Counter-Strike 2"),
        ("Tom-Clancys-Rainbow-Six-Siege", "tomclancysrainbowsixsiege", "Rainbow Six Siege"),
        ("Counter-Strike-Global-Offensive", "counterstrikeglobaloffensive", "Counter-Strike: Global Offensive"),
        ("Overcooked-2", "overcooked2", "Overcooked 2"),
        ("Phasmophobia", "phasmophobia", "Phasmophobia"),
        ("Lethal-Company", "lethalcompany", "Lethal Company"),
        ("Forza-Horizon-6", "forzahorizon6", "Forza Horizon 6"),
        ("ROUNDS", "rounds", "ROUNDS"),
        ("Marvel’s-Spider-Man-Remastered", "marvelsspidermanremastered", "Marvel's Spider-Man Remastered"),
        ("Call-of-Duty", "callofduty", "Call of Duty"),
        ("THE-FINALS", "thefinals", "THE FINALS"),
        ("TerraTech", "terratech", "TerraTech"),
        ("Super-Battle-Golf", "superbattlegolf", "Super Battle Golf"),
        ("Make-Way", "makeway", "Make Way"),
        ("Chameleon", "chameleon", "Chameleon"),
        ("agentic-ide", "agenticide", "Agentic IDE"),
        ("Recall-Hub", "recallhub", "Recall Hub"),
        ("Some_Game--Name©", "somegamename", "Some Game Name"),
        ("  __ - ", "unknown", "Unknown"),
    ];
    for (prefix, key, display) in cases {
        assert_eq!(normalize_game(prefix), (key.to_owned(), display.to_owned()), "{prefix}");
    }
}

#[test]
fn parse_handles_every_shape_on_the_drive() {
    let cases = [
        ("VALORANT__2026-10-17__22-34-31-275__3d260146faca4115835ba29741d71fe3.mp4", "VALORANT", stamp(2026, 10, 17, 22, 34, 31)),
        ("Forza-Horizon-6__2026-06-02__10-01-55.mp4", "Forza-Horizon-6", stamp(2026, 6, 2, 10, 1, 55)),
        ("Forza-Horizon-6__2026-06-06__13-06-02_trim.mp4", "Forza-Horizon-6", stamp(2026, 6, 6, 13, 6, 2)),
        ("Apex-Legends__2023-08-27__23-23-00 BLIND FUZE.mp4", "Apex-Legends", stamp(2023, 8, 27, 23, 23, 0)),
        ("Valorant_2023_01_27_10_10_11.mp4", "Valorant", stamp(2023, 1, 27, 10, 10, 11)),
        ("Battlefield™-6__2025-10-11__20-15-00.mp4", "Battlefield™-6", stamp(2025, 10, 11, 20, 15, 0)),
        ("Marvel’s-Spider-Man-Remastered__2024-01-02__03-04-05.mp4", "Marvel’s-Spider-Man-Remastered", stamp(2024, 1, 2, 3, 4, 5)),
        ("Overwatch®-2__2024-02-29__00-00-59.mp4", "Overwatch®-2", stamp(2024, 2, 29, 0, 0, 59)),
        ("Game__2026-10-17.mp4", "Game", stamp(2026, 10, 17, 0, 0, 0)),
        ("Game__2026-10-17__25-00-00.mp4", "Game", stamp(2026, 10, 17, 0, 0, 0)),
        ("Rocket League 2024.01.27 - 22.10.11.03.DVR.mp4", "Rocket League", stamp(2024, 1, 27, 22, 10, 11)),
        ("Rocket League 2024-01-27 22-10-11.mp4", "Rocket League", stamp(2024, 1, 27, 22, 10, 11)),
        ("2024-01-27 22-10-11.mkv", "", stamp(2024, 1, 27, 22, 10, 11)),
    ];
    for (name, prefix, started) in cases {
        assert_eq!(parse_file_name(name), Some((prefix.to_owned(), started)), "{name}");
    }
    for name in ["lethal company moment.mp4", "clip.mp4", "Game__2026-13-01__10-00-00.mp4", "Game__2025-02-29__10-00-00.mp4", "Game__20261017__1000.mp4", ""] {
        assert_eq!(parse_file_name(name), None, "{name}");
    }
}

#[test]
fn fallback_prefix_for_undated_names() {
    assert_eq!(names::fallback_prefix("lethal company moment.mp4"), "lethal company moment");
    assert_eq!(names::fallback_prefix("Odd__name.mp4"), "Odd");
    assert_eq!(names::fallback_prefix("noext"), "noext");
}

#[test]
fn calendar_arithmetic_round_trips() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    for days in (-1000..30_000).step_by(17) {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }
    assert_eq!(weekday(2026, 10, 17), 5, "17 Oct 2026 is a Saturday");
    assert_eq!(weekday(1970, 1, 1), 3, "Thursday");
    assert_eq!(stamp_from_unix_offset(0, 19_800), stamp(1970, 1, 1, 5, 30, 0));
    // 2026-10-17 22:34:31 IST = 17:04:31 UTC.
    let utc = (days_from_civil(2026, 10, 17) * 86_400) + 17 * 3600 + 4 * 60 + 31;
    assert_eq!(stamp_from_unix_offset(utc, 19_800), stamp(2026, 10, 17, 22, 34, 31));
}

#[test]
fn day_labels_relative_to_a_fixed_today() {
    let today = stamp(2026, 10, 18, 9, 0, 0);
    assert_eq!(day_label((2026, 10, 18), today), "Today");
    assert_eq!(day_label((2026, 10, 17), today), "Yesterday");
    assert_eq!(day_label((2026, 10, 16), today), "Friday, 16 October");
    assert_eq!(day_label((2026, 10, 19), today), "Monday, 19 October");
    assert_eq!(day_label((2025, 10, 17), today), "Friday, 17 October 2025");
    let new_year = stamp(2027, 1, 1, 0, 0, 1);
    assert_eq!(day_label((2026, 12, 31), new_year), "Yesterday");
    assert_eq!(day_label((2026, 12, 30), new_year), "Wednesday, 30 December 2026");
}

#[test]
fn group_by_day_labels_totals_and_completeness() {
    let records = vec![
        record("VALORANT__2026-10-18__10-00-00.mp4", stamp(2026, 10, 18, 10, 0, 0), Some(60.0), 1),
        record("VALORANT__2026-10-18__09-00-00.mp4", stamp(2026, 10, 18, 9, 0, 0), Some(30.5), 1),
        record("Rocket-League__2026-10-17__23-00-00.mp4", stamp(2026, 10, 17, 23, 0, 0), None, 1),
        record("Rocket-League__2026-10-17__22-00-00.mp4", stamp(2026, 10, 17, 22, 0, 0), Some(10.0), 1),
        record("Apex-Legends__2025-10-17__22-00-00.mp4", stamp(2025, 10, 17, 22, 0, 0), Some(5.0), 1),
    ];
    let sorted = filter_sort(&records, &Filter::default(), SortOrder::Newest);
    let groups = group_by_day(&records, &sorted, stamp(2026, 10, 18, 12, 0, 0), true);
    let labels: Vec<_> = groups.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(labels, ["Today", "Yesterday", "Friday, 17 October 2025"]);
    assert_eq!(groups[0].items, vec![0, 1]);
    assert_eq!(groups[0].total_secs, 90.5);
    assert!(groups[0].durations_complete);
    assert_eq!(groups[1].total_secs, 10.0);
    assert!(!groups[1].durations_complete);
    assert_eq!(groups[2].date, (2025, 10, 17));
    let flat = group_by_day(&records, &sorted, stamp(2026, 10, 18, 12, 0, 0), false);
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].label, "All clips");
    assert_eq!(flat[0].items.len(), 5);
    assert_eq!(flat[0].total_secs, 105.5);
    assert!(!flat[0].durations_complete);
    assert!(group_by_day(&records, &[], stamp(2026, 10, 18, 12, 0, 0), true).is_empty());
}

#[test]
fn game_counts_merge_variants_most_first() {
    let records = vec![
        record("Rocket-League__2026-01-01__00-00-00.mp4", stamp(2026, 1, 1, 0, 0, 0), None, 1),
        record("Rocket-League®__2026-01-02__00-00-00.mp4", stamp(2026, 1, 2, 0, 0, 0), None, 1),
        record("VALORANT__2026-01-03__00-00-00.mp4", stamp(2026, 1, 3, 0, 0, 0), None, 1),
        record("Overwatch__2026-01-03__00-00-00.mp4", stamp(2026, 1, 3, 0, 0, 0), None, 1),
        record("Overwatch®-2__2026-01-03__00-00-00.mp4", stamp(2026, 1, 3, 0, 0, 0), None, 1),
    ];
    let counts = game_counts(&records);
    assert_eq!(counts[0], GameCount { key: "rocketleague".into(), name: "Rocket League".into(), count: 2 });
    let names: Vec<_> = counts[1..].iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names, ["Overwatch", "Overwatch 2", "VALORANT"], "ties by name; Overwatch 2 stays separate");
}

#[test]
fn filter_and_sort_orders() {
    let mut records = vec![
        record("VALORANT__2026-10-18__10-00-00.mp4", stamp(2026, 10, 18, 10, 0, 0), Some(20.0), 300),
        record("Rocket-League__2026-10-17__10-00-00.mp4", stamp(2026, 10, 17, 10, 0, 0), Some(120.0), 100),
        record("VALORANT__2026-10-16__10-00-00.mp4", stamp(2026, 10, 16, 10, 0, 0), None, 900),
    ];
    records[1].favourite = true;
    records[2].title = Some("Ace on Bind".into());
    let all = Filter::default();
    assert_eq!(filter_sort(&records, &all, SortOrder::Newest), vec![0, 1, 2]);
    assert_eq!(filter_sort(&records, &all, SortOrder::Oldest), vec![2, 1, 0]);
    assert_eq!(filter_sort(&records, &all, SortOrder::Longest), vec![1, 0, 2]);
    assert_eq!(filter_sort(&records, &all, SortOrder::Largest), vec![2, 0, 1]);
    let valorant = Filter { game_key: Some("valorant".into()), ..Filter::default() };
    assert_eq!(filter_sort(&records, &valorant, SortOrder::Newest), vec![0, 2]);
    let favourites = Filter { favourites_only: true, ..Filter::default() };
    assert_eq!(filter_sort(&records, &favourites, SortOrder::Newest), vec![1]);
    let titled = Filter { query: "  ACE ".into(), ..Filter::default() };
    assert_eq!(filter_sort(&records, &titled, SortOrder::Newest), vec![2]);
    let by_game = Filter { query: "rocket".into(), ..Filter::default() };
    assert_eq!(filter_sort(&records, &by_game, SortOrder::Newest), vec![1]);
    let by_file = Filter { query: "2026-10-16".into(), ..Filter::default() };
    assert_eq!(filter_sort(&records, &by_file, SortOrder::Newest), vec![2]);
    assert_eq!(records[2].display_title(), "Ace on Bind");
    assert_eq!(records[0].display_title(), "VALORANT");
}

#[test]
fn formatting() {
    assert_eq!(format_duration(119.146), "1:59");
    assert_eq!(format_duration(724.0), "12:04");
    assert_eq!(format_duration(3723.0), "1:02:03");
    assert_eq!(format_duration(-5.0), "0:00");
    assert_eq!(format_bytes(6_400_000), "6.4 MB");
    assert_eq!(format_bytes(364_000_000), "364 MB");
    assert_eq!(format_bytes(1_870_000_000), "1.87 GB");
}
